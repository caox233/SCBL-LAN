use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::sync::OnceLock;

use server_api::friends::friends_client::FriendsClient;
use server_api::friends::InviteRequest;
use server_api::friends::ListRequest;
use server_api::misc::misc_client::MiscClient;
use server_api::misc::AcceptInviteRequest;
use server_api::misc::EventRequest;
use server_api::misc::EventResponse;
use server_api::users::users_client::UsersClient;
use server_api::users::LoginRequest;
use tonic::metadata::Ascii;
use tonic::metadata::MetadataValue;
use tracing::error;
use tracing::info;
use tracing::instrument;

static TOKEN: Mutex<Option<MetadataValue<Ascii>>> = Mutex::new(None);
static CREDS: Mutex<Option<(String, String)>> = Mutex::new(None);

struct InviteIdState {
    next_local_id: i64,
    server_to_current: HashMap<i64, (i64, bool)>,
    local_to_server: HashMap<i64, i64>,
}

impl InviteIdState {
    fn new() -> Self {
        Self {
            next_local_id: -1,
            server_to_current: HashMap::new(),
            local_to_server: HashMap::new(),
        }
    }

    fn allocate_local_id(&mut self) -> i64 {
        while self.local_to_server.contains_key(&self.next_local_id) {
            self.next_local_id = self.next_local_id.saturating_sub(1);
        }
        let local_id = self.next_local_id;
        self.next_local_id = self.next_local_id.saturating_sub(1);
        local_id
    }

    fn map_server_event(&mut self, server_id: i64) -> Option<i64> {
        if let Some((local_id, retired)) = self.server_to_current.get(&server_id).copied() {
            // Current server IDs are persistent AUTOINCREMENT generations.
            // A late poll must not turn an acknowledged invitation into a new one.
            return (!retired).then_some(local_id);
        }
        let local_id = self.allocate_local_id();
        self.server_to_current.insert(server_id, (local_id, false));
        self.local_to_server.insert(local_id, server_id);
        Some(local_id)
    }

    fn is_pending_local(&self, local_id: i64) -> bool {
        let Some(server_id) = self.local_to_server.get(&local_id) else {
            // Preserve the legacy direct positive server-ID call path.
            return local_id > 0;
        };
        self.server_to_current.get(server_id)
            .is_some_and(|(current, retired)| *current == local_id && !retired)
    }

    fn server_id_for_local(&self, local_id: i64) -> i64 {
        self.local_to_server.get(&local_id).copied().unwrap_or(local_id)
    }

    fn retire_local(&mut self, local_id: i64) -> bool {
        let Some(server_id) = self.local_to_server.get(&local_id).copied() else {
            return local_id > 0;
        };
        if let Some((current_local_id, retired)) = self.server_to_current.get_mut(&server_id) {
            if *current_local_id == local_id && !*retired {
                *retired = true;
                return true;
            }
        }
        false
    }
}

static INVITE_ID_STATE: OnceLock<Mutex<InviteIdState>> = OnceLock::new();

fn invite_id_state() -> &'static Mutex<InviteIdState> {
    INVITE_ID_STATE.get_or_init(|| Mutex::new(InviteIdState::new()))
}

