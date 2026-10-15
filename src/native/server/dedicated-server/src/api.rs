//! This module defines and implements the gRPC services for the dedicated server,
//! including Friends, Users, Misc, `UsersAdmin`, and `GamesAdmin` services.

use std::collections::HashMap;
use std::collections::HashSet;
use std::future::Future;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use quazal::rmc::types::Property;
use quazal::rmc::types::QList;
use quazal::rmc::types::StationURL;
use serde::Deserialize;
use serde::Serialize;
use server_api::release_contract::{AdmissionError, ReleaseIdentity};
use server_api::friends;
use server_api::friends::friends_server::Friends;
use server_api::friends::friends_server::FriendsServer;
use server_api::friends::Friend;
use server_api::games;
use server_api::games::games_admin_server::GamesAdmin;
use server_api::games::games_admin_server::GamesAdminServer;
use server_api::misc;
use server_api::misc::misc_server::Misc;
use server_api::misc::misc_server::MiscServer;
use server_api::users;
use server_api::users::users_admin_server::UsersAdmin;
use server_api::users::users_admin_server::UsersAdminServer;
use server_api::users::users_server::Users;
use server_api::users::users_server::UsersServer;
use server_api::users::User;
use slog::Logger;
use sodiumoxide::base64;
use sodiumoxide::crypto::auth::hmacsha256;
use sodiumoxide::crypto::hash::sha256;
use sodiumoxide::crypto::secretbox;
use sodiumoxide::randombytes;
use tokio::sync::RwLock;
use tonic::transport::Server;
use tonic::Request;
use tonic::Response;
use tonic::Status;

use crate::config::DebugConfig;
use crate::storage::AuthIdentity;
use crate::storage::GameSession;
use crate::storage::InvitableGameSession;
use crate::storage::LoginError;
use crate::storage::Storage;
use crate::storage::INVITE_KIND_LOBBY_PARTY;
use crate::storage::INVITE_KIND_LOBBY_RESTORE;
use crate::storage::INVITE_KIND_PARTY_FOLLOW;
use crate::storage::INVITE_KIND_PRIVATE_ROOM;

const MAX_UPLAY_GAME_SESSION_DATA: usize = 16 * 1024;
const RECOVERY_CODE_BYTES: usize = 15;
const RECOVERY_CODE_LENGTH: usize = 24;
const AUTH_TOKEN_VERSION: &str = "v1";
const DEFAULT_AUTH_TOKEN_TTL_SECONDS: i64 = 24 * 60 * 60;
const MIN_AUTH_TOKEN_TTL_SECONDS: i64 = 5 * 60;
const MAX_AUTH_TOKEN_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;
const MAX_AUTH_TOKEN_FUTURE_SKEW_SECONDS: i64 = 60;
const MAX_AUTH_TOKEN_LENGTH: usize = 4096;
// SCBL LAN: address family validation, never a provider or subnet identity.
#[derive(Clone, Copy)]
struct LanClientNetwork;
impl LanClientNetwork {
    fn from_env() -> Result<Self, std::io::Error> { Ok(Self) }
    fn contains_client(self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ip) => ip.octets()[0] != 0 && ip.octets()[0] < 224 && !ip.is_broadcast(),
            IpAddr::V6(_) => false,
        }
    }
}
#[cfg(test)]
mod lan_source_tests {
    use super::*;
    #[test] fn provider_neutral_ipv4() {
        let policy = LanClientNetwork;
        for s in ["192.168.5.9", "10.66.0.1", "26.1.2.3", "100.64.2.5", "127.0.0.1"] {
            assert!(policy.contains_client(s.parse().unwrap()), "{s}");
        }
        for s in ["0.0.0.0", "0.1.2.3", "224.0.0.1", "255.255.255.255", "::1"] {
            assert!(!policy.contains_client(s.parse().unwrap()), "{s}");
        }
    }
}

#[derive(Clone)]
struct AuthTokenConfig {
    release: ReleaseIdentity,
    key: hmacsha256::Key,
    ttl_seconds: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct AuthTokenClaims {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    release: Option<ReleaseIdentity>,
    user_id: u32,
    username: String,
    source_ip: String,
    issued_at: i64,
    expires_at: i64,
    auth_version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    game_run: Option<GameRunClaim>,
}


/// Signed identity of one native game run. Account/launcher tokens never carry it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct GameRunClaim {
    product: String,
    instance: String,
}

impl GameRunClaim {
    fn valid(&self) -> bool {
        use server_api::gameplay_contract::{ClientContract,valid_game_instance};
        matches!(ClientContract::parse(&self.product),Some(ClientContract::Game(_)))
            && valid_game_instance(&self.instance)
    }
}

/// Validate declarations before credentials, account writes, run mutations or tokens.
/// Only the caller-supplied verified startup policy decides admission; raw metadata
/// is never an alternative source of identity.
fn release_for_login(policy: &ReleaseIdentity, declared: Option<&users::PackageIdentity>) -> Result<ReleaseIdentity, Status> {
    let fail = |error: AdmissionError| Status::failed_precondition(format!("{}: Update/restore the complete SCBL LAN package (launcher, main DLL, gameplay DLL, server).", error.reason()));
    let value = declared.ok_or_else(|| fail(AdmissionError::Required))?;
    fn role(component: &Option<users::ReleaseComponent>) -> Option<(&str, &str)> { component.as_ref().map(|c| (c.release_id.as_str(), c.compat_id.as_str())) }
    let identity = ReleaseIdentity::from_components(value.schema, &value.release_id, &value.compat_id, &value.manifest_sha256,
        [role(&value.launcher), role(&value.main_dll), role(&value.gameplay_dll), role(&value.server)]).map_err(fail)?;
    policy.admit(Some(&identity)).map_err(fail)?;
    Ok(identity)
}

fn game_run_for_login(request: &Request<users::LoginRequest>) -> Result<Option<GameRunClaim>,Status> {
    use server_api::gameplay_contract::{ClientContract,GAME_INSTANCE_HEADER};
    let product=&request.get_ref().client_product;
    match ClientContract::parse(product) {
        Some(ClientContract::Account) => {
            if request.metadata().contains_key(GAME_INSTANCE_HEADER) {
                return Err(Status::invalid_argument("Account login must not declare a game instance"));
            }
            Ok(None)
        }
        Some(ClientContract::Game(_)) => {
            let instance=request.metadata().get(GAME_INSTANCE_HEADER)
                .and_then(|value|value.to_str().ok())
                .ok_or_else(||Status::invalid_argument("Native game instance is required"))?;
            let claim=GameRunClaim {product:product.clone(),instance:instance.into()};
            if !claim.valid() {return Err(Status::invalid_argument("Invalid native game instance"));}
            Ok(Some(claim))
        }
        None => Err(Status::failed_precondition("Use the matching SCBL LAN client and server")),
    }
}

async fn authenticated_game_mode<T>(storage:&Storage,request:&Request<T>)->Result<i64,Status> {
    let release=request.extensions().get::<ReleaseIdentity>().ok_or_else(||Status::unauthenticated("Missing signed package identity"))?;
    let run=request.extensions().get::<GameRunClaim>().ok_or_else(||Status::failed_precondition("Start the matching SCBL LAN game to use game sessions"))?;
    let mode=crate::gameplay_modes::mode_code(&run.product).ok_or_else(||Status::unauthenticated("Invalid native mode"))?;
    // Recheck after taking the handler's generation lock. A new game may have
    // logged in between the interceptor and this handler acquiring that lock.
    let source=request_source_ip(request,LanClientNetwork)?;
    if !storage.game_run_is_current(authenticated_user_id(request)?,&run.instance,mode,source,release).await.map_err(|_|Status::internal("Game identity lookup failed"))? {
        return Err(Status::unauthenticated("Game instance has been superseded"));
    }
    Ok(mode)
}

fn apply_verified_identity<T>(request: &mut Request<T>, claims: &AuthTokenClaims) {
    // Never route using a client-supplied metadata identity. Only the signed,
    // source-bound token verified by the interceptor can populate this extension.
    request.extensions_mut().remove::<ReleaseIdentity>();
    if let Some(release) = &claims.release { request.extensions_mut().insert(release.clone()); }
    request.extensions_mut().remove::<GameRunClaim>();
    if let Some(game_run)=&claims.game_run {request.extensions_mut().insert(game_run.clone());}
    request.metadata_mut().remove("authorization");
    request.metadata_mut().remove(server_api::gameplay_contract::GAME_INSTANCE_HEADER);
    request.metadata_mut().insert("user_id",claims.user_id.into());
}

#[cfg(test)]
mod game_mode_identity_tests {
    use super::*;
    use server_api::gameplay_contract::{ACCOUNT_PRODUCT,GAME_INSTANCE_HEADER,GameplayMode};
    const INSTANCE:&str="0123456789abcdef0123456789abcdef";
    fn request(product:&str,instance:Option<&str>)->Request<users::LoginRequest> {
        let mut r=Request::new(users::LoginRequest{package_identity:Some(test_package_identity()),client_product:product.into(),username:"fixture".into(),password:"fixture".into()});
        if let Some(v)=instance {r.metadata_mut().insert(GAME_INSTANCE_HEADER,v.parse().unwrap());}r
    }
    fn config()->AuthTokenConfig {AuthTokenConfig{release:test_release_identity(),key:hmacsha256::Key([0x31;32]),ttl_seconds:DEFAULT_AUTH_TOKEN_TTL_SECONDS}}
    fn claims(game_run:Option<GameRunClaim>)->AuthTokenClaims {
        AuthTokenClaims{release:Some(test_release_identity()),user_id:42,username:"fixture".into(),source_ip:"127.0.0.1".into(),issued_at:1700000000,expires_at:1700086400,auth_version:1,game_run}
    }
    #[test] fn login_contract_distinguishes_account_official_and_mod() {
        assert_eq!(game_run_for_login(&request(ACCOUNT_PRODUCT,None)).unwrap(),None);
        for mode in [GameplayMode::Official,GameplayMode::Mod] {
            assert_eq!(game_run_for_login(&request(mode.product(),Some(INSTANCE))).unwrap(),Some(GameRunClaim{product:mode.product().into(),instance:INSTANCE.into()}));
        }
    }
    #[test] fn runtime_requires_instance_and_account_rejects_it() {
        assert!(game_run_for_login(&request(ACCOUNT_PRODUCT,Some(INSTANCE))).is_err());
        for value in [None,Some(""),Some("abc"),Some("00000000000000000000000000000000")] {
            assert!(game_run_for_login(&request(GameplayMode::Mod.product(),value)).is_err());
        }
        assert!(game_run_for_login(&request("scbl-lan/2/game/unknown",Some(INSTANCE))).is_err());
    }
    #[test] fn runtime_claim_roundtrips_signed_token_without_changing_account_identity() {
        for mode in [GameplayMode::Official,GameplayMode::Mod] {
            let c=claims(game_run_for_login(&request(mode.product(),Some(INSTANCE))).unwrap());
            let token=issue_auth_token(&config(),&c).unwrap();
            assert_eq!(verify_auth_token(&config(),&token,"127.0.0.1".parse().unwrap(),1700000001).unwrap(),c);
        }
    }
    #[test] fn mode_tampering_without_resigning_is_rejected() {
        let c=claims(game_run_for_login(&request(GameplayMode::Mod.product(),Some(INSTANCE))).unwrap());
        let token=issue_auth_token(&config(),&c).unwrap();let parts:Vec<_>=token.split('.').collect();
        let changed=String::from_utf8(decode_base64url(parts[1]).unwrap()).unwrap().replace("/game/mod","/game/official");
        let tampered=format!("{}.{}.{}",parts[0],encode_base64url(changed.as_bytes()),parts[2]);
        assert_eq!(verify_auth_token(&config(),&tampered,"127.0.0.1".parse().unwrap(),1700000001),Err(AuthTokenError::Invalid));
    }
    #[test] fn signed_but_invalid_game_identity_is_rejected() {
        for game_run in [GameRunClaim{product:ACCOUNT_PRODUCT.into(),instance:INSTANCE.into()},GameRunClaim{product:GameplayMode::Official.product().into(),instance:"".into()}] {
            let token=issue_auth_token(&config(),&claims(Some(game_run))).unwrap();
            assert_eq!(verify_auth_token(&config(),&token,"127.0.0.1".parse().unwrap(),1700000001),Err(AuthTokenError::Invalid));
        }
    }
    #[test] fn legacy_account_token_cannot_gain_runtime_mode_through_metadata() {
        let mut r=Request::new(());r.metadata_mut().insert(GAME_INSTANCE_HEADER,INSTANCE.parse().unwrap());
        r.metadata_mut().insert("user_id",999u32.into());
        r.extensions_mut().insert(GameRunClaim{product:GameplayMode::Mod.product().into(),instance:INSTANCE.into()});
        apply_verified_identity(&mut r,&claims(None));
        assert!(r.extensions().get::<GameRunClaim>().is_none());
        assert!(!r.metadata().contains_key(GAME_INSTANCE_HEADER));assert_eq!(authenticated_user_id(&r).unwrap(),42);
    }
    #[test] fn mode_extension_comes_only_from_verified_claim() {
        let c=claims(game_run_for_login(&request(GameplayMode::Official.product(),Some(INSTANCE))).unwrap());
        let mut r=Request::new(());apply_verified_identity(&mut r,&c);
        assert_eq!(r.extensions().get::<GameRunClaim>(),c.game_run.as_ref());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthTokenError {
    Invalid,
    Expired,
    FutureIssued,
    WrongSource,
    Revoked,
    UnknownUser,
}

impl AuthTokenConfig {
    fn from_env(release: ReleaseIdentity) -> Result<Self, String> {
        // A fresh independent signing key for this server process. Restart requires login.
        let key = hmacsha256::gen_key();
        let ttl_seconds = match std::env::var("SCBL_LAN_AUTH_TOKEN_TTL_SECONDS") {
            Ok(value) => value
                .parse::<i64>()
                .ok()
                .filter(|ttl| (MIN_AUTH_TOKEN_TTL_SECONDS..=MAX_AUTH_TOKEN_TTL_SECONDS).contains(ttl))
                .ok_or_else(|| format!("SCBL_AUTH_TOKEN_TTL_SECONDS must be between {MIN_AUTH_TOKEN_TTL_SECONDS} and {MAX_AUTH_TOKEN_TTL_SECONDS}"))?,
            Err(std::env::VarError::NotPresent) => DEFAULT_AUTH_TOKEN_TTL_SECONDS,
            Err(std::env::VarError::NotUnicode(_)) => return Err(String::from("SCBL_AUTH_TOKEN_TTL_SECONDS is invalid")),
        };
        Ok(Self { key, ttl_seconds, release })
    }
}

#[cfg(test)]
fn decode_hex_key(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut output = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        output[index] = (high << 4) | low;
    }
    Some(output)
}