// Buffered overlay events may outlive the asynchronous acceptance that retired them.
pub(crate) fn invite_is_pending(local_id: i64) -> bool {
    invite_id_state().lock().unwrap().is_pending_local(local_id)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Missing URL for the api server")]
    MissingUrl,
    #[error("Transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("gRPC error: {0}")]
    GRPCStatus(#[from] tonic::Status),
    #[error("Login failure")]
    LoginFailure,
    #[error("Login failure")]
    InvalidToken(#[from] tonic::metadata::errors::InvalidMetadataValue),
}

// A Channel normally reconnects by itself, but Windows sleep can leave the
// underlying HTTP/2 transport alive-looking while requests are still routed to
// a dead socket. A reset also retires pending creators from the previous epoch.
struct ConnectionCache<T> {
    epoch: std::sync::Arc<()>,
    channel: Option<T>,
}

impl<T: Clone> ConnectionCache<T> {
    fn new() -> Self {
        Self { epoch: std::sync::Arc::new(()), channel: None }
    }

    fn snapshot(&self) -> (std::sync::Arc<()>, Option<T>) {
        (self.epoch.clone(), self.channel.clone())
    }

    fn publish(&mut self, epoch: &std::sync::Arc<()>, channel: T) -> Option<T> {
        if !std::sync::Arc::ptr_eq(epoch, &self.epoch) {
            return None;
        }
        // Concurrent creators in this epoch reuse its first completed channel.
        Some(self.channel.get_or_insert(channel).clone())
    }

    fn reset(&mut self) -> Option<T> {
        // Retained Arc identities cannot wrap or alias a still-pending creator.
        self.epoch = std::sync::Arc::new(());
        self.channel.take()
    }
}

static CONNECTION: std::sync::LazyLock<Mutex<ConnectionCache<tonic::transport::Channel>>> =
    std::sync::LazyLock::new(|| Mutex::new(ConnectionCache::new()));

async fn create_channel() -> std::result::Result<tonic::transport::Channel, Error> {
    loop {
        let (epoch, cached) = CONNECTION.lock().unwrap().snapshot();
        if let Some(channel) = cached {
            tracing::debug!("Reusing connection {channel:?}");
            return Ok(channel);
        }

        let Some(url) = crate::config::URL.get() else {
            return Err(Error::MissingUrl);
        };
        tracing::debug!("Connecting to {url}");
        let channel = tonic::transport::Channel::from_shared(url.as_str())
            .unwrap()
            .connect_timeout(std::time::Duration::from_secs(1))
            .timeout(std::time::Duration::from_secs(10))
            .connect()
            .await?;
        let published = CONNECTION.lock().unwrap().publish(&epoch, channel);
        if let Some(channel) = published {
            tracing::debug!("Connected to {url}");
            return Ok(channel);
        }
        // reset_connection won while connect was pending. Do not return or
        // cache the retired handle; reuse the new epoch or connect for it.
    }
}

pub fn reset_connection(reason: &str) {
    let previous = CONNECTION.lock().unwrap().reset();
    // Drop the old handle and log outside the cache lock.
    let removed = previous.is_some();
    tracing::warn!("API connection reset: reason={reason}, had_cached_channel={removed}");
}

macro_rules! connect {
    ($client:ident) => {{
        let channel = create_channel().await?;
        $client::with_interceptor(channel, move |mut req: tonic::Request<_>| {
            let guard = TOKEN.lock().unwrap();
            if let Some(token) = (*guard).as_ref() {
                tracing::debug!("Adding auth token");
                req.metadata_mut().insert("authorization", token.clone());
            }
            Ok(req)
        })
    }};
}

mod publication;
pub use publication::{clear_game_session, publish_game_session};
use publication::{republish_local_game_session, retry_pending_game_session};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Friend {
    pub id: String,
    pub username: String,
    pub is_online: bool,
}

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

pub fn runtime() -> Result<&'static tokio::runtime::Runtime, Error> {
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let _ = RUNTIME.set(tokio::runtime::Runtime::new()?);
    Ok(RUNTIME.get().unwrap())
}

fn run<T>(future: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
    runtime()?.block_on(future)
}

fn invite_connection_retryable(error: &Error) -> bool {
    match error {
        Error::Transport(_) => true,
        Error::GRPCStatus(status) => status.code() == tonic::Code::Unavailable,
        _ => false,
    }
}

async fn invite_friend_remote(id: &str) -> Result<(), Error> {
    let mut client = connect!(FriendsClient);
    let request = tonic::Request::new(InviteRequest { id: id.into() });
    client.invite(request).await?;
    Ok(())
}

pub fn invite_friend(id: &str) -> Result<(), Error> {
    run(async {
        let first = invite_friend_remote(id).await;
        if !first.as_ref().is_err_and(invite_connection_retryable) {
            return first;
        }
        tracing::warn!("Invite transport unavailable; resetting the cached channel and retrying once");
        reset_connection("invite transport unavailable");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        invite_friend_remote(id).await
    })
}

async fn accept_server_invite(server_id: i64) -> Result<bool, Error> {
    let mut client = connect!(MiscClient);
    let request = tonic::Request::new(AcceptInviteRequest { id: server_id });
    Ok(client.accept_invite(request).await?.into_inner().accepted)
}

fn invite_accept_result_is_terminal(result: &Result<bool, Error>) -> bool {
    result.is_ok() || matches!(result, Err(Error::GRPCStatus(status)) if status.code() == tonic::Code::NotFound)
}

pub(crate) async fn accept_invite_async(local_id: i64) -> Result<(bool, i64), Error> {
    let server_id = {
        let state = invite_id_state().lock().unwrap();
        let server_id = state.server_id_for_local(local_id);
        if !state.is_pending_local(local_id) {
            return Ok((false, server_id));
        }
        server_id
    };
    let result = accept_server_invite(server_id).await;
    if invite_accept_result_is_terminal(&result) {
        // The server is idempotent; the native transition must still be emitted
        // only once if two old/new overlay workers complete the same local ID.
        let first_terminal = invite_id_state().lock().unwrap().retire_local(local_id);
        if matches!(result, Ok(true)) && !first_terminal {
            return Ok((false, server_id));
        }
    }
    result.map(|accepted| (accepted, server_id))
}

pub async fn list_friends_async() -> Result<Vec<Friend>, Error> {
    let mut client = connect!(FriendsClient);
    let request = tonic::Request::new(ListRequest {});
    let response = client.list(request).await?.into_inner();
    Ok(response
        .friends
        .into_iter()
        .map(|friend| Friend {
            id: friend.id,
            username: friend.username,
            is_online: friend.is_online,
        })
        .collect())
}

pub fn list_friends() -> Result<Vec<Friend>, Error> {
    run(list_friends_async())
}


fn game_login_request(username: &str, password: &str, mode: hooks_config::GameplayMode, instance: &str, release: server_api::users::PackageIdentity) -> Result<tonic::Request<LoginRequest>, Error> {
    use server_api::gameplay_contract::{GameplayMode, GAME_INSTANCE_HEADER, valid_game_instance};
    if !valid_game_instance(instance) { return Err(Error::LoginFailure); }
    let mode = match mode {
        hooks_config::GameplayMode::Official => GameplayMode::Official,
        hooks_config::GameplayMode::Mod => GameplayMode::Mod,
    };
    let mut request = tonic::Request::new(LoginRequest {
        package_identity: Some(release),
        client_product: mode.product().into(),
        username: username.into(), password: password.into(),
    });
    request.metadata_mut().insert(GAME_INSTANCE_HEADER,instance.parse()?);
    Ok(request)
}

fn current_game_instance() -> Result<&'static str, Error> {
    // Created once per loaded main DLL. Relogin must not create another run;
    // account UI login is deliberately a separate product with no such nonce.
    static INSTANCE: OnceLock<Result<String,String>> = OnceLock::new();
    let value = INSTANCE.get_or_init(|| unsafe {
        windows::Win32::System::Com::CoCreateGuid()
            .map(|guid| format!("{:032x}",guid.to_u128()))
            .map_err(|e| e.to_string())
    });
    value.as_ref().map(String::as_str).map_err(|_| Error::LoginFailure)
}

#[cfg(test)]
mod mode_identity_tests {
    use super::*;
    use server_api::gameplay_contract::{GAME_INSTANCE_HEADER,valid_game_instance};
    fn fixture_release() -> server_api::users::PackageIdentity {
        crate::release_identity::to_declaration("a".repeat(64))
    }
    #[test] fn explicit_config_is_carried_without_changing_credentials() {
        for (mode,product) in [(hooks_config::GameplayMode::Official,"scbl-lan/2/game/official"),(hooks_config::GameplayMode::Mod,"scbl-lan/2/game/mod")] {
            let r=game_login_request("fixture_user","fixture_password",mode,"0123456789abcdef0123456789abcdef",fixture_release()).unwrap();
            assert_eq!(r.get_ref().client_product,product);
            assert_eq!(r.get_ref().package_identity,Some(fixture_release()));
            assert_eq!(r.get_ref().username,"fixture_user");assert_eq!(r.get_ref().password,"fixture_password");
            assert_eq!(r.metadata().get(GAME_INSTANCE_HEADER).unwrap(),"0123456789abcdef0123456789abcdef");
        }
    }
    #[test] fn runtime_instance_survives_relogin_and_is_valid() {
        let one=current_game_instance().unwrap();assert!(valid_game_instance(one));
        assert_eq!(one,current_game_instance().unwrap());
    }
    #[test] fn missing_instance_is_not_an_official_fallback() {
        assert!(game_login_request("fixture","fixture",hooks_config::GameplayMode::Official,"",fixture_release()).is_err());
    }
}