#[cfg(test)]
fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn unix_time_seconds() -> Result<i64, Status> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Status::internal("System clock is before the Unix epoch"))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| Status::internal("System clock is out of range"))
}

fn request_source_ip<T>(request: &Request<T>, overlay_network: LanClientNetwork) -> Result<IpAddr, Status> {
    let ip = request
        .remote_addr()
        .map(|address| address.ip())
        .ok_or_else(|| Status::unauthenticated("Missing client source address"))?;
    if !overlay_network.contains_client(ip) {
        return Err(Status::unauthenticated("Invalid client source address"));
    }
    Ok(ip)
}

fn encode_base64url(value: &[u8]) -> String {
    base64::encode(value, base64::Variant::UrlSafeNoPadding)
}

fn decode_base64url(value: &str) -> Result<Vec<u8>, AuthTokenError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')) {
        return Err(AuthTokenError::Invalid);
    }
    let decoded = base64::decode(value, base64::Variant::UrlSafeNoPadding).map_err(|()| AuthTokenError::Invalid)?;
    if encode_base64url(&decoded) != value {
        return Err(AuthTokenError::Invalid);
    }
    Ok(decoded)
}

fn issue_auth_token(config: &AuthTokenConfig, claims: &AuthTokenClaims) -> Result<String, Status> {
    let payload = serde_json::to_vec(claims).map_err(|_| Status::internal("Token payload encoding failed"))?;
    let payload = encode_base64url(&payload);
    let signing_input = format!("{AUTH_TOKEN_VERSION}.{payload}");
    let signature = hmacsha256::authenticate(signing_input.as_bytes(), &config.key);
    Ok(format!("{signing_input}.{}", encode_base64url(signature.as_ref())))
}

fn verify_auth_token(config: &AuthTokenConfig, token: &str, source_ip: IpAddr, now: i64) -> Result<AuthTokenClaims, AuthTokenError> {
    if token.len() > MAX_AUTH_TOKEN_LENGTH {
        return Err(AuthTokenError::Invalid);
    }
    let mut parts = token.split('.');
    let version = parts.next().ok_or(AuthTokenError::Invalid)?;
    let payload_segment = parts.next().ok_or(AuthTokenError::Invalid)?;
    let signature_segment = parts.next().ok_or(AuthTokenError::Invalid)?;
    if parts.next().is_some() || version != AUTH_TOKEN_VERSION {
        return Err(AuthTokenError::Invalid);
    }
    let signature = decode_base64url(signature_segment)?;
    let signature = hmacsha256::Tag::from_slice(&signature).ok_or(AuthTokenError::Invalid)?;
    let signing_input = format!("{version}.{payload_segment}");
    if !hmacsha256::verify(&signature, signing_input.as_bytes(), &config.key) {
        return Err(AuthTokenError::Invalid);
    }
    let payload = decode_base64url(payload_segment)?;
    let claims: AuthTokenClaims = serde_json::from_slice(&payload).map_err(|_| AuthTokenError::Invalid)?;
    if claims.user_id == 0 || claims.username.is_empty() || claims.username.len() > 64 || claims.auth_version < 1 {
        return Err(AuthTokenError::Invalid);
    }
    if config.release.admit(claims.release.as_ref()).is_err() { return Err(AuthTokenError::Invalid); }
    if claims.game_run.as_ref().is_some_and(|run| !run.valid()) {
        return Err(AuthTokenError::Invalid);
    }
    if claims.source_ip != source_ip.to_string() {
        return Err(AuthTokenError::WrongSource);
    }
    if claims.issued_at > now + MAX_AUTH_TOKEN_FUTURE_SKEW_SECONDS {
        return Err(AuthTokenError::FutureIssued);
    }
    if claims.expires_at <= now {
        return Err(AuthTokenError::Expired);
    }
    let lifetime = claims.expires_at.checked_sub(claims.issued_at).ok_or(AuthTokenError::Invalid)?;
    if lifetime < MIN_AUTH_TOKEN_TTL_SECONDS || lifetime > MAX_AUTH_TOKEN_TTL_SECONDS {
        return Err(AuthTokenError::Invalid);
    }
    Ok(claims)
}

fn validate_auth_identity(claims: &AuthTokenClaims, identity: Option<&AuthIdentity>) -> Result<(), AuthTokenError> {
    let identity = identity.ok_or(AuthTokenError::UnknownUser)?;
    if claims.user_id != identity.user_id || claims.username != identity.username || claims.auth_version != identity.auth_version {
        return Err(AuthTokenError::Revoked);
    }
    Ok(())
}

#[cfg(test)]
mod auth_token_tests {
    use std::net::Ipv4Addr;

    use super::*;

    const NOW: i64 = 1_700_000_000;

    fn config() -> AuthTokenConfig {
        AuthTokenConfig {
            release: test_release_identity(),
            key: hmacsha256::Key([0x11; 32]),
            ttl_seconds: DEFAULT_AUTH_TOKEN_TTL_SECONDS,
        }
    }

    fn claims() -> AuthTokenClaims {
        AuthTokenClaims {
            release: Some(test_release_identity()),
            user_id: 42,
            username: String::from("CanonicalPlayer"),
            source_ip: String::from("10.66.0.42"),
            issued_at: NOW,
            expires_at: NOW + DEFAULT_AUTH_TOKEN_TTL_SECONDS,
            auth_version: 3,
            game_run: None,
        }
    }

    fn source_ip() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 66, 0, 42))
    }

    #[test]
    fn legacy_fixed_token_is_rejected_and_current_signed_release_roundtrips() {
        let token = issue_auth_token(&config(), &claims()).unwrap();
        assert_eq!(verify_auth_token(&config(), "v1.eyJ1c2VyX2lkIjo0MiwidXNlcm5hbWUiOiJDYW5vbmljYWxQbGF5ZXIiLCJzb3VyY2VfaXAiOiIxMC42Ni4wLjQyIiwiaXNzdWVkX2F0IjoxNzAwMDAwMDAwLCJleHBpcmVzX2F0IjoxNzAwMDg2NDAwLCJhdXRoX3ZlcnNpb24iOjN9.5GGWk0JTcbhyPcXHWHH5msYvAv9RLkxjyPknzbGcaHQ", source_ip(), NOW), Err(AuthTokenError::Invalid));
        let verified = verify_auth_token(&config(), &token, source_ip(), NOW).unwrap();
        assert_eq!(verified, claims());
    }

    #[test]
    fn tampered_token_is_rejected() {
        let token = issue_auth_token(&config(), &claims()).unwrap();
        let mut bytes = token.into_bytes();
        let last = bytes.last_mut().unwrap();
        *last = if *last == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).unwrap();
        assert_eq!(verify_auth_token(&config(), &tampered, source_ip(), NOW), Err(AuthTokenError::Invalid));
    }

    #[test]
    fn oversized_token_is_rejected() {
        let oversized = "A".repeat(MAX_AUTH_TOKEN_LENGTH + 1);
        assert_eq!(verify_auth_token(&config(), &oversized, source_ip(), NOW), Err(AuthTokenError::Invalid));
    }

    #[test]
    fn expired_and_future_tokens_are_rejected() {
        let expired = issue_auth_token(&config(), &claims()).unwrap();
        assert_eq!(
            verify_auth_token(&config(), &expired, source_ip(), NOW + DEFAULT_AUTH_TOKEN_TTL_SECONDS),
            Err(AuthTokenError::Expired)
        );

        let mut future_claims = claims();
        future_claims.issued_at = NOW + MAX_AUTH_TOKEN_FUTURE_SKEW_SECONDS + 1;
        future_claims.expires_at = future_claims.issued_at + DEFAULT_AUTH_TOKEN_TTL_SECONDS;
        let future = issue_auth_token(&config(), &future_claims).unwrap();
        assert_eq!(verify_auth_token(&config(), &future, source_ip(), NOW), Err(AuthTokenError::FutureIssued));
    }

    #[test]
    fn wrong_source_ip_is_rejected() {
        let token = issue_auth_token(&config(), &claims()).unwrap();
        let wrong_ip = IpAddr::V4(Ipv4Addr::new(10, 66, 0, 43));
        assert_eq!(verify_auth_token(&config(), &token, wrong_ip, NOW), Err(AuthTokenError::WrongSource));
    }

    #[test]
    fn revoked_and_unknown_users_are_rejected() {
        let claims = claims();
        let revoked = AuthIdentity {
            user_id: claims.user_id,
            username: claims.username.clone(),
            account_id: String::from("account-42"),
            auth_version: claims.auth_version + 1,
        };
        assert_eq!(validate_auth_identity(&claims, Some(&revoked)), Err(AuthTokenError::Revoked));
        assert_eq!(validate_auth_identity(&claims, None), Err(AuthTokenError::UnknownUser));
    }

    #[test]
    fn control_auth_key_requires_exact_hex() {
        assert_eq!(decode_hex_key(&"11".repeat(32)), Some([0x11; 32]));
        assert_eq!(decode_hex_key("11"), None);
        assert_eq!(decode_hex_key(&"zz".repeat(32)), None);
    }
}

fn normalize_recovery_code(value: &str) -> String {
    value.chars().filter(|ch| ch.is_ascii_alphanumeric()).map(|ch| ch.to_ascii_uppercase()).collect()
}

fn hash_recovery_code(value: &str) -> String {
    let normalized = normalize_recovery_code(value);
    sodiumoxide::hex::encode(sha256::hash(normalized.as_bytes()).0)
}

fn generate_recovery_code() -> String {
    let compact = base32(&randombytes::randombytes(RECOVERY_CODE_BYTES));
    compact
        .as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).expect("base32 output is ASCII"))
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod recovery_code_tests {
    #[test]
    fn maintenance_hex_recovery_observation() {
        let cases=[("","e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),("0ABC-1DEF-2GHI-3JKL-4MNO-5PQR","8881af7569409b8d90a43455709336e7f839f2740f33fcffc99ab5af323668c9"),("0abc 1def 2ghi 3jkl 4mno 5pqr","8881af7569409b8d90a43455709336e7f839f2740f33fcffc99ab5af323668c9"),("xyz_19","adf0b7e0234f19c0a8cf5273f2f485d5dce294965c4df8379f2d2a3312cb9447"),(" !!! ","e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),("Aé中b-09","f0e4e5371db2c2f77a361894a429cd765072b746489af8281e28607986f5bfef")];
        for (case,(input,expected)) in cases.iter().enumerate() {
            let actual=hash_recovery_code(input);assert_eq!(actual,*expected);
            println!("HEX_RECOVERY case={case} normalized_sha256_lower_exact=true");
        }
    }

    use super::*;

    #[test]
    fn generated_code_has_120_bits_in_readable_groups() {
        let code = generate_recovery_code();
        assert_eq!(code.len(), 29);
        assert_eq!(code.matches('-').count(), 5);
        assert_eq!(normalize_recovery_code(&code).len(), RECOVERY_CODE_LENGTH);
    }

    #[test]
    fn hashing_ignores_case_and_separators() {
        assert_eq!(hash_recovery_code("0ABC-1DEF-2GHI-3JKL-4MNO-5PQR"), hash_recovery_code("0abc 1def 2ghi 3jkl 4mno 5pqr"));
    }
}

fn invite_requires_uplay_game_session(kind: i32) -> bool {
    matches!(kind, INVITE_KIND_PRIVATE_ROOM | INVITE_KIND_PARTY_FOLLOW)
}

fn invite_forces_join(kind: i32) -> bool {
    matches!(kind, INVITE_KIND_PARTY_FOLLOW | INVITE_KIND_LOBBY_RESTORE)
}

fn validate_uplay_game_session(data: &[u8]) -> bool {
    !data.is_empty() && data.len() <= MAX_UPLAY_GAME_SESSION_DATA
}

fn invite_projects_exact_uplay_target(kind: i32) -> bool {
    kind == INVITE_KIND_PRIVATE_ROOM
}

fn invite_uplay_payload_owner(kind: i32, session_host_id: Option<u32>) -> Option<u32> {
    invite_requires_uplay_game_session(kind).then_some(session_host_id).flatten()
}

/// Return a response copy of the inviter-published Uplay `GameSession` for the
/// private invitation transports that consume it. Lobby invitations always
/// use Friends accepted -> participant search -> NAT -> `JoinSession` and must
/// not attach or gate acceptance on a Uplay `GameSession` payload.
fn project_invite_uplay_game_session(kind: i32, invite_session_id: u32, published: Option<misc::UplayGameSession>) -> Option<misc::UplayGameSession> {
    if !invite_requires_uplay_game_session(kind) {
        return None;
    }
    let mut session = published?;
    if invite_projects_exact_uplay_target(kind) {
        session.id = u64::from(invite_session_id);
    }
    Some(session)
}