// Initial login, recovery and the native SDK must not overwrite one another.
static LOGIN_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
const NATIVE_LOGIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

// Created only after LOGIN_LOCK is acquired, so cancelling a waiter cannot
// invalidate the current owner. Drop precedes releasing that lock.
struct AuthenticationAttempt {
    completed: bool,
}

impl Drop for AuthenticationAttempt {
    fn drop(&mut self) {
        if !self.completed {
            // Cancellation skips the result match; fail closed just as an error
            // does, without panicking again if an earlier token write panicked.
            *TOKEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }
}

fn login_response_token(response: server_api::users::LoginResponse) -> Result<MetadataValue<Ascii>, Error> {
    if !response.error.is_empty() || response.token.is_empty() {
        return Err(Error::LoginFailure);
    }
    Ok(response.token.parse()?)
}

async fn authenticate_async(username: &str, password: &str) -> Result<(), Error> {
    let _login = LOGIN_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
    let mut attempt = AuthenticationAttempt { completed: false };
    *CREDS.lock().unwrap() = Some((username.to_owned(), password.to_owned()));
    let result = async {
        let mut client = connect!(UsersClient);
        let mode = hooks_config::get().ok_or(Error::LoginFailure)?.gameplay_mode;
        let release = crate::release_identity::declaration().map_err(Error::IO)?;
        let request = game_login_request(username, password, mode, current_game_instance()?, release)?;
        login_response_token(client.login(request).await?.into_inner())
    }.await;
    let result = match result {
        Ok(token) => {
            *TOKEN.lock().unwrap() = Some(token);
            info!("Game identity login acknowledged");
            Ok(())
        }
        Err(error) => {
            *TOKEN.lock().unwrap() = None;
            Err(error)
        }
    };
    attempt.completed = true;
    result
}

async fn bounded_native_login(future: impl Future<Output=Result<(), Error>>, deadline: std::time::Duration) -> Result<(), Error> {
    tokio::time::timeout(deadline, future).await.map_err(|_| Error::IO(std::io::Error::new(std::io::ErrorKind::TimedOut, "Native identity login timed out")))?
}

/// Called at the game's actual credential export, never from DllMain.
/// Refresh even with a cached token: after a server restart an old token alone
/// does not prove that the per-process game identity exists on that server.
/// Room republication needs a native connection, so is NOT part of this gate.
pub fn ensure_native_login() -> Result<(), Error> {
    let config = hooks_config::get().ok_or(Error::LoginFailure)?;
    if tokio::runtime::Handle::try_current().is_ok() {
        // Do not panic or recursively block an async executor from an FFI call.
        return Err(Error::LoginFailure);
    }
    run(bounded_native_login(authenticate_async(&config.user.username, &config.user.password), NATIVE_LOGIN_DEADLINE))
}

async fn login_async(username: &str, password: &str) -> Result<(), Error> {
    authenticate_async(username, password).await?;
    republish_local_game_session().await;
    Ok(())
}

#[cfg(test)]
mod native_login_tests {
    use super::*;
    #[test]
    fn empty_error_and_invalid_tokens_are_not_successful_login() {
        for (error, token) in [("", ""), ("denied", "fixture-token"), ("", "bad\ntoken")] {
            assert!(login_response_token(server_api::users::LoginResponse{error:error.into(),token:token.into(),user:None}).is_err());
        }
        assert!(login_response_token(server_api::users::LoginResponse{error:String::new(),token:"fixture-token".into(),user:None}).is_ok());
    }
    #[test]
    fn native_wait_has_a_deadline_and_drops_cancelled_work() {
        let rt=tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use std::sync::atomic::{AtomicBool, Ordering};
            struct Cancel<'a>(&'a AtomicBool);
            impl Drop for Cancel<'_> {fn drop(&mut self){self.0.store(true,Ordering::SeqCst);}}
            let cancelled=AtomicBool::new(false);
            let operation=async {
                let _guard=Cancel(&cancelled);
                std::future::pending::<Result<(),Error>>().await
            };
            assert!(matches!(bounded_native_login(operation, std::time::Duration::from_millis(10)).await,Err(Error::IO(error)) if error.kind()==std::io::ErrorKind::TimedOut));
            assert!(cancelled.load(Ordering::SeqCst));
            assert!(bounded_native_login(async {Ok(())},std::time::Duration::from_millis(10)).await.is_ok());
            assert!(matches!(bounded_native_login(async {Err(Error::LoginFailure)},std::time::Duration::from_millis(10)).await,Err(Error::LoginFailure)));
        });
    }
}

#[instrument(skip(password))]
pub fn login(username: &str, password: &str) -> Result<(), Error> {
    let result = run(login_async(username, password));
    if let Err(error) = &result {
        error!("Initial login failed: {error}");
    }
    result
}

#[instrument]
pub async fn event() -> Result<EventResponse, Error> {
    let mut client = connect!(MiscClient);
    let request = tonic::Request::new(EventRequest {});
    let mut response = client.event(request).await?.into_inner();

    // Party-follow invitations must reach the overlay. The server marks them
    // force_join, and the overlay accepts them once before injecting the same
    // proven game-invitation event used by direct room and lobby invitations.
    if let Some(mut invite) = response.invite.take() {
        let server_id = invite.id;
        let local_id = invite_id_state().lock().unwrap().map_server_event(server_id);
        if let Some(local_id) = local_id {
            info!("InvitePollDelivered server_id={server_id} local_id={local_id} kind={}", invite.kind);
            invite.id = local_id;
            response.invite = Some(invite);
        }
    }
    retry_pending_game_session();
    Ok(response)
}

#[instrument]
pub async fn relogin() -> bool {
    let (username, password) = {
        let Some((username, password)) = CREDS.lock().unwrap().clone() else {
            error!("Relogin skipped because credentials are unavailable");
            return false;
        };
        (username, password)
    };
    match login_async(&username, &password).await {
        Ok(()) => {
            info!("Relogin successful");
            true
        }
        Err(error) => {
            error!("Relogin failed: {error}");
            false
        }
    }
}

#[instrument]
pub async fn reconnect_and_relogin() -> bool {
    reset_connection("polling failure or system resume");
    relogin().await
}

#[cfg(test)]
mod tests {

    use super::invite_accept_result_is_terminal;
    use super::invite_connection_retryable;
    use super::Error;
    use super::InviteIdState;

    #[test]
    fn invite_retry_is_limited_to_connection_failures() {
        assert!(invite_connection_retryable(&Error::GRPCStatus(tonic::Status::unavailable("connect error"))));
        assert!(!invite_connection_retryable(&Error::GRPCStatus(tonic::Status::not_found("friend"))));
        assert!(!invite_connection_retryable(&Error::GRPCStatus(tonic::Status::failed_precondition("room"))));
        assert!(!invite_connection_retryable(&Error::GRPCStatus(tonic::Status::already_exists("room"))));
    }

    #[test]
    fn repeated_delivery_reuses_local_id_until_acceptance() {
        let mut state = InviteIdState::new();
        let first = state.map_server_event(1).unwrap();
        assert_eq!(Some(first), state.map_server_event(1));
        assert_eq!(state.server_id_for_local(first), 1);
    }