fn project_party_roster(roster: Option<(u32, Vec<crate::storage::User>)>) -> Option<misc::PartyRoster> {
    let (leader_id, members) = roster?;
    let leader = members.iter().find(|member| member.id == leader_id)?;
    Some(misc::PartyRoster {
        leader: Some(User {
            id: leader.ubi_id.clone(),
            username: leader.username.clone(),
            ips: Vec::new(),
        }),
        members: members
            .into_iter()
            .map(|member| User {
                id: member.ubi_id,
                username: member.username,
                ips: Vec::new(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod private_game_direct_route_tests {
    use super::*;

    fn published() -> misc::UplayGameSession {
        misc::UplayGameSession {
            id: 465,
            data: vec![1, 2, 3, 4],
            flags: 7,
            invite_only: true,
        }
    }

    #[test]
    fn direct_private_projects_only_top_level_target_id() {
        let projected = project_invite_uplay_game_session(INVITE_KIND_PRIVATE_ROOM, 466, Some(published())).unwrap();
        assert_eq!(projected.id, 466);
        assert_eq!(projected.data, vec![1, 2, 3, 4]);
        assert_eq!(projected.flags, 7);
        assert!(projected.invite_only);
    }

    #[test]
    fn party_follow_keeps_host_published_target_id() {
        let projected = project_invite_uplay_game_session(INVITE_KIND_PARTY_FOLLOW, 466, Some(published())).unwrap();
        assert_eq!(projected.id, 465);
        assert_eq!(projected.data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn lobby_party_never_attaches_uplay_game_session_payload() {
        let mut host_publication = published();
        host_publication.data = vec![0x5a; 496];
        assert!(project_invite_uplay_game_session(INVITE_KIND_LOBBY_PARTY, 460, Some(host_publication)).is_none());
    }

    #[test]
    fn missing_publication_stays_missing() {
        assert!(project_invite_uplay_game_session(INVITE_KIND_PRIVATE_ROOM, 466, None).is_none());
    }
}

fn session_invite_kind(session: &GameSession) -> Option<i32> {
    let attributes = session.attributes.parse::<QList<Property>>().ok()?;
    attributes.0.iter().find_map(|property| {
        if property.id != 113 {
            return None;
        }
        match property.value {
            0 => Some(INVITE_KIND_PRIVATE_ROOM),
            1 => Some(INVITE_KIND_LOBBY_PARTY),
            _ => None,
        }
    })
}

fn session_attributes_are_public_game(session_attributes: &str) -> bool {
    let Ok(attributes) = session_attributes.parse::<QList<Property>>() else {
        return false;
    };
    let values = attributes.0.iter().map(|property| (property.id, property.value)).collect::<HashMap<_, _>>();
    values.get(&113) == Some(&0) && values.get(&3).is_some_and(|slots| *slots > 0)
}

fn session_is_public_game(session: &GameSession) -> bool {
    session_attributes_are_public_game(&session.attributes)
}

#[derive(Debug)]
struct InvitableSessionSelection {
    session: GameSession,
    kind: i32,
}

#[derive(Default)]
struct UplayGameSessionRegistry {
    current: HashMap<u32, misc::UplayGameSession>,
    last_public: HashMap<u32, misc::UplayGameSession>,
}

impl UplayGameSessionRegistry {
    fn current(&self, user_id: u32) -> Option<&misc::UplayGameSession> {
        self.current.get(&user_id)
    }

    /// Retail clears the newly elected host's current Uplay publication while
    /// completing native match migration. Its most recent public descriptor is
    /// still the validated transport payload for that same running client and
    /// can be projected onto the migrated match Session just as the original
    /// host's lobby descriptor was projected before migration.
    fn public_for_active_match(&self, user_id: u32) -> Option<&misc::UplayGameSession> {
        match self.current.get(&user_id) {
            Some(publication) if !publication.invite_only => Some(publication),
            Some(_) => None,
            None => self.last_public.get(&user_id).filter(|publication| !publication.invite_only),
        }
    }

    fn publish(&mut self, user_id: u32, session: misc::UplayGameSession) {
        if !session.invite_only {
            self.last_public.insert(user_id, session.clone());
        }
        self.current.insert(user_id, session);
    }

    fn clear_current(&mut self, user_id: u32) -> bool {
        self.current.remove(&user_id).is_some()
    }
}

fn invite_host_publication<'a>(registry: &'a UplayGameSessionRegistry, kind: i32, session_attributes: &str, host_id: u32) -> Option<&'a misc::UplayGameSession> {
    if kind == INVITE_KIND_PRIVATE_ROOM && session_attributes_are_public_game(session_attributes) {
        registry.public_for_active_match(host_id)
    } else {
        registry.current(host_id)
    }
}

fn select_invitable_session(
    sender_id: u32,
    sessions: impl IntoIterator<Item = InvitableGameSession>,
    uplay_game_sessions: &UplayGameSessionRegistry,
) -> Option<InvitableSessionSelection> {
    let mut private_room = None;
    let mut public_match = None;
    let mut party_lobby = None;

    for candidate in sessions {
        let session = candidate.session;
        let Some(kind) = session_invite_kind(&session) else {
            continue;
        };
        match kind {
            INVITE_KIND_PRIVATE_ROOM => {
                let host_publication = uplay_game_sessions.current(session.creator_id);
                let public_match_publication = uplay_game_sessions.public_for_active_match(session.creator_id);
                let sender_publication = uplay_game_sessions.current(sender_id);
                let current_private_room = !session_is_public_game(&session)
                    && host_publication.is_some_and(|published| published.invite_only)
                    && (candidate.direct_member || sender_publication.is_some_and(|published| published.invite_only));
                let current_public_match = session_is_public_game(&session)
                    && public_match_publication
                        .is_some_and(|published| candidate.direct_member || sender_publication.is_some_and(|sender| !sender.invite_only && sender.id == published.id));
                if current_private_room && private_room.is_none() {
                    private_room = Some(session);
                } else if current_public_match && public_match.is_none() {
                    public_match = Some(session);
                }
            }
            INVITE_KIND_LOBBY_PARTY => {
                // This is an explicit invitation, not a direct-join discovery request.
                // Invite-only lobbies remain private; the exact live publication is required.
                if uplay_game_sessions
                    .current(sender_id)
                    .is_some_and(|published| published.id == u64::from(session.session_id))
                    && party_lobby.is_none()
                {
                    party_lobby = Some(session);
                }
            }
            _ => {}
        }
    }

    if let Some(session) = private_room {
        return Some(InvitableSessionSelection {
            session,
            kind: INVITE_KIND_PRIVATE_ROOM,
        });
    }
    if let Some(match_session) = public_match {
        return Some(InvitableSessionSelection {
            session: match_session,
            kind: INVITE_KIND_PRIVATE_ROOM,
        });
    }
    party_lobby.map(|session| InvitableSessionSelection {
        session,
        kind: INVITE_KIND_LOBBY_PARTY,
    })
}

fn invite_publication_is_current(kind: i32, session_id: u32, session_attributes: &str, publication: Option<&misc::UplayGameSession>) -> bool {
    match kind {
        INVITE_KIND_PRIVATE_ROOM => publication.is_some_and(|published| {
            if session_attributes_are_public_game(session_attributes) {
                !published.invite_only
            } else {
                published.invite_only
            }
        }),
        INVITE_KIND_LOBBY_PARTY if session_attributes_are_public_game(session_attributes) => true,
        // Delivery and acceptance must use the same rule as explicit-invite selection.
        // Do not mutate invite_only or fall back to a previously published session.
        INVITE_KIND_LOBBY_PARTY => publication.is_some_and(|published| published.id == u64::from(session_id)),
        _ => true,
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvitePublicationReadiness {
    Ready,
    Pending,
    Invalid,
}

/// Publication readiness is not invitation authorization. A Clear/Set gap is
/// retryable while the exact database target and its authorization remain live.
/// Party-follow intentionally retains the host-published lobby ID and kind=3.
fn invite_publication_readiness(
    kind: i32,
    session_id: u32,
    target: Option<(u32, &str)>,
    registry: &UplayGameSessionRegistry,
) -> InvitePublicationReadiness {
    if !matches!(kind, INVITE_KIND_PRIVATE_ROOM | INVITE_KIND_LOBBY_PARTY | INVITE_KIND_PARTY_FOLLOW) {
        return InvitePublicationReadiness::Ready;
    }
    let Some((host_id, attributes)) = target else {
        return InvitePublicationReadiness::Invalid;
    };
    let publication = invite_host_publication(registry, kind, attributes, host_id);
    if !invite_publication_is_current(kind, session_id, attributes, publication) {
        // Publication absence or a mismatching transitional descriptor does
        // not prove a still-live authorized database target is destroyed.
        return InvitePublicationReadiness::Pending;
    }
    if invite_requires_uplay_game_session(kind) && publication.is_none() {
        return InvitePublicationReadiness::Pending;
    }
    InvitePublicationReadiness::Ready
}

#[cfg(test)]
mod invite_target_selection_tests {
    use super::*;

    fn candidate(session: GameSession, direct_member: bool) -> InvitableGameSession {
        InvitableGameSession { session, direct_member }
    }

    fn session(session_id: u32, creator_id: u32, room: bool) -> InvitableGameSession {
        candidate(
            GameSession {
                session_type: 1,
                session_id,
                creator_id,
                attributes: if room {
                    String::from("113 => 0;3 => 0;4 => 8")
                } else {
                    String::from("113 => 1;3 => 8;4 => 0")
                },
                participants: Vec::new(),
            },
            false,
        )
    }

    fn public_game(session_id: u32, creator_id: u32) -> InvitableGameSession {
        candidate(
            GameSession {
                session_type: 1,
                session_id,
                creator_id,
                attributes: String::from("113 => 0;3 => 12;4 => 0;103 => 4008726265"),
                participants: Vec::new(),
            },
            false,
        )
    }

    fn publication(id: u64, invite_only: bool) -> misc::UplayGameSession {
        misc::UplayGameSession {
            id,
            data: vec![0x5a; 496],
            flags: 0,
            invite_only,
        }
    }

    fn registry(current: HashMap<u32, misc::UplayGameSession>) -> UplayGameSessionRegistry {
        UplayGameSessionRegistry {
            current,
            last_public: HashMap::new(),
        }
    }

    #[test]
    fn clearing_current_publication_preserves_only_the_last_public_transport() {
        let mut publications = UplayGameSessionRegistry::default();
        publications.publish(1005, publication(1659, false));
        assert!(publications.clear_current(1005));
        assert_eq!(publications.public_for_active_match(1005).map(|session| session.id), Some(1659));

        publications.publish(1005, publication(1660, true));
        assert!(publications.public_for_active_match(1005).is_none());
    }

    #[test]
    fn current_private_publication_keeps_private_room_authoritative() {
        let sessions = vec![session(1330, 1003, true), session(1327, 1005, false)];
        let publications = registry(HashMap::from([(1003, publication(1327, true))]));

        let selected = select_invitable_session(1003, sessions, &publications).unwrap();
        assert_eq!(selected.session.session_id, 1330);
        assert_eq!(selected.kind, INVITE_KIND_PRIVATE_ROOM);
    }

    #[test]
    fn direct_private_room_member_uses_the_hosts_publication() {
        let sessions = vec![candidate(session(1472, 1003, true).session, true)];
        let publications = registry(HashMap::from([(1003, publication(1472, true))]));

        let selected = select_invitable_session(1021, sessions, &publications).unwrap();
        assert_eq!(selected.session.session_id, 1472);
        assert_eq!(selected.kind, INVITE_KIND_PRIVATE_ROOM);
    }

    #[test]
    fn non_member_without_a_sender_publication_cannot_use_the_hosts_room() {
        let sessions = vec![session(1472, 1003, true)];
        let publications = registry(HashMap::from([(1003, publication(1472, true))]));

        assert!(select_invitable_session(1005, sessions, &publications).is_none());
    }

    #[test]
    fn returned_lobby_skips_stale_private_room_and_uses_shared_party() {
        let sessions = vec![session(1330, 1003, true), session(1327, 1005, false), session(1331, 1003, false)];
        let publications = registry(HashMap::from([(1003, publication(1327, false)), (1005, publication(1327, false))]));

        let selected = select_invitable_session(1003, sessions, &publications).unwrap();
        assert_eq!(selected.session.session_id, 1327);
        assert_eq!(selected.kind, INVITE_KIND_LOBBY_PARTY);
    }

    #[test]
    fn active_public_match_invitation_targets_the_joinable_match_directly() {
        let sessions = vec![public_game(1467, 1005), session(1464, 1005, false)];
        let publications = registry(HashMap::from([(1005, publication(1464, false))]));

        let selected = select_invitable_session(1005, sessions, &publications).unwrap();
        assert_eq!(selected.session.session_id, 1467);
        assert_eq!(selected.kind, INVITE_KIND_PRIVATE_ROOM);
    }

    #[test]
    fn active_public_match_accepts_another_current_party_member_as_inviter() {
        let sessions = vec![public_game(1467, 1005), session(1464, 1005, false)];
        let publications = registry(HashMap::from([(1005, publication(1464, false)), (1021, publication(1464, false))]));

        let selected = select_invitable_session(1021, sessions, &publications).unwrap();
        assert_eq!(selected.session.session_id, 1467);
        assert_eq!(selected.kind, INVITE_KIND_PRIVATE_ROOM);
    }

    #[test]
    fn active_public_match_without_a_current_party_lobby_keeps_direct_fallback() {
        let sessions = vec![public_game(1467, 1005)];
        let publications = registry(HashMap::from([(1005, publication(1464, false))]));

        let selected = select_invitable_session(1005, sessions, &publications).unwrap();
        assert_eq!(selected.session.session_id, 1467);
        assert_eq!(selected.kind, INVITE_KIND_PRIVATE_ROOM);
    }

    #[test]
    fn migrated_public_match_reuses_the_new_hosts_last_validated_public_transport() {
        let sessions = vec![candidate(public_game(1661, 1005).session, true)];
        let publications = UplayGameSessionRegistry {
            current: HashMap::new(),
            last_public: HashMap::from([(1005, publication(1659, false))]),
        };

        let selected = select_invitable_session(1005, sessions, &publications).unwrap();
        assert_eq!(selected.session.session_id, 1661);
        assert_eq!(selected.kind, INVITE_KIND_PRIVATE_ROOM);
        let projected = project_invite_uplay_game_session(
            selected.kind,
            selected.session.session_id,
            invite_host_publication(&publications, selected.kind, &selected.session.attributes, selected.session.creator_id).cloned(),
        )
        .unwrap();
        assert_eq!(projected.id, 1661);
        assert_eq!(projected.data.len(), 496);
    }

    #[test]
    fn migrated_public_match_without_a_validated_transport_fails_closed() {
        let sessions = vec![candidate(public_game(1661, 1005).session, true)];

        assert!(select_invitable_session(1005, sessions, &UplayGameSessionRegistry::default()).is_none());
    }

    #[test]
    fn migrated_public_match_fallback_rejects_a_non_member() {
        let sessions = vec![public_game(1661, 1005)];
        let publications = UplayGameSessionRegistry {
            current: HashMap::new(),
            last_public: HashMap::from([(1005, publication(1659, false))]),
        };

        assert!(select_invitable_session(1021, sessions, &publications).is_none());
    }

    #[test]
    fn unrelated_publication_cannot_route_into_an_active_public_match() {
        let sessions = vec![public_game(1467, 1005), session(1469, 1021, false)];
        let publications = registry(HashMap::from([(1005, publication(1464, false)), (1021, publication(1469, false))]));

        let selected = select_invitable_session(1021, sessions, &publications).unwrap();
        assert_eq!(selected.session.session_id, 1469);
        assert_eq!(selected.kind, INVITE_KIND_LOBBY_PARTY);
    }

    #[test]
    fn missing_sender_publication_fails_closed() {
        let sessions = vec![session(1330, 1003, true), session(1327, 1005, false)];

        assert!(select_invitable_session(1003, sessions, &UplayGameSessionRegistry::default()).is_none());
    }

    #[test]
    fn current_lobby_publication_outranks_a_preserved_solo_fallback() {
        let sessions = vec![session(1347, 1021, false), session(1335, 1005, false)];
        let publications = registry(HashMap::from([(1021, publication(1335, false)), (1005, publication(1335, false))]));

        let selected = select_invitable_session(1021, sessions, &publications).unwrap();
        assert_eq!(selected.session.session_id, 1335);
        assert_eq!(selected.kind, INVITE_KIND_LOBBY_PARTY);
    }

    #[test]
    fn stale_solo_lobby_is_not_a_fallback_when_current_lobby_is_absent() {
        let sessions = vec![session(1347, 1021, false)];
        let publications = registry(HashMap::from([(1021, publication(1335, false))]));

        assert!(select_invitable_session(1021, sessions, &publications).is_none());
    }

    #[test]
    fn private_delivery_requires_current_invite_only_publication() {
        assert!(invite_publication_is_current(
            INVITE_KIND_PRIVATE_ROOM,
            1330,
            "113 => 0;3 => 0;4 => 8",
            Some(&publication(1327, true))
        ));
        assert!(!invite_publication_is_current(
            INVITE_KIND_PRIVATE_ROOM,
            1330,
            "113 => 0;3 => 0;4 => 8",
            Some(&publication(1327, false))
        ));
        assert!(!invite_publication_is_current(INVITE_KIND_PRIVATE_ROOM, 1330, "113 => 0;3 => 0;4 => 8", None));
        assert!(invite_publication_is_current(
            INVITE_KIND_PRIVATE_ROOM,
            1330,
            "113 => 0;3 => 12;4 => 0",
            Some(&publication(1327, false))
        ));
        assert!(!invite_publication_is_current(
            INVITE_KIND_PRIVATE_ROOM,
            1330,
            "113 => 0;3 => 12;4 => 0",
            Some(&publication(1327, true))
        ));
    }

    #[test]
    fn lobby_delivery_requires_the_hosts_exact_current_publication() {
        assert!(invite_publication_is_current(
            INVITE_KIND_LOBBY_PARTY,
            1335,
            "113 => 1;3 => 8;4 => 0",
            Some(&publication(1335, false))
        ));
        assert!(!invite_publication_is_current(
            INVITE_KIND_LOBBY_PARTY,
            1347,
            "113 => 1;3 => 8;4 => 0",
            Some(&publication(1335, false))
        ));
        assert!(invite_publication_is_current(
            INVITE_KIND_LOBBY_PARTY,
            1335,
            "113 => 1;3 => 8;4 => 0",
            Some(&publication(1335, true))
        ));
        assert!(!invite_publication_is_current(INVITE_KIND_LOBBY_PARTY, 1335, "113 => 1;3 => 8;4 => 0", None));
    }
}

fn authenticated_user_id<T>(request: &Request<T>) -> Result<u32, Status> {
    request
        .metadata()
        .get("user_id")
        .ok_or_else(|| Status::unauthenticated("Missing authenticated user"))?
        .to_str()
        .map_err(|_| Status::unauthenticated("Invalid authenticated user"))?
        .parse()
        .map_err(|_| Status::unauthenticated("Invalid authenticated user"))
}

#[cfg(test)]
mod authenticated_user_tests {
    use super::*;

    #[test]
    fn accepts_valid_interceptor_metadata() {
        let mut request = Request::new(());
        request.metadata_mut().insert("user_id", 42_u32.into());
        assert_eq!(authenticated_user_id(&request).unwrap(), 42);
    }

    #[test]
    fn rejects_missing_or_malformed_metadata_without_panicking() {
        let missing = authenticated_user_id(&Request::new(())).unwrap_err();
        assert_eq!(missing.code(), tonic::Code::Unauthenticated);

        let mut malformed = Request::new(());
        malformed.metadata_mut().insert("user_id", "not-a-number".parse().unwrap());
        let malformed = authenticated_user_id(&malformed).unwrap_err();
        assert_eq!(malformed.code(), tonic::Code::Unauthenticated);
    }
}

/// Implements the `Friends` gRPC service.
pub struct MyFriends {
    logger: Logger,
    storage: Arc<Storage>,
    debug_config: Arc<DebugConfig>,
    uplay_game_sessions: Arc<RwLock<UplayGameSessionRegistry>>,
}

#[tonic::async_trait]
impl Friends for MyFriends {
    /// Handles friend invitation requests.
    ///
    /// Extracts sender and receiver IDs, adds the invite to storage, and returns a response.
    async fn invite(&self, request: Request<friends::InviteRequest>) -> Result<Response<friends::InviteResponse>, Status> {
        let _generation=self.storage.mode_gate.read().await;
        let game_mode=authenticated_game_mode(&self.storage,&request).await?;
        let sender = authenticated_user_id(&request)?;
        debug!(self.logger, "Invite request: {:?} from {}", request, sender);
        let receiver = request.into_inner().id;

        let Some(receiver_id) = self
            .storage
            .find_user_id_by_ubi_id_async(&receiver)
            .await
            .map_err(|e| Status::internal(format!("Couldn't add invite: {e:?}")))?
        else {
            return Err(Status::not_found("User not found"));
        };
        if receiver_id == sender {
            return Err(Status::invalid_argument("Cannot invite yourself"));
        }

        let invitable_sessions = self
            .storage
            .find_invitable_session_candidates_async(sender)
            .await
            .map_err(|e| Status::internal(format!("Couldn't resolve invite target: {e:?}")))?;
        // Storage deliberately keeps a private room first during the short
        // lobby-to-room participant-capture window. It may also retain that
        // room while native host migration settles. Only the host's current
        // invite-only Uplay publication proves the room is still the active
        // invitation target; after everyone returns to a lobby, skip the stale
        // room and use the remaining Party lobby instead.
        let selected = {
            let publications = self.uplay_game_sessions.read().await;
            select_invitable_session(sender, invitable_sessions, &publications)
        };
        let Some(selected) = selected else {
            return Err(Status::failed_precondition("No active lobby or private room to invite into"));
        };
        let InvitableSessionSelection { session, kind } = selected;
        if self.storage.room_mode(session.session_type,session.session_id).await.map_err(|_|Status::internal("Cannot read room mode"))?!=Some(game_mode)
            || self.storage.current_game_mode(receiver_id).await.map_err(|_|Status::internal("Cannot read receiver mode"))?!=Some(game_mode) {
            return Err(Status::failed_precondition("Gameplay mode mismatch or peer game identity unavailable"));
        }
        if self
            .storage
            .invite_target_already_contains_async(receiver_id, kind, session.session_id)
            .await
            .map_err(|e| Status::internal(format!("Couldn't inspect invite target: {e:?}")))?
        {
            return Err(Status::already_exists(if kind == INVITE_KIND_PRIVATE_ROOM {
                "Already in the same room"
            } else {
                "Already in the same party"
            }));
        }

        let Some(invite_id) = self
            .storage
            .add_invite_async(sender, receiver_id, kind, session.session_type, session.session_id)
            .await
            .map_err(|e| Status::internal(format!("Couldn't add invite: {e:?}")))?
        else {
            return Err(Status::failed_precondition("Invite target is no longer joinable"));
        };
        info!(
            self.logger,
            "InviteBound";
            "invite_id" => invite_id,
            "kind" => kind,
            "sender_id" => sender,
            "receiver_id" => receiver_id,
            "session_type" => session.session_type,
            "session_id" => session.session_id,
        );
        Ok(Response::new(friends::InviteResponse {}))
    }

    /// Handles requests to list friends.
    ///
    /// Retrieves all users from storage and marks them as online based on debug configuration.
    async fn list(&self, request: Request<friends::ListRequest>) -> Result<Response<friends::ListResponse>, Status> {
        let user_id = authenticated_user_id(&request)?;
        debug!(self.logger, "Friendlist request: {:?} from {}", request, user_id);
        let users = self.storage.list_game_ready_users_async().await.map_err(|e| Status::internal(format!("{e}")))?;
        let friends = users
            .into_iter()
            .filter(|user| user.id != user_id)
            .map(|u| Friend {
                id: u.ubi_id,
                username: u.username,
                is_online: u.is_online || self.debug_config.mark_all_as_online,
            })
            .collect();
        let resp = friends::ListResponse { friends };
        Ok(Response::new(resp))
    }
}

/// Authenticates a gRPC request using the same bounded, source-bound token
/// accepted by the sidecar control plane.
async fn check_token<T>(
    logger: &Logger,
    config: &AuthTokenConfig,
    storage: &Arc<Storage>,
    overlay_network: LanClientNetwork,
    mut req: Request<T>,
) -> Result<Request<T>, Status> {
    let source_ip = request_source_ip(&req, overlay_network)?;
    let token = req.metadata().get("authorization").ok_or(Status::unauthenticated("Missing authorization"))?;
    let token = token.to_str().map_err(|_| Status::unauthenticated("Invalid token"))?;
    let claims = verify_auth_token(config, token, source_ip, unix_time_seconds()?).map_err(|_| Status::unauthenticated("Invalid token"))?;
    let identity = storage
        .find_auth_identity_by_user_id_async(claims.user_id)
        .await
        .map_err(|_| Status::unauthenticated("Invalid token"))?;
    validate_auth_identity(&claims, identity.as_ref()).map_err(|_| Status::unauthenticated("Invalid token"))?;
    debug!(logger, "Valid token for user {}", claims.user_id);
    if let Some(game_run)=&claims.game_run {
        let _generation=storage.mode_gate.read().await;
        let mode=crate::gameplay_modes::mode_code(&game_run.product).ok_or_else(||Status::unauthenticated("Invalid game mode"))?;
        if !storage.touch_game_run_with_release(claims.user_id,&game_run.instance,mode,source_ip,claims.release.as_ref().ok_or_else(||Status::unauthenticated("Missing signed package identity"))?).await.map_err(|_|Status::internal("Game heartbeat failed"))? {
            return Err(Status::unauthenticated("Game instance has been superseded; restart this game"));
        }
        storage.touch_api_presence_async(claims.user_id).await.map_err(|error|Status::internal(format!("Presence heartbeat failed: {error}")))?;
    }
    apply_verified_identity(&mut req, &claims);

    Ok(req)
}

/// Implements the `Users` gRPC service.
pub struct MyUsers {
    logger: Logger,
    token_config: AuthTokenConfig,
    storage: Arc<Storage>,
    overlay_network: LanClientNetwork,
}

// LAN users choose their own password. Only nonempty input and transport capacity
// are required; do not trim whitespace or impose strength/character-class rules.
fn valid_lan_password(password: &str) -> bool {
    !password.is_empty() && password.len() <= 4096 && !password.contains('\0')
}


// Names and account IDs later cross the native SDK as C strings. Reject NUL
// rather than truncating identities; retain the existing byte limits and policy.
fn valid_lan_registration_identity(username: &str, account: &str) -> bool {
    !username.trim().is_empty()
        && username.len() <= 64
        && !username.contains('\0')
        && !account.is_empty()
        && account.len() <= 64
        && !account.contains('\0')
}

#[tonic::async_trait]
impl Users for MyUsers {
    /// Handles user login requests.
    ///
    /// Authenticates the user against the storage and generates an authorization token upon successful login.
    async fn get_capabilities(&self, _request: Request<users::CapabilitiesRequest>) -> Result<Response<users::CapabilitiesResponse>, Status> {
        let ports = crate::lan_runtime::ports();
        Ok(Response::new(users::CapabilitiesResponse {
            product: "SCBL LAN".into(), protocol_version: 1, version: "0.1.0-dev".into(),
            ready: crate::lan_runtime::is_ready(), official_only: false,
            config_port: ports[0] as u32, content_port: ports[1] as u32,
            auth_port: ports[2] as u32, secure_port: ports[3] as u32,
        }))
    }
    async fn login(&self, request: Request<users::LoginRequest>) -> Result<Response<users::LoginResponse>, Status> {
        let source_ip = request_source_ip(&request, self.overlay_network)?;
        validate_lan_product(&request.get_ref().client_product)?;
        let release = release_for_login(&self.token_config.release, request.get_ref().package_identity.as_ref())?;
        let game_run = game_run_for_login(&request)?;
        let request = request.into_inner();
        let username = request.username;
        let password = request.password;

        let maybe_user = self
            .storage
            .authenticate_user_async(&username, &password)
            .await
            .map_err(|e| Status::internal(format!("Login error: {e:?}")))?;

        let identity = maybe_user.map_err(|err| match err {
            LoginError::InvalidPassword => Status::unauthenticated("Invalid login"),
            LoginError::NotFound => Status::not_found("Unknown user"),
        })?;

        #[cfg(test)]
        self.storage.run_login_checkpoint("publication");
        // Serialize credential-version confirmation, run creation, and token
        // publication with recovery. Never replace identity with a fresh version.
        let _generation = self.storage.mode_gate.write().await;
        if !self.storage.auth_identity_is_current_async(&identity).await
            .map_err(|error| Status::internal(format!("Login identity check failed: {error}")))? {
            return Err(Status::unauthenticated("Invalid login"));
        }
        let user_id = identity.user_id;
        if let Some(run)=&game_run {
            let mode=crate::gameplay_modes::mode_code(&run.product).ok_or_else(||Status::invalid_argument("Invalid native mode"))?;
            self.storage.begin_game_run_with_release(user_id,&run.instance,mode,source_ip,&release).await.map_err(|e|Status::failed_precondition(e.to_string()))?;
        }
        let issued_at = unix_time_seconds()?;
        let expires_at = issued_at
            .checked_add(self.token_config.ttl_seconds)
            .ok_or_else(|| Status::internal("Token expiry is out of range"))?;
        let token = issue_auth_token(
            &self.token_config,
            &AuthTokenClaims {
                user_id: identity.user_id,
                username: identity.username.clone(),
                source_ip: source_ip.to_string(),
                issued_at,
                expires_at,
                auth_version: identity.auth_version,
                release: Some(release),
                game_run,
            },
        )?;

        info!(self.logger, "Account login succeeded");
        Ok(Response::new(users::LoginResponse {
            error: String::new(),
            token,
            user: Some(User {
                id: identity.account_id,
                username: identity.username,
                ips: vec![source_ip.to_string()],
            }),
        }))
    }

    /// Handles user registration requests.
    ///
    /// Registers a new user in the storage, handling potential conflicts like duplicate usernames or Ubisoft IDs.
    async fn register(&self, request: Request<users::RegisterRequest>) -> Result<Response<users::RegisterResponse>, Status> {
        request_source_ip(&request, self.overlay_network)?;
        if request.get_ref().client_product != server_api::gameplay_contract::ACCOUNT_PRODUCT {
            return Err(Status::failed_precondition("Register with the SCBL LAN launcher"));
        }
        release_for_login(&self.token_config.release, request.get_ref().package_identity.as_ref())?;
        if !valid_lan_registration_identity(&request.get_ref().username, &request.get_ref().ubi_id)
            || !valid_lan_password(&request.get_ref().password) {
            return Err(Status::invalid_argument("username/account and a nonempty transport-compatible password required"));
        }
        let request = request.into_inner();
        let username = request.username;
        let password = request.password;
        let ubi_id = request.ubi_id;

        let recovery_code = generate_recovery_code();
        let recovery_code_hash = hash_recovery_code(&recovery_code);
        let error = if let Err(err) = self
            .storage
            .register_user_with_recovery_async(&username, &password, Some(&ubi_id), Some(&recovery_code_hash))
            .await
        {
            match err.downcast::<sqlx::Error>() {
                Ok(sqlx::Error::Database(db_err)) => {
                    if db_err.is_unique_violation() {
                        return Err(Status::already_exists(String::from("Username already taken or Ubisoft ID already registered")));
                    }
                    return Err(Status::internal(db_err.to_string()));
                }
                Ok(err) => return Err(Status::internal(err.to_string())),
                Err(err) => err.to_string(),
            }
        } else {
            String::new()
        };
        info!(self.logger, "Account registration completed");
        Ok(Response::new(users::RegisterResponse { error, user: None, recovery_code }))
    }

    /// Resets a password using a one-time recovery code. A successful reset
    /// rotates the code so replaying a leaked or previously used code fails.
    async fn reset_password(&self, request: Request<users::ResetPasswordRequest>) -> Result<Response<users::ResetPasswordResponse>, Status> {
        let request = request.into_inner();
        let username = request.username.trim();
        let normalized_code = normalize_recovery_code(&request.recovery_code);
        if username.is_empty() || username.len() > 64 || normalized_code.len() != RECOVERY_CODE_LENGTH || !valid_lan_password(&request.new_password) {
            return Err(Status::invalid_argument("Invalid recovery request"));
        }

        let supplied_hash = hash_recovery_code(&normalized_code);
        let replacement_code = generate_recovery_code();
        let replacement_hash = hash_recovery_code(&replacement_code);
        let reset = self
            .storage
            .reset_password_with_recovery_async(username, &supplied_hash, &request.new_password, &replacement_hash)
            .await
            .map_err(|error| Status::internal(format!("Password reset failed: {error}")))?;

        if !reset {
            // Keep account existence and recovery-code state private.
            return Err(Status::unauthenticated("Invalid recovery credentials"));
        }

        info!(self.logger, "Account password reset completed");
        Ok(Response::new(users::ResetPasswordResponse { recovery_code: replacement_code }))
    }
}

/// Implements the `Misc` gRPC service.
pub struct MyMisc {
    logger: Logger,
    storage: Arc<Storage>,
    debug_config: Arc<DebugConfig>,
    uplay_game_sessions: Arc<RwLock<UplayGameSessionRegistry>>,
}

#[tonic::async_trait]
impl Misc for MyMisc {
    /// Handles event requests, primarily for retrieving pending friend invites.
    async fn event(&self, request: Request<misc::EventRequest>) -> Result<Response<misc::EventResponse>, Status> {
        let _generation=self.storage.mode_gate.read().await;
        let game_mode=authenticated_game_mode(&self.storage,&request).await?;
        let user_id = authenticated_user_id(&request)?;
        if self.storage.lobby_restore_split_cleanup_due() {
            let storage = Arc::clone(&self.storage);
            let resolved = tokio::task::spawn_blocking(move || storage.expire_lobby_restore_split_transitions())
                .await
                .map_err(|error| Status::internal(format!("Couldn't join lobby-restore split worker: {error:?}")))?
                .map_err(|error| Status::internal(format!("Couldn't expire lobby-restore split transition: {error:?}")))?;
            if resolved > 0 {
                info!(self.logger, "LobbyRestoreDeferredRemovalCompleted"; "resolved_members" => resolved);
            }
        }
        if self.storage.native_host_migration_cleanup_due() {
            let storage = Arc::clone(&self.storage);
            let resolved = tokio::task::spawn_blocking(move || storage.expire_native_host_migrations())
                .await
                .map_err(|error| Status::internal(format!("Couldn't join host-migration fallback worker: {error:?}")))?
                .map_err(|error| Status::internal(format!("Couldn't expire host migration: {error:?}")))?;
            if resolved > 0 {
                info!(self.logger, "NativeHostMigrationFallbackCompleted"; "resolved_sessions" => resolved);
            }
        }
        if self.storage.pending_quick_match_member_leave_cleanup_due() {
            let storage = Arc::clone(&self.storage);
            let resolved = tokio::task::spawn_blocking(move || storage.expire_pending_quick_match_member_leaves())
                .await
                .map_err(|error| Status::internal(format!("Couldn't join quick-match member-leave worker: {error:?}")))?
                .map_err(|error| Status::internal(format!("Couldn't expire quick-match member leave: {error:?}")))?;
            if resolved > 0 {
                info!(self.logger, "QuickMatchMemberLeaveFallbackCompleted"; "resolved_members" => resolved);
            }
        }
        let roster=self.storage.find_party_roster_async(user_id).await.map_err(|error|Status::internal(format!("Couldn't resolve Party roster: {error:?}")))?;
        let mut party_compatible=true;
        if let Some((_,members))=&roster {
            for member in members {
                if self.storage.current_game_mode(member.id).await.map_err(|_|Status::internal("Cannot read party mode"))?!=Some(game_mode) {party_compatible=false;break;}
            }
        }
        let party=if party_compatible {project_party_roster(roster)} else {None};
        let Some(invite) = self.storage.take_invite_async(user_id).await.map_err(|e| {
            error!(self.logger, "Error getting latest invite for user: {e}");
            Status::internal(format!("{e:?}"))
        })?
        else {
            return Ok(Response::new(misc::EventResponse { invite: None, party }));
        };

        if !self.storage.invite_matches_mode(user_id,invite.id,game_mode).await.map_err(|_|Status::internal("Cannot read invitation mode"))? {
            self.storage.consume_invite_async(user_id,invite.id).await.map_err(|_|Status::internal("Cannot retire incompatible invitation"))?;
            return Ok(Response::new(misc::EventResponse {invite:None,party}));
        }
        let Some(sender) = self.storage.find_user_by_id_async(invite.sender).await.map_err(|e| {
            error!(self.logger, "Error getting ubi id for user: {e}");
            Status::internal(format!("{e:?}"))
        })?
        else {
            return Err(Status::not_found(""));
        };
        let session_target = self
            .storage
            .find_active_session_invite_target_async(invite.session_type, invite.session_id)
            .await
            .map_err(|error| Status::internal(format!("Couldn't resolve invite session host: {error:?}")))?;
        let session_host_id = session_target.as_ref().map(|(creator_id, _)| *creator_id);
        let session_attributes = session_target.as_ref().map_or("", |(_, attributes)| attributes.as_str());
        let (host_publication, readiness) = {
            let publications = self.uplay_game_sessions.read().await;
            let readiness = invite_publication_readiness(
                invite.kind,
                invite.session_id,
                session_target.as_ref().map(|(id, attributes)| (*id, attributes.as_str())),
                &publications,
            );
            let publication = session_host_id
                .and_then(|host_id| invite_host_publication(&publications, invite.kind, session_attributes, host_id))
                .cloned();
            (publication, readiness)
        };
        if readiness == InvitePublicationReadiness::Pending {
            info!(self.logger, "InvitePublicationPending"; "invite_id" => invite.id, "kind" => invite.kind, "stage" => "delivery");
            return Ok(Response::new(misc::EventResponse { invite: None, party }));
        }
        let published_game_session = if invite_uplay_payload_owner(invite.kind, session_host_id).is_some() {
            host_publication.clone()
        } else {
            None
        };
        if readiness == InvitePublicationReadiness::Invalid {
            self.storage
                .consume_invite_async(user_id, invite.id)
                .await
                .map_err(|error| Status::internal(format!("Couldn't retire stale invitation: {error:?}")))?;
            warn!(
                self.logger,
                "StaleInviteSuppressed";
                "invite_id" => invite.id,
                "sender_id" => invite.sender,
                "session_host_id" => session_host_id.unwrap_or(0),
                "receiver_id" => invite.receiver,
                "session_id" => invite.session_id,
            );
            return Ok(Response::new(misc::EventResponse { invite: None, party }));
        }
        let published_uplay_game_session_id = published_game_session.as_ref().map_or(0, |session| session.id);
        let game_session = project_invite_uplay_game_session(invite.kind, invite.session_id, published_game_session);
        if invite.kind == INVITE_KIND_PRIVATE_ROOM {
            if let Some(projected) = game_session.as_ref() {
                info!(
                    self.logger,
                    "PrivateInviteUplayTargetProjected";
                    "invite_id" => invite.id,
                    "sender_id" => invite.sender,
                    "session_host_id" => session_host_id.unwrap_or(0),
                    "receiver_id" => invite.receiver,
                    "session_type" => invite.session_type,
                    "private_session_id" => invite.session_id,
                    "published_uplay_game_session_id" => published_uplay_game_session_id,
                    "projected_uplay_game_session_id" => projected.id,
                    "payload_bytes" => projected.data.len(),
                    "payload_unchanged" => true,
                    "flags_unchanged" => true,
                    "invite_only_unchanged" => true,
                    "response_only" => true,
                );
            }
        }
        if invite_requires_uplay_game_session(invite.kind) && game_session.is_none() {
            warn!(
                self.logger,
                "InviteUplayGameSessionUnavailable";
                "invite_id" => invite.id,
                "kind" => invite.kind,
                "sender_id" => invite.sender,
                "session_host_id" => session_host_id.unwrap_or(0),
                "receiver_id" => invite.receiver,
                "session_id" => invite.session_id,
            );
        }
        info!(
            self.logger,
            "InviteEventDelivered";
            "invite_id" => invite.id,
            "delivery_count" => invite.delivery_count,
            "kind" => invite.kind,
            "sender_id" => invite.sender,
            "session_host_id" => session_host_id.unwrap_or(0),
            "receiver_id" => invite.receiver,
            "session_type" => invite.session_type,
            "session_id" => invite.session_id,
            "uplay_game_session_present" => game_session.is_some(),
            "uplay_game_session_published_id" => published_uplay_game_session_id,
            "uplay_game_session_id" => game_session.as_ref().map_or(0, |session| session.id),
            "uplay_target_projected" => invite_projects_exact_uplay_target(invite.kind) && game_session.is_some(),
            "uplay_game_session_bytes" => game_session.as_ref().map_or(0, |session| session.data.len()),
            "uplay_game_session_flags" => game_session.as_ref().map_or(0, |session| session.flags),
            "uplay_game_session_invite_only" => game_session.as_ref().is_some_and(|session| session.invite_only),
        );
        Ok(Response::new(misc::EventResponse {
            invite: Some(misc::InviteEvent {
                id: invite.id,
                sender: Some(User {
                    id: sender.ubi_id,
                    username: sender.username,
                    ips: vec![],
                }),
                force_join: self.debug_config.force_joins || invite_forces_join(invite.kind),
                kind: invite.kind,
                game_session,
            }),
            party,
        }))
    }

    async fn accept_invite(&self, request: Request<misc::AcceptInviteRequest>) -> Result<Response<misc::AcceptInviteResponse>, Status> {
        let _generation=self.storage.mode_gate.read().await;
        let game_mode=authenticated_game_mode(&self.storage,&request).await?;
        let user_id = authenticated_user_id(&request)?;
        let invite_id = request.into_inner().id;
        if !self.storage.invite_matches_mode(user_id,invite_id,game_mode).await.map_err(|_|Status::internal("Cannot read invitation mode"))? {
            return Err(Status::failed_precondition("Gameplay mode mismatch or invitation unavailable"));
        }
        // Lock order: publication read lock -> SQLite write transaction.
        // Set/Clear never holds a SQLite transaction while waiting for this lock.
        // The guard runs on the actual target inside the acceptance transaction,
        // before accepted_at or competing invitations are changed.
        let publications = self.uplay_game_sessions.read().await;
        let result = self.storage.accept_invite_checked_async(user_id, invite_id, |invite, target| {
            match invite_publication_readiness(
                invite.kind,
                invite.session_id,
                target.as_ref().map(|(id, attributes)| (*id, attributes.as_str())),
                &publications,
            ) {
                InvitePublicationReadiness::Ready => Ok(()),
                InvitePublicationReadiness::Pending => {
                    info!(self.logger, "InvitePublicationPending"; "invite_id" => invite.id, "kind" => invite.kind, "stage" => "acceptance");
                    Err(Status::unavailable("Invitation target is preparing; retry the same invitation").into())
                }
                InvitePublicationReadiness::Invalid => {
                    Err(Status::not_found("Invitation target is no longer current").into())
                }
            }
        }).await;
        drop(publications);
        let Some(invite) = result.map_err(|error| match error.downcast::<Status>() {
            Ok(status) => status,
            Err(error) => Status::internal(format!("Couldn't accept invite: {error:?}")),
        })? else {
            return Err(Status::not_found("Invitation expired or unavailable"));
        };
        info!(
            self.logger,
            "InviteAccepted";
            "invite_id" => invite.id,
            "kind" => invite.kind,
            "sender_id" => invite.sender,
            "receiver_id" => invite.receiver,
            "session_type" => invite.session_type,
            "session_id" => invite.session_id,
        );
        Ok(Response::new(misc::AcceptInviteResponse {
            accepted: true,
            kind: invite.kind,
        }))
    }

    async fn set_game_session(&self, request: Request<misc::SetGameSessionRequest>) -> Result<Response<misc::SetGameSessionResponse>, Status> {
        let _generation=self.storage.mode_gate.read().await;
        let _game_mode=authenticated_game_mode(&self.storage,&request).await?;
        let user_id = authenticated_user_id(&request)?;
        let request = request.into_inner();
        if !validate_uplay_game_session(&request.data) {
            return Err(Status::invalid_argument("Uplay GameSession data is empty or too large"));
        }
        let session = misc::UplayGameSession {
            id: request.id,
            data: request.data,
            flags: request.flags,
            invite_only: request.invite_only,
        };
        info!(
            self.logger,
            "UplayGameSessionPublished";
            "user_id" => user_id,
            "game_session_id" => session.id,
            "data_bytes" => session.data.len(),
            "flags" => session.flags,
            "invite_only" => session.invite_only,
        );
        let mut publications = self.uplay_game_sessions.write().await;
        self.storage.record_published_game_session(user_id, session.id);
        publications.publish(user_id, session);
        Ok(Response::new(misc::SetGameSessionResponse {}))
    }

    async fn clear_game_session(&self, request: Request<misc::ClearGameSessionRequest>) -> Result<Response<misc::ClearGameSessionResponse>, Status> {
        let _generation=self.storage.mode_gate.read().await;
        let _game_mode=authenticated_game_mode(&self.storage,&request).await?;
        let user_id = authenticated_user_id(&request)?;
        let mut publications = self.uplay_game_sessions.write().await;
        self.storage.clear_published_game_session(user_id);
        let removed = publications.clear_current(user_id);
        info!(self.logger, "UplayGameSessionCleared"; "user_id" => user_id, "removed" => removed);
        Ok(Response::new(misc::ClearGameSessionResponse {}))
    }

    /// Handles P2P testing requests.
    ///
    /// Attempts to establish a UDP connection with the client and exchanges a challenge.
    async fn test_p2p(&self, request: Request<misc::TestP2pRequest>) -> Result<Response<misc::TestP2pResponse>, Status> {
        let mut client_addr = request.remote_addr().ok_or(Status::failed_precondition("no client address"))?;
        client_addr.set_port(13_000);
        let request = request.into_inner();
        let mut resp_data = b"P2P Test - ".to_vec();
        resp_data.extend(request.challenge);

        let buf = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
            socket.connect(client_addr).await?;
            socket.send(&resp_data).await?;
            let mut buf = [0; 1024];
            let len = socket.recv(&mut buf).await?;
            Ok(buf[..len].to_vec())
        });
        let Ok(challenge) = buf.await else {
            return Err(Status::deadline_exceeded("client didn't response in time"));
        };
        let Ok(challenge): std::io::Result<Vec<u8>> = challenge else {
            return Err(Status::unknown(format!("P2P communication failed: {}", challenge.unwrap_err())));
        };

        Ok(Response::new(misc::TestP2pResponse { challenge }))
    }
}

/// Implements the `UsersAdmin` gRPC service for administrative user management.
pub struct MyUsersAdmin {
    logger: Logger,
    storage: Arc<Storage>,
}

#[tonic::async_trait]
impl UsersAdmin for MyUsersAdmin {
    /// Handles requests to list all users.
    ///
    /// Retrieves user information and their associated IP addresses from storage.
    async fn list(&self, request: Request<users::ListRequest>) -> Result<Response<users::ListResponse>, Status> {
        let _request = request.into_inner();
        let Ok(db_users) = self.storage.list_users_async().await else {
            return Err(Status::internal("Error listing users"));
        };

        let mut users = vec![];
        for user in db_users {
            let urls = self.storage.list_urls(user.id).await.map_err(|e| Status::internal(format!("{e:?}")))?;
            let ips = urls
                .into_iter()
                .map(|u| u.parse::<StationURL>())
                .filter_map(Result::ok)
                .map(|u| u.address)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            users.push(User {
                id: user.ubi_id,
                username: user.username,
                ips,
            });
        }

        let resp = users::ListResponse {
            #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
            total: users.len() as i32,
            users,
        };
        Ok(Response::new(resp))
    }

    /// Handles requests to retrieve a single user's details.
    ///
    /// Retrieves user information and their associated IP addresses from storage based on the provided user ID.
    async fn get(&self, request: Request<users::GetRequest>) -> Result<Response<users::GetResponse>, Status> {
        let request = request.into_inner();
        let user_id = request.id;
        let user_id: u32 = user_id.parse().map_err(|_| Status::invalid_argument("Invalid ID"))?;
        let Ok(user) = self.storage.find_user_by_id_async(user_id).await else {
            return Err(Status::internal("Error retrieving user"));
        };

        let Some(user) = user else {
            return Err(Status::not_found("User not found"));
        };

        let urls = self.storage.list_urls(user.id).await.map_err(|e| Status::internal(format!("{e:?}")))?;
        let ips = urls
            .into_iter()
            .map(|u| u.parse::<StationURL>())
            .filter_map(Result::ok)
            .map(|u| u.address)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let resp = users::GetResponse {
            user: Some(User {
                id: user.ubi_id,
                username: user.username,
                ips,
            }),
        };
        Ok(Response::new(resp))
    }

    /// Handles requests to delete a user.
    ///
    /// Deletes a user from storage based on their Ubisoft ID.
    async fn delete(&self, request: Request<users::DeleteRequest>) -> Result<Response<users::DeleteResponse>, Status> {
        let request = request.into_inner();
        let Some(user) = self
            .storage
            .find_user_by_ubi_id_async(&request.id)
            .await
            .map_err(|_| Status::invalid_argument("Invalid ID"))?
        else {
            return Err(Status::not_found("User not found"));
        };
        match self.storage.delete_user_async(user.id).await {
            Ok(()) => {
                warn!(self.logger, "Deleted user {user:?}");
                Ok(Response::new(users::DeleteResponse {}))
            }
            Err(e) => Err(Status::internal(format!("{e:?}"))),
        }
    }
}

/// Implements the `GamesAdmin` gRPC service for administrative game session management.
struct MyGamesAdmin {
    logger: Logger,
    storage: Arc<Storage>,
}

#[tonic::async_trait]
impl GamesAdmin for MyGamesAdmin {
    /// Handles requests to list all active game sessions.
    ///
    /// Retrieves game session information from storage and parses game-specific attributes.
    async fn list(&self, request: Request<games::ListRequest>) -> Result<Response<games::ListResponse>, Status> {
        let _request = request.into_inner();
        let sessions = self.storage.list_game_sessions_async().await.map_err(|e| {
            error!(self.logger, "Error listing games: {e}");
            Status::internal(format!("{e:?}"))
        })?;

        Ok(Response::new(games::ListResponse {
            games: sessions
                .into_iter()
                .filter_map(|s| {
                    let attributes: QList<Property> = s
                        .attributes
                        .parse()
                        .inspect_err(|e| {
                            error!(self.logger, "Error parsing game type: {e}");
                        })
                        .unwrap_or_default();
                    let attributes: HashMap<u32, u32> = attributes.0.into_iter().map(|p| (p.id, p.value)).collect();

                    // just guessing that 105 gives me what I want... ¯\_(ツ)_/¯
                    let game_type = match attributes.get(&105) {
                        None => String::from("Lobby"),
                        Some(&1) => String::from("SvM"),
                        Some(&2) => String::from("Coop"),
                        Some(v) => format!("Unknown({v})"),
                    };

                    let Some(creator) = s.participants.iter().find(|participant| participant.user_id == s.creator_id) else {
                        error!(self.logger, "Skipping inconsistent game session without creator"; "session_id" => s.session_id, "creator_id" => s.creator_id);
                        return None;
                    };
                    let creator = creator.name.clone();

                    Some(games::Game {
                        id: s.session_id,
                        creator,
                        participants: s.participants.into_iter().filter(|p| p.user_id != s.creator_id).map(|p| p.name).collect(),
                        game_type,
                    })
                })
                .collect(),
        }))
    }

    /// Handles requests to delete a game session.
    ///
    /// Deletes a game session from storage based on its session ID.
    async fn delete(&self, request: Request<games::DeleteRequest>) -> Result<Response<games::DeleteResponse>, Status> {
        let request = request.into_inner();
        let session_id = request.id;
        match self.storage.delete_game_session_by_id_async(session_id).await {
            Ok(()) => {
                warn!(self.logger, "Deleted game session {session_id:?}");
                Ok(Response::new(games::DeleteResponse {}))
            }
            Err(e) => Err(Status::internal(format!("{e:?}"))),
        }
    }
}

/// Creates an authenticated gRPC service.
///
/// This function wraps a gRPC service with an interceptor that validates
/// an authorization token present in the request metadata.
fn authenticated<S>(
    service: S,
    logger: Logger,
    token_config: AuthTokenConfig,
    storage: Arc<Storage>,
    overlay_network: LanClientNetwork,
) -> tonic_async_interceptor::AsyncInterceptedService<S, impl tonic_async_interceptor::AsyncInterceptor<Future = impl Future<Output = Result<Request<()>, Status>> + Send> + Clone>
{
    tonic_async_interceptor::AsyncInterceptedService::new(service, move |req: Request<()>| {
        let storage = Arc::clone(&storage);
        let logger = logger.clone();
        let token_config = token_config.clone();
        async move {
            let this = check_token(&logger, &token_config, &storage, overlay_network, req).await;
            if let Err(ref e) = this {
                error!(logger, "Auth failure: {e}");
            }
            this
        }
    })
}

/// Creates a gRPC service with preshared key authentication.
///
/// This function wraps a gRPC service with an interceptor that validates
/// a preshared key present in the "authorization" metadata of the request.
fn preshared_authentication<S>(service: S, key: String) -> tonic::service::interceptor::InterceptedService<S, impl tonic::service::Interceptor + Clone> {
    tonic::service::interceptor::InterceptedService::new(service, move |req: Request<()>| {
        let header_value = req.metadata().get("authorization").ok_or(Status::unauthenticated("Missing authorization"))?;
        let token = header_value.to_str().map_err(|_| Status::unauthenticated("Invalid token"))?;

        if token == key {
            Ok(req)
        } else {
            Err(Status::permission_denied("Invalid token"))
        }
    })
}

/// Encodes a byte slice into a Base32 string.
fn base32(data: &[u8]) -> String {
    let mut s = String::new();
    for chunk in data.chunks(5) {
        let mut value = 0u64;
        let mut i = 0;
        for c in chunk {
            value <<= 8;
            value |= u64::from(*c);
            i += 1;
        }
        value <<= 8 * (5 - i);
        if i == 8 {
            i = 0;
        } else {
            i = 8 - (i * 8 + 4) / 5;
        }
        for i in (i..8).rev() {
            let ch = match ((value >> (5 * i)) & 0b11111) as u8 {
                b @ 0..=9 => b'0' + b,
                b @ 10..=31 => b'A' + b - 10,
                b => unreachable!("{b:?}"),
            };
            s.push(ch as char);
        }
    }
    s
}

/// Starts the gRPC server, binding to the specified address and registering services.
///
/// This function initializes the server, sets up reflection services, and registers
/// the Friends, Users, and Misc gRPC services. Optionally, it enables and registers
/// administrative services (`UsersAdmin` and `GamesAdmin`) if `enable_admin_services` is true.
pub async fn start_server(
    logger: Logger,
    storage: Arc<Storage>,
    server_addr: SocketAddr,
    debug_config: Arc<DebugConfig>,
    enable_admin_services: bool,
    verified_release: ReleaseIdentity,
) -> Result<(), Box<dyn std::error::Error>> {
    let token_config = AuthTokenConfig::from_env(verified_release)?;
    let overlay_network = LanClientNetwork::from_env()?;
    let uplay_game_sessions = Arc::new(RwLock::new(UplayGameSessionRegistry::default()));
    info!(logger, "Listening on {server_addr}");
    let builder = Server::builder()
        .add_service(
            tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(users::FILE_DESCRIPTOR_SET)
                .register_encoded_file_descriptor_set(friends::FILE_DESCRIPTOR_SET)
                .register_encoded_file_descriptor_set(misc::FILE_DESCRIPTOR_SET)
                .build_v1alpha()
                .unwrap(),
        )
        .add_service(
            tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(users::FILE_DESCRIPTOR_SET)
                .register_encoded_file_descriptor_set(friends::FILE_DESCRIPTOR_SET)
                .register_encoded_file_descriptor_set(misc::FILE_DESCRIPTOR_SET)
                .build_v1()
                .unwrap(),
        )
        .add_service(authenticated(
            FriendsServer::new(MyFriends {
                logger: logger.clone(),
                storage: Arc::clone(&storage),
                debug_config: Arc::clone(&debug_config),
                uplay_game_sessions: Arc::clone(&uplay_game_sessions),
            }),
            logger.clone(),
            token_config.clone(),
            Arc::clone(&storage),
            overlay_network,
        ))
        .add_service(authenticated(
            MiscServer::new(MyMisc {
                logger: logger.clone(),
                storage: Arc::clone(&storage),
                debug_config,
                uplay_game_sessions: Arc::clone(&uplay_game_sessions),
            }),
            logger.clone(),
            token_config.clone(),
            Arc::clone(&storage),
            overlay_network,
        ))
        .add_service(UsersServer::new(MyUsers {
            logger: logger.clone(),
            storage: Arc::clone(&storage),
            token_config,
            overlay_network,
        }));

    let builder = if enable_admin_services {
        warn!(logger, "Enabling admin services");
        let preshared = base32(&secretbox::gen_key().0);
        println!("Admin Key: {preshared}");
        builder
            .add_service(preshared_authentication(
                UsersAdminServer::new(MyUsersAdmin {
                    logger: logger.clone(),
                    storage: Arc::clone(&storage),
                }),
                preshared.clone(),
            ))
            .add_service(preshared_authentication(GamesAdminServer::new(MyGamesAdmin { logger, storage }), preshared))
    } else {
        builder
    };

    builder.serve(server_addr).await?;
    Ok(())
}

#[cfg(test)]
mod invite_target_tests {
    use super::*;

    fn session(attributes: &str) -> GameSession {
        GameSession {
            session_type: 1,
            session_id: 1,
            creator_id: 1,
            attributes: attributes.to_string(),
            participants: Vec::new(),
        }
    }

    #[test]
    fn classifies_private_room_and_lobby_party_sessions() {
        assert_eq!(session_invite_kind(&session("113 => 0;103 => 0;112 => 4")), Some(INVITE_KIND_PRIVATE_ROOM));
        assert_eq!(session_invite_kind(&session("113 => 1;3 => 8;4 => 0")), Some(INVITE_KIND_LOBBY_PARTY));
        assert_eq!(session_invite_kind(&session("not valid attributes")), None);
    }

    #[test]
    fn only_private_and_follow_invites_require_uplay_game_session_data() {
        assert!(invite_requires_uplay_game_session(INVITE_KIND_PRIVATE_ROOM));
        assert!(invite_requires_uplay_game_session(INVITE_KIND_PARTY_FOLLOW));
        assert!(!invite_requires_uplay_game_session(INVITE_KIND_LOBBY_PARTY));
        assert!(!invite_requires_uplay_game_session(INVITE_KIND_LOBBY_RESTORE));
    }

    #[test]
    fn member_invitation_uses_the_real_session_hosts_uplay_payload() {
        assert_eq!(invite_uplay_payload_owner(INVITE_KIND_PRIVATE_ROOM, Some(41)), Some(41));
        assert_eq!(invite_uplay_payload_owner(INVITE_KIND_PARTY_FOLLOW, Some(41)), Some(41));
        assert_eq!(invite_uplay_payload_owner(INVITE_KIND_LOBBY_PARTY, Some(41)), None);
        assert_eq!(invite_uplay_payload_owner(INVITE_KIND_PRIVATE_ROOM, None), None);
    }

    #[test]
    fn party_roster_projection_preserves_leader_and_all_members() {
        let roster = project_party_roster(Some((
            2,
            vec![
                crate::storage::User {
                    id: 2,
                    username: String::from("second"),
                    ubi_id: String::from("account-two"),
                    is_online: true,
                },
                crate::storage::User {
                    id: 3,
                    username: String::from("third"),
                    ubi_id: String::from("account-three"),
                    is_online: true,
                },
            ],
        )))
        .unwrap();
        assert_eq!(roster.leader.as_ref().unwrap().id, "account-two");
        assert_eq!(roster.members.len(), 2);
        assert_eq!(roster.members[1].id, "account-three");
    }

    #[test]
    fn follow_and_lobby_restore_invites_force_join() {
        assert!(invite_forces_join(INVITE_KIND_PARTY_FOLLOW));
        assert!(invite_forces_join(INVITE_KIND_LOBBY_RESTORE));
        assert!(!invite_forces_join(INVITE_KIND_PRIVATE_ROOM));
        assert!(!invite_forces_join(INVITE_KIND_LOBBY_PARTY));
    }

    #[test]
    fn validates_uplay_game_session_blob_boundaries() {
        assert!(!validate_uplay_game_session(&[]));
        assert!(validate_uplay_game_session(&[1]));
        assert!(validate_uplay_game_session(&vec![0; MAX_UPLAY_GAME_SESSION_DATA]));
        assert!(!validate_uplay_game_session(&vec![0; MAX_UPLAY_GAME_SESSION_DATA + 1]));
    }
}

fn validate_lan_product(value: &str) -> Result<(), Status> {
    if server_api::gameplay_contract::ClientContract::parse(value).is_some() { Ok(()) }
    else { Err(Status::failed_precondition("Use the matching SCBL LAN client and server")) }
}

#[cfg(test)]
mod lan_invite_regression_tests {
    use super::*;

    fn lobby(id: u32) -> InvitableGameSession {
        InvitableGameSession {
            session: GameSession {session_type: 1, session_id: id, creator_id: 1000,
                attributes: "113 => 1;3 => 8;4 => 0".into(), participants: Vec::new()},
            direct_member: true,
        }
    }
    fn publication(id: u64, invite_only: bool) -> misc::UplayGameSession {
        misc::UplayGameSession {id, data: vec![0x5a;496], flags: 0, invite_only}
    }
    fn registry(id: u64, invite_only: bool) -> UplayGameSessionRegistry {
        let mut registry=UplayGameSessionRegistry::default();
        registry.publish(1000, publication(id,invite_only));registry
    }
    #[test]
    fn lan_menu_invite_only_lobby_is_selectable() {
        println!("INPUT type=1 attributes=113:1,3:8,4:0 current_publication_id=2 invite_only=true");
        let selected=select_invitable_session(1000,vec![lobby(2)],&registry(2,true));
        println!("LOBBY_SELECTED {}",selected.is_some());
        let selected=selected.expect("An exact live lobby publication may be invitation-only");
        assert_eq!(selected.kind,INVITE_KIND_LOBBY_PARTY);
        assert_eq!(selected.session.session_id,2);
    }
    #[test]
    fn lan_menu_invite_only_lobby_survives_delivery_and_acceptance_guard() {
        let live=invite_publication_is_current(INVITE_KIND_LOBBY_PARTY,2,"113 => 1;3 => 8;4 => 0",Some(&publication(2,true)));
        println!("LOBBY_DELIVERY_AND_ACCEPT_GUARD {live}");assert!(live);
    }
    #[test]
    fn lan_missing_or_mismatched_publication_still_rejected() {
        assert!(select_invitable_session(1000,vec![lobby(2)],&registry(3,true)).is_none());
        assert!(select_invitable_session(1000,vec![lobby(2)],&UplayGameSessionRegistry::default()).is_none());
        assert!(!invite_publication_is_current(INVITE_KIND_LOBBY_PARTY,2,"113 => 1;3 => 8;4 => 0",Some(&publication(3,true))));
        assert!(!invite_publication_is_current(INVITE_KIND_LOBBY_PARTY,2,"113 => 1;3 => 8;4 => 0",None));
    }
    #[test]
    fn lan_empty_or_unrelated_sessions_are_not_invented() {
        assert!(select_invitable_session(1000,Vec::new(),&registry(2,true)).is_none());
        let mut unrelated=lobby(2);unrelated.session.attributes="113 => 9".into();
        assert!(select_invitable_session(1000,vec![unrelated],&registry(2,true)).is_none());
    }
    #[test]
    fn lan_public_lobby_and_private_room_keep_their_routes() {
        assert_eq!(select_invitable_session(1000,vec![lobby(2)],&registry(2,false)).unwrap().kind,INVITE_KIND_LOBBY_PARTY);
        let mut room=lobby(3);room.session.attributes="113 => 0;3 => 0;4 => 8".into();
        let selected=select_invitable_session(1000,vec![lobby(2),room],&registry(2,true)).unwrap();
        assert_eq!(selected.kind,INVITE_KIND_PRIVATE_ROOM);assert_eq!(selected.session.session_id,3);
    }
    #[test]
    fn lan_lobby_invite_still_uses_friends_route_not_private_game_payload() {
        assert!(project_invite_uplay_game_session(INVITE_KIND_LOBBY_PARTY,2,Some(publication(2,true))).is_none());
        assert!(!invite_requires_uplay_game_session(INVITE_KIND_LOBBY_PARTY));
    }
}

#[cfg(test)]
mod lan_invite_privacy_tests {
    use super::*;

    fn publication(invite_only: bool) -> misc::UplayGameSession {
        misc::UplayGameSession { id: 2, data: vec![0x5a; 496], flags: 7, invite_only }
    }

    fn lobby() -> InvitableGameSession {
        InvitableGameSession {
            session: GameSession { session_type: 1, session_id: 2, creator_id: 1000,
                attributes: "113 => 1;3 => 8;4 => 0".into(), participants: Vec::new() },
            direct_member: true,
        }
    }

    #[test]
    fn explicit_invitation_keeps_privacy_payload_and_flags_unchanged() {
        let mut registry = UplayGameSessionRegistry::default();
        registry.publish(1000, publication(true));
        let selected = select_invitable_session(1000, vec![lobby()], &registry).unwrap();
        assert!(invite_publication_is_current(selected.kind, selected.session.session_id,
            &selected.session.attributes, registry.current(1000)));
        let unchanged = registry.current(1000).unwrap();
        assert!(unchanged.invite_only);
        assert_eq!(unchanged.id, 2);
        assert_eq!(unchanged.flags, 7);
        assert_eq!(unchanged.data, vec![0x5a; 496]);
        assert!(registry.public_for_active_match(1000).is_none());
    }

    #[test]
    fn privacy_toggle_on_same_live_lobby_keeps_explicit_invitation_valid() {
        let mut registry = UplayGameSessionRegistry::default();
        for invite_only in [true, false, true] {
            registry.publish(1000, publication(invite_only));
            assert!(select_invitable_session(1000, vec![lobby()], &registry).is_some());
            assert!(invite_publication_is_current(INVITE_KIND_LOBBY_PARTY, 2,
                "113 => 1;3 => 8;4 => 0", registry.current(1000)));
            assert_eq!(registry.current(1000).unwrap().invite_only, invite_only);
        }
    }

    #[test]
    fn cleared_current_lobby_cannot_use_cached_public_session_to_invite() {
        let mut registry = UplayGameSessionRegistry::default();
        registry.publish(1000, publication(false));
        registry.publish(1000, publication(true));
        assert!(registry.clear_current(1000));
        assert!(registry.public_for_active_match(1000).is_some());
        assert!(select_invitable_session(1000, vec![lobby()], &registry).is_none());
        assert!(!invite_publication_is_current(INVITE_KIND_LOBBY_PARTY, 2,
            "113 => 1;3 => 8;4 => 0", registry.current(1000)));
    }
}

#[cfg(test)]
mod lan_follow_readiness_tests {
    use super::*;
    const PRIVATE:&str="113 => 0;3 => 0;4 => 12;103 => 0;105 => 1;112 => 6";
    const PUBLIC:&str="113 => 0;3 => 12;4 => 0;103 => 4008726265;105 => 1;112 => 6";
    fn request<T>(user:u32,value:T)->Request<T>{
        // These handler/transaction fixtures emulate the verified interceptor
        // boundary. Full Login -> check_token coverage is in the mode matrix.
        let now=unix_time_seconds().unwrap();
        let config=AuthTokenConfig{release:test_release_identity(),key:hmacsha256::Key([0x48;32]),ttl_seconds:86400};
        let claims=AuthTokenClaims{release:Some(test_release_identity()),user_id:user,username:format!("synthetic-user-{user}"),source_ip:"127.0.0.1".into(),issued_at:now,expires_at:now+86400,auth_version:1,
            game_run:Some(GameRunClaim{product:"scbl-lan/2/game/official".into(),instance:format!("{user:032x}")})};
        let token=issue_auth_token(&config,&claims).unwrap();
        let verified=verify_auth_token(&config,&token,"127.0.0.1".parse().unwrap(),now).unwrap();
        let mut r=Request::new(value);
        r.extensions_mut().insert(tonic::transport::server::TcpConnectInfo{local_addr:Some("127.0.0.1:50051".parse().unwrap()),remote_addr:Some("127.0.0.1:30500".parse().unwrap())});
        apply_verified_identity(&mut r,&verified);r
    }
    fn fixture(private:bool,members:u32)->(MyMisc,Arc<Storage>,u32,u32,tokio::runtime::Runtime){
        let (storage,lobby,room)=Storage::lan_readiness_fixture(if private{PRIVATE}else{PUBLIC},members);
        let mut registry=UplayGameSessionRegistry::default();
        registry.publish(1,misc::UplayGameSession{id:u64::from(lobby),data:vec![0x5a;496],flags:7,invite_only:private});
        storage.record_published_game_session(1,u64::from(lobby));
        let service=MyMisc{logger:Logger::root(slog::Discard,slog::o!()),storage:Arc::clone(&storage),
            debug_config:Arc::new(DebugConfig::default()),uplay_game_sessions:Arc::new(RwLock::new(registry))};
        (service,storage,lobby,room,tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap())
    }
    fn event(service:&MyMisc,rt:&tokio::runtime::Runtime)->misc::EventResponse {
        rt.block_on(service.event(request(2,misc::EventRequest{}))).unwrap().into_inner()
    }
    fn clear(service:&MyMisc,rt:&tokio::runtime::Runtime){
        rt.block_on(service.clear_game_session(request(1,misc::ClearGameSessionRequest{}))).unwrap();
    }
    fn publish(service:&MyMisc,rt:&tokio::runtime::Runtime,id:u32,private:bool){
        rt.block_on(service.set_game_session(request(1,misc::SetGameSessionRequest{id:u64::from(id),data:vec![0x5a;496],flags:7,invite_only:private}))).unwrap();
    }
    fn accept(service:&MyMisc,rt:&tokio::runtime::Runtime,user:u32,id:i64)->Result<Response<misc::AcceptInviteResponse>,Status>{
        rt.block_on(service.accept_invite(request(user,misc::AcceptInviteRequest{id})))
    }
    #[test] fn follow_retains_kind_payload_delay_contract_and_party() {
        let (s,db,lobby,room,rt)=fixture(true,3);let e=event(&s,&rt);let i=e.invite.unwrap();let p=i.game_session.unwrap();
        println!("FOLLOW kind={} published={} room={} party_members={}",i.kind,p.id,room,e.party.as_ref().unwrap().members.len());
        assert_eq!(i.kind,3);assert_eq!(p.id,u64::from(lobby));assert_eq!(p.data,vec![0x5a;496]);assert_eq!(p.flags,7);assert!(i.force_join);
        assert_eq!(e.party.unwrap().members.len(),3);assert_eq!(db.lan_readiness_state(i.id),Some((3,false,false)));
        assert_eq!(db.game_session_diagnostics(1,room).unwrap().unwrap().2,vec![1]);
    }
    #[test] fn accepted_follow_does_not_enter_manual_search() {
        let (s,db,_,room,rt)=fixture(true,2);let i=event(&s,&rt).invite.unwrap();
        assert!(accept(&s,&rt,2,i.id).unwrap().into_inner().accepted);
        let manual=db.find_accepted_manual_invited_session(2,1).unwrap();println!("FOLLOW_MANUAL_SEARCH={}",manual.is_some());assert!(manual.is_none());
        let exact=db.find_accepted_invited_session(2,1).unwrap().unwrap();assert_eq!(exact.kind,3);assert_eq!(exact.session.session_id,room);
        assert_eq!(exact.session.attributes,PRIVATE);assert_eq!(exact.session.participants.len(),1);
    }
    #[test] fn event_clear_set_retains_same_invite() {
        let (s,db,lobby,_,rt)=fixture(true,2);let id=db.lan_readiness_id();clear(&s,&rt);
        assert!(event(&s,&rt).invite.is_none());println!("DEFERRED_EVENT_STATE={:?}",db.lan_readiness_state(id));
        assert_eq!(db.lan_readiness_state(id),Some((3,false,false)));
        publish(&s,&rt,lobby,true);db.lan_readiness_redelivery();assert_eq!(event(&s,&rt).invite.unwrap().id,id);
    }
    #[test] fn accept_clear_set_is_non_destructive_and_retries() {
        let (s,db,lobby,room,rt)=fixture(true,2);let i=event(&s,&rt).invite.unwrap();let other=db.lan_readiness_competitor(i.id);
        clear(&s,&rt);let err=accept(&s,&rt,2,i.id).unwrap_err();
        println!("DEFERRED_ACCEPT code={:?} chosen={:?} competing={:?}",err.code(),db.lan_readiness_state(i.id),db.lan_readiness_state(other));
        assert_eq!(err.code(),tonic::Code::Unavailable);assert_eq!(db.lan_readiness_state(i.id),Some((3,false,false)));
        assert_eq!(db.lan_readiness_state(other),Some((1,false,false)));assert_eq!(db.game_session_diagnostics(1,room).unwrap().unwrap().2,vec![1]);
        publish(&s,&rt,lobby,true);assert!(accept(&s,&rt,2,i.id).unwrap().into_inner().accepted);
        assert_eq!(db.lan_readiness_state(i.id),Some((3,true,false)));assert_eq!(db.lan_readiness_state(other),Some((1,false,true)));
        assert!(accept(&s,&rt,2,i.id).unwrap().into_inner().accepted);
    }
    #[test] fn manual_invite_missing_publication_defers_without_acceptance() {
        let (s,db,_,_,rt)=fixture(true,2);let id=db.lan_readiness_id();db.lan_readiness_kind(id,1);clear(&s,&rt);
        assert!(event(&s,&rt).invite.is_none());assert_eq!(db.lan_readiness_state(id),Some((1,false,false)));
        assert_eq!(accept(&s,&rt,2,id).unwrap_err().code(),tonic::Code::Unavailable);
        assert_eq!(db.lan_readiness_state(id),Some((1,false,false)));
    }
    #[test] fn stale_manual_publication_does_not_accept_or_retire_competitor() {
        let (s,db,lobby,_,rt)=fixture(true,2);let id=db.lan_readiness_id();db.lan_readiness_kind(id,1);let other=db.lan_readiness_competitor(id);
        publish(&s,&rt,lobby,false);assert_eq!(accept(&s,&rt,2,id).unwrap_err().code(),tonic::Code::Unavailable,
            "a live target with a switching publication remains pending, not permanently invalid");
        println!("STALE_ACCEPT chosen={:?} competing={:?}",db.lan_readiness_state(id),db.lan_readiness_state(other));
        assert!(!db.lan_readiness_state(id).unwrap().1);assert_eq!(db.lan_readiness_state(other),Some((1,false,false)));
    }
    #[test] fn manual_private_path_still_projects_exact_room_and_searches() {
        let (s,db,_,room,rt)=fixture(true,2);let id=db.lan_readiness_id();db.lan_readiness_kind(id,1);
        let i=event(&s,&rt).invite.unwrap();assert_eq!(i.kind,1);assert_eq!(i.game_session.unwrap().id,u64::from(room));assert!(!i.force_join);
        accept(&s,&rt,2,id).unwrap();assert_eq!(db.find_accepted_manual_invited_session(2,1).unwrap().unwrap().session.session_id,room);
    }
    #[test] fn cancelled_after_delivery_cannot_accept() {
        let (s,db,_,room,rt)=fixture(true,2);let id=event(&s,&rt).invite.unwrap().id;db.lan_readiness_invalidate(room,false);
        assert!(accept(&s,&rt,2,id).is_err());assert!(db.find_accepted_invited_session(2,1).unwrap().is_none());
    }
    #[test] fn expired_invite_never_delivered_or_accepted() {
        let (s,db,_,room,rt)=fixture(true,2);let id=db.lan_readiness_id();db.lan_readiness_invalidate(room,true);
        assert!(event(&s,&rt).invite.is_none());assert!(accept(&s,&rt,2,id).is_err());
    }
    #[test] fn other_receiver_cannot_accept() {
        let (s,db,_,_,rt)=fixture(true,2);let id=event(&s,&rt).invite.unwrap().id;
        assert!(accept(&s,&rt,3,id).is_err());assert_eq!(db.lan_readiness_state(id),Some((3,false,false)));
    }
    #[test] fn native_admission_suppresses_duplicate_follow() {
        let (s,db,_,room,rt)=fixture(true,2);assert!(db.join_game_session_unbounded(2,1,room).unwrap());assert!(event(&s,&rt).invite.is_none());
    }
    #[test] fn public_follow_retains_party_transport() {
        let (s,db,lobby,_,rt)=fixture(false,2);let i=event(&s,&rt).invite.unwrap();
        assert_eq!(i.kind,3);assert_eq!(i.game_session.unwrap().id,u64::from(lobby));assert!(i.force_join);
        accept(&s,&rt,2,i.id).unwrap();assert!(db.find_accepted_manual_invited_session(2,1).unwrap().is_none());
    }
    #[test] fn ready_accept_preserves_three_member_party_until_native_admission() {
        let (s,db,_,room,rt)=fixture(true,3);let i=event(&s,&rt).invite.unwrap();
        accept(&s,&rt,2,i.id).unwrap();assert_eq!(rt.block_on(db.find_party_roster_async(2)).unwrap().unwrap().1.len(),3);
        assert_eq!(db.game_session_diagnostics(1,room).unwrap().unwrap().2,vec![1]);
        db.add_participants(1,1,room,vec![2,3],Vec::new()).unwrap();
        assert_eq!(db.game_session_diagnostics(1,room).unwrap().unwrap().2,vec![1,2,3]);
    }
    fn acceptance_commit_race(cancel_target:bool) {
        let (s,db,lobby,room,rt)=fixture(true,2);
        let id=event(&s,&rt).invite.unwrap().id;
        let service=Arc::new(s);
        rt.block_on(async {
            let mut held=db.lan_readiness_hold_write().await;
            let accepting=Arc::clone(&service);
            let accept_task=tokio::spawn(async move {
                accepting.accept_invite(request(2,misc::AcceptInviteRequest{id})).await
            });
            let registry_locked=tokio::time::timeout(std::time::Duration::from_secs(1),async {
                loop {
                    if service.uplay_game_sessions.try_write().is_err(){break;}
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
            }).await.is_ok();
            if !registry_locked {
                held.rollback().await.unwrap();
                let _=accept_task.await;
                panic!("acceptance must pin publication before waiting for the SQLite transaction");
            }
            let clearing=Arc::clone(&service);
            let (tx,rx)=tokio::sync::oneshot::channel();
            let mut clear_task=tokio::spawn(async move {
                tx.send(()).unwrap();
                clearing.clear_game_session(request(1,misc::ClearGameSessionRequest{})).await
            });
            rx.await.unwrap();
            let clear_was_blocked=tokio::time::timeout(std::time::Duration::from_millis(30),&mut clear_task).await.is_err();
            let published_while_clear_waits=db.lan_readiness_published(1);
            if cancel_target {
                sqlx::query("UPDATE game_sessions SET destroyed_at=CURRENT_TIMESTAMP WHERE id=?").bind(room).execute(&mut *held).await.unwrap();
            }
            held.commit().await.unwrap();
            let result=tokio::time::timeout(std::time::Duration::from_secs(3),accept_task).await.unwrap().unwrap();
            if clear_was_blocked {
                tokio::time::timeout(std::time::Duration::from_secs(3),clear_task).await.unwrap().unwrap().unwrap();
            }
            println!("COMMIT_RACE cancel_target={cancel_target} publication_pinned={registry_locked} clear_waited={clear_was_blocked} accepted={}",result.is_ok());
            assert_eq!(published_while_clear_waits,Some(lobby),"storage publication must not clear before the registry lock");
            assert!(clear_was_blocked,"Clear must not interleave readiness validation and acceptance commit");
            assert_eq!(result.is_ok(),!cancel_target,"authorization must recheck the target in the commit transaction");
            assert!(service.uplay_game_sessions.read().await.current(1).is_none());
        });
    }
    #[test] fn publication_clear_waits_for_acceptance_commit() { acceptance_commit_race(false); }
    #[test] fn target_cancelled_while_waiting_for_commit_is_rejected() { acceptance_commit_race(true); }

    #[test] fn expiry_between_guard_and_update_cannot_accept_or_retire_competitor() {
        let (_s,db,_,_,rt)=fixture(true,2);let id=db.lan_readiness_id();let other=db.lan_readiness_competitor(id);
        db.lan_readiness_expire_soon(id);
        let accepted=rt.block_on(db.accept_invite_checked_async(2,id,|_,_|{
            std::thread::sleep(std::time::Duration::from_millis(1200));Ok(())
        })).unwrap();
        println!("EXPIRY_AT_COMMIT accepted={} competing={:?}",accepted.is_some(),db.lan_readiness_state(other));
        assert!(accepted.is_none());assert_eq!(db.lan_readiness_state(other),Some((1,false,false)));
    }

}