    #[test]
    fn terminal_server_id_is_not_reissued_by_late_poll() {
        let mut state = InviteIdState::new();
        let first = state.map_server_event(1).unwrap();
        assert!(state.retire_local(first));
        assert_eq!(state.map_server_event(1), None);
        let second = state.map_server_event(2).unwrap();
        assert_ne!(first, second);
        assert_eq!(state.server_id_for_local(second), 2);
        assert_eq!(Some(second), state.map_server_event(2));
    }

    #[test]
    fn only_first_terminal_completion_owns_the_local_generation() {
        let mut state = InviteIdState::new();
        let first = state.map_server_event(1).unwrap();
        let second = state.map_server_event(2).unwrap();
        assert!(state.is_pending_local(first));
        assert!(state.retire_local(first));
        assert!(!state.retire_local(first));
        assert!(!state.is_pending_local(first));
        assert!(state.is_pending_local(second));
        assert!(state.is_pending_local(42));
        assert!(!state.is_pending_local(-9000));
    }

    #[test]
    fn only_terminal_accept_results_retire_the_local_generation() {
        assert!(invite_accept_result_is_terminal(&Ok(true)));
        assert!(invite_accept_result_is_terminal(&Ok(false)));
        assert!(invite_accept_result_is_terminal(&Err(Error::GRPCStatus(tonic::Status::not_found("expired")))));
        assert!(!invite_accept_result_is_terminal(&Err(Error::GRPCStatus(tonic::Status::unavailable("retry")))));
    }



}


#[cfg(test)]
mod connection_cache_tests {
    use super::ConnectionCache;

    #[test]
    fn first_publish_is_reused_within_one_epoch() {
        let mut cache = ConnectionCache::new();
        let (epoch, value) = cache.snapshot();
        assert_eq!(value, None);
        assert_eq!(cache.publish(&epoch, 1), Some(1));
        assert_eq!(cache.publish(&epoch, 2), Some(1));
        assert_eq!(cache.snapshot().1, Some(1));
    }

    #[test]
    fn resetting_empty_cache_retires_pending_creators() {
        let mut cache = ConnectionCache::new();
        let (old, _) = cache.snapshot();
        assert_eq!(cache.reset(), None);
        assert_eq!(cache.publish(&old, 1), None);
        let (new, value) = cache.snapshot();
        assert_eq!(value, None);
        assert_eq!(cache.publish(&new, 2), Some(2));
    }

    #[test]
    fn old_completion_cannot_replace_new_epoch_winner() {
        let mut cache = ConnectionCache::new();
        let (old, _) = cache.snapshot();
        cache.reset();
        let (new, _) = cache.snapshot();
        assert_eq!(cache.publish(&new, 2), Some(2));
        assert_eq!(cache.publish(&old, 1), None);
        assert_eq!(cache.snapshot().1, Some(2));
    }

    #[test]
    fn reset_returns_old_handle_without_retaining_it() {
        let mut cache = ConnectionCache::new();
        let (old, _) = cache.snapshot();
        assert_eq!(cache.publish(&old, 1), Some(1));
        assert_eq!(cache.reset(), Some(1));
        assert_eq!(cache.snapshot().1, None);
        assert_eq!(cache.reset(), None);
    }

    #[test]
    fn repeated_resets_do_not_revalidate_retained_epochs() {
        let mut cache = ConnectionCache::new();
        let mut old_epochs = Vec::new();
        for _ in 0..128 {
            old_epochs.push(cache.snapshot().0);
            cache.reset();
        }
        for old in old_epochs {
            assert_eq!(cache.publish(&old, 1), None);
        }
        let (new, _) = cache.snapshot();
        assert_eq!(cache.publish(&new, 2), Some(2));
    }

    #[test]
    fn cloned_epoch_keeps_the_same_identity() {
        let mut cache = ConnectionCache::new();
        let (epoch, _) = cache.snapshot();
        assert_eq!(cache.publish(&epoch.clone(), 3), Some(3));
        assert_eq!(cache.publish(&epoch, 4), Some(3));
    }
}