#[cfg(test)]
mod maintenance_mode_contract_observer {
    #[test]
    fn same_input_product_contract_observation() {
        println!("MODE_CONTRACT legacy_account={} native_official={} native_mod={} unknown={}",
            super::validate_lan_product("scbl-lan/1/official").is_ok(),
            super::validate_lan_product("scbl-lan/2/game/official").is_ok(),
            super::validate_lan_product("scbl-lan/2/game/mod").is_ok(),
            super::validate_lan_product("scbl-lan/2/game/unknown").is_ok());
    }
}


#[cfg(test)]
pub(crate) fn maintenance_authenticated_game_login(storage: Arc<Storage>, username: &str, product: &str, instance: &str) -> u32 {
    use tonic::transport::server::TcpConnectInfo;
    use server_api::gameplay_contract::GAME_INSTANCE_HEADER;
    let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let config=AuthTokenConfig{release:test_release_identity(),key:hmacsha256::Key([0x46;32]),ttl_seconds:DEFAULT_AUTH_TOKEN_TTL_SECONDS};
        let logger=Logger::root(slog::Discard,slog::o!());
        let users=MyUsers{logger:logger.clone(),token_config:config.clone(),storage:storage.clone(),overlay_network:LanClientNetwork};
        let connect=TcpConnectInfo{local_addr:Some("127.0.0.1:50051".parse().unwrap()),remote_addr:Some("127.0.0.1:30500".parse().unwrap())};
        let mut request=Request::new(users::LoginRequest{package_identity:Some(test_package_identity()),client_product:product.into(),username:username.into(),password:"fixture-only-password".into()});
        request.metadata_mut().insert(GAME_INSTANCE_HEADER,instance.parse().unwrap());
        request.extensions_mut().insert(connect.clone());
        let response=users.login(request).await.unwrap().into_inner();
        assert!(response.error.is_empty());
        // Execute the same authenticated interceptor, not a fabricated mode flag.
        let mut event=Request::new(());event.extensions_mut().insert(connect);
        event.metadata_mut().insert("authorization",response.token.parse().unwrap());
        let verified=check_token(&logger,&config,&storage,LanClientNetwork,event).await.unwrap();
        let run=verified.extensions().get::<GameRunClaim>().unwrap();
        assert_eq!(run.product,product);assert_eq!(run.instance,instance);
        authenticated_user_id(&verified).unwrap()
    })
}


#[cfg(test)]
pub(crate) fn maintenance_api_invitation_checks(storage:Arc<Storage>,host:u32,peer:u32,other:u32,room:u32) {
    async fn request<T>(storage:&Arc<Storage>,config:&AuthTokenConfig,user:u32,mode:Option<i64>,payload:T)->Request<T> {
        let identity=storage.find_auth_identity_by_user_id_async(user).await.unwrap().unwrap();
        let now=unix_time_seconds().unwrap();
        let claims=AuthTokenClaims{release:Some(test_release_identity()),user_id:user,username:identity.username,source_ip:"127.0.0.1".into(),issued_at:now,expires_at:now+86400,auth_version:identity.auth_version,
            game_run:mode.map(|m|GameRunClaim{product:if m==1 {"scbl-lan/2/game/official".into()} else {"scbl-lan/2/game/mod".into()},instance:format!("{user:032x}")})};
        let token=issue_auth_token(config,&claims).unwrap();
        let mut request=Request::new(payload);
        request.metadata_mut().insert("authorization",token.parse().unwrap());
        request.extensions_mut().insert(tonic::transport::server::TcpConnectInfo{local_addr:Some("127.0.0.1:50051".parse().unwrap()),remote_addr:Some("127.0.0.1:30500".parse().unwrap())});
        check_token(&Logger::root(slog::Discard,slog::o!()),config,storage,LanClientNetwork,request).await.unwrap()
    }
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let logger=Logger::root(slog::Discard,slog::o!());
        let config=AuthTokenConfig{release:test_release_identity(),key:hmacsha256::Key([0x39;32]),ttl_seconds:DEFAULT_AUTH_TOKEN_TTL_SECONDS};
        let debug_config=Arc::new(DebugConfig::default());
        let mut publication=UplayGameSessionRegistry::default();
        publication.publish(host,misc::UplayGameSession{id:room as u64,data:vec![0x5a;496],flags:0,invite_only:false});
        let publications=Arc::new(RwLock::new(publication));
        let friends=MyFriends{logger:logger.clone(),storage:storage.clone(),debug_config:debug_config.clone(),uplay_game_sessions:publications.clone()};
        let misc=MyMisc{logger,storage:storage.clone(),debug_config,uplay_game_sessions:publications};
        let other_account=storage.find_user_by_id_async(other).await.unwrap().unwrap().ubi_id;
        let peer_account=storage.find_user_by_id_async(peer).await.unwrap().unwrap().ubi_id;
        let bad=friends.invite(request(&storage,&config,host,Some(2),friends::InviteRequest{id:other_account}).await).await;
        assert_eq!(bad.unwrap_err().code(),tonic::Code::FailedPrecondition);
        friends.invite(request(&storage,&config,host,Some(2),friends::InviteRequest{id:peer_account}).await).await.unwrap();
        let event=misc.event(request(&storage,&config,peer,Some(2),misc::EventRequest{}).await).await.unwrap().into_inner();
        let invite=event.invite.unwrap();assert_eq!(invite.kind,INVITE_KIND_LOBBY_PARTY);
        assert!(misc.accept_invite(request(&storage,&config,peer,Some(2),misc::AcceptInviteRequest{id:invite.id}).await).await.unwrap().into_inner().accepted);
        // Simulate a stale queued event written before a mode switch. Both delivery
        // and acceptance inspect the immutable real target, not invite UI strings.
        let stale=storage.add_invite_async(host,other,INVITE_KIND_LOBBY_PARTY,1,room).await.unwrap().unwrap();
        let rejected=misc.accept_invite(request(&storage,&config,other,Some(1),misc::AcceptInviteRequest{id:stale}).await).await.unwrap_err();
        assert_eq!(rejected.code(),tonic::Code::FailedPrecondition);
        assert!(misc.event(request(&storage,&config,other,Some(1),misc::EventRequest{}).await).await.unwrap().into_inner().invite.is_none());
        let account=misc.event(request(&storage,&config,peer,None,misc::EventRequest{}).await).await.unwrap_err();
        assert_eq!(account.code(),tonic::Code::FailedPrecondition);
        assert_eq!(storage.current_game_mode(peer).await.unwrap(),Some(2));
        // The interceptor has already accepted this request. Retire its run before
        // the method takes its lock; the method must still reject the stale claim.
        let delayed=request(&storage,&config,peer,Some(2),misc::EventRequest{}).await;
        {let _generation=storage.mode_gate.write().await;storage.begin_game_run(peer,"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",1,"127.0.0.1".parse().unwrap()).await.unwrap();}
        assert_eq!(misc.event(delayed).await.unwrap_err().code(),tonic::Code::Unauthenticated);
        println!("MODE_API same_mode_invite_delivery_accept=true cross_mode_create_delivery_accept_rejected=true account_cannot_act_as_game=true delayed_old_claim_rejected=true");
    });
}


#[cfg(test)]
mod registration_identity_tests {
    use super::valid_lan_registration_identity;

    #[test]
    fn rejects_nul_at_every_identity_position() {
        for invalid in ["\0", "\0name", "na\0me", "name\0", "玩家\0甲"] {
            assert!(!valid_lan_registration_identity(invalid, "account"));
            assert!(!valid_lan_registration_identity("Player", invalid));
        }
    }

    #[test]
    fn preserves_valid_unicode_whitespace_and_byte_limits() {
        for name in ["Player", "玩家🙂", " Player ", &"x".repeat(64), &"甲".repeat(21)] {
            assert!(valid_lan_registration_identity(name, "account"));
        }
        // Account whitespace was accepted before; this change does not rewrite it.
        for account in [" ", "帳號🙂", &"x".repeat(64)] {
            assert!(valid_lan_registration_identity("Player", account));
        }
    }

    #[test]
    fn preserves_existing_empty_and_oversize_rejections() {
        for name in ["", " \t\n", &"x".repeat(65), &"甲".repeat(22)] {
            assert!(!valid_lan_registration_identity(name, "account"));
        }
        for account in ["", &"x".repeat(65)] {
            assert!(!valid_lan_registration_identity("Player", account));
        }
    }
}

// Test fixtures are not a startup catalog/provider and are never used by main.
#[cfg(test)]
pub(crate) fn test_release_identity() -> ReleaseIdentity {
    ReleaseIdentity::new(1,"fixture-release-2","fixture-compat-1",&"a".repeat(64)).unwrap()
}
#[cfg(test)]
fn test_package_identity() -> users::PackageIdentity {
    let role=Some(users::ReleaseComponent{release_id:"fixture-release-2".into(),compat_id:"fixture-compat-1".into()});
    users::PackageIdentity{schema:1,release_id:"fixture-release-2".into(),compat_id:"fixture-compat-1".into(),manifest_sha256:"a".repeat(64),launcher:role.clone(),main_dll:role.clone(),gameplay_dll:role.clone(),server:role}
}
#[cfg(test)]
#[path = "release_admission_tests.rs"]
mod release_admission_tests;

#[cfg(test)]
#[path = "login_reset_tests.rs"]
mod login_reset_tests;

#[cfg(test)]
#[path = "diagnostic_identity_tests.rs"]
pub(crate) mod diagnostic_identity_tests;
