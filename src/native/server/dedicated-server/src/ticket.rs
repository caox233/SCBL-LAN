use std::sync::{Arc, atomic::{AtomicU32, Ordering}};

use quazal::kerberos::KerberosTicket;
use quazal::kerberos::KerberosTicketInternal;
use quazal::kerberos::SESSION_KEY_SIZE;
use quazal::prudp::ClientRegistry;
use quazal::rmc::types::QResult;
use quazal::rmc::types::StationURL;
use quazal::rmc::Protocol;
use quazal::Context;
use rand::TryRngCore;

pub use crate::protocols::authentication_foundation::ticket_granting_protocol::*;
pub use crate::protocols::authentication_foundation::types::*;
use crate::protocols::ubi_authentication::types::UbiAuthenticationLoginCustomData;
use crate::storage::{AuthIdentity, Storage};
use crate::SERVER_PID;

/// Implementation of the `TicketGrantingProtocolServerTrait` for handling ticket-granting requests.
struct TicketGrantingProtocolServerImpl {
    storage: Arc<Storage>,
}

// A failed key/store operation must not advertise an unauthenticated request
// as the transport's authenticated user. No authorization gate is relaxed.
fn bind_after_ticket<T, E>(bound_user: &mut Option<u32>, requested_user: u32,
    issue: impl FnOnce() -> std::result::Result<T, E>) -> std::result::Result<T, E>
{
    let ticket = issue()?;
    *bound_user = Some(requested_user);
    Ok(ticket)
}
static PLAIN_LOGIN_DIAGNOSTICS: AtomicU32 = AtomicU32::new(0);

impl TicketGrantingProtocolServerImpl {
    /// Generates a new session key for a user and stores it in the database.
    fn get_session_key(&self, logger:&slog::Logger, user_id:u32, source:std::net::IpAddr, verified:Option<&AuthIdentity>)->quazal::rmc::Result<[u8;SESSION_KEY_SIZE]> {
        let _generation=self.storage.mode_gate.blocking_read();
        if let Some(identity) = verified {
            if identity.user_id != user_id || !self.storage.auth_identity_is_current(identity)
                .map_err(|error| { error!(logger, "Login identity check failed: {error}"); quazal::rmc::Error::InternalError })? {
                return Err(quazal::rmc::Error::AccessDenied);
            }
        }
        let mut key=[0u8;SESSION_KEY_SIZE];
        rand::rngs::OsRng.try_fill_bytes(&mut key).map_err(|_|quazal::rmc::Error::InternalError)?;
        self.storage.create_bound_user_session(user_id,&key,source).map_err(|e|{error!(logger,"Error saving native ticket: {e}");quazal::rmc::Error::InternalError})?;
        Ok(key)
    }

    /// Retrieves the password for a user by their principal ID (PID).
    #[allow(unreachable_code)]
    fn get_password_by_pid(&self, logger: &slog::Logger, pid: u32) -> quazal::rmc::Result<Option<String>> {
        self.storage.find_password_for_user(pid).map_err(|e| {
            eprintln!("Error finding user password: {e}");
            error!(logger, "Error finding user password: {e}");
            quazal::rmc::Error::InternalError
        })
    }

    /// Retrieves the password for a user by their username.
    fn get_password_by_username(&self, logger: &slog::Logger, username: &str) -> quazal::rmc::Result<Option<String>> {
        Ok(self
            .get_pid_by_username(logger, username)?
            .map(|uid| self.get_password_by_pid(logger, uid))
            .transpose()?
            .flatten())
    }

    /// Retrieves the principal ID (PID) for a user by their username.
    #[allow(unreachable_code)]
    fn get_pid_by_username(&self, logger: &slog::Logger, username: &str) -> quazal::rmc::Result<Option<u32>> {
        self.storage.find_user_id_by_name(username).map_err(|e| {
            eprintln!("Error finding user password: {e}");
            error!(logger, "Error finding user password: {e}");
            quazal::rmc::Error::InternalError
        })
    }

    /// Authenticates a user with their username and password.
    #[allow(unreachable_code)]
    fn login(&self, logger: &slog::Logger, username: &str, password: &str) -> quazal::rmc::Result<Option<AuthIdentity>> {
        self.storage
            .authenticate_user(username, password)
            .map_err(|e| {
                eprintln!("Error finding user password: {e}");
                error!(logger, "Error finding user password: {e}");
                quazal::rmc::Error::InternalError
            })
            .map(Result::ok)
    }
}

/// Creates the `RVConnectionData` for a client.
fn get_connection_data(ctx: &Context, pid: u32) -> RVConnectionData {
    ctx.secure_server_addr.map_or_else(
        || RVConnectionData {
            url_regular_protocols: format!("prudp:/address={};port={};CID=1;PID={};sid=2;stream=3;type=2", ctx.listen.ip(), ctx.listen.port(), pid)
                .parse()
                .unwrap(),
            lst_special_protocols: vec![],
            url_special_protocols: StationURL::default(),
        },
        |a| RVConnectionData {
            url_regular_protocols: format!(
                // CID => ConnectionID
                // PID => PrincipalID
                // RVCID => RVConnectionID
                // sid => StreamID
                // stream => StreamType
                "prudps:/address={};port={};CID=1;PID={};sid=1;stream=3;type=2",
                a.ip(),
                a.port(),
                pid
            )
            .parse()
            .unwrap(),
            lst_special_protocols: vec![],
            url_special_protocols: StationURL::default(),
        },
    )
}

impl<T> TicketGrantingProtocolServerTrait<T> for TicketGrantingProtocolServerImpl {
    fn login(
        &self,
        logger: &slog::Logger,
        ctx: &Context,
        ci: &mut quazal::ClientInfo<T>,
        request: LoginRequest,
        _client_registry: &ClientRegistry<T>,
        _socket: &std::net::UdpSocket,
    ) -> Result<LoginResponse, quazal::rmc::Error> {
        self.login_core(logger, ctx, ci, request)
    }

    fn login_ex(
        &self,
        logger: &slog::Logger,
        ctx: &Context,
        ci: &mut quazal::ClientInfo<T>,
        request: LoginExRequest,
        _client_registry: &ClientRegistry<T>,
        _socket: &std::net::UdpSocket,
    ) -> Result<LoginExResponse, quazal::rmc::Error> {
        self.login_ex_core(logger, ctx, ci, request)
    }

    fn request_ticket(
        &self,
        logger: &slog::Logger,
        ctx: &Context,
        ci: &mut quazal::ClientInfo<T>,
        request: RequestTicketRequest,
        _client_registry: &ClientRegistry<T>,
        _socket: &std::net::UdpSocket,
    ) -> Result<RequestTicketResponse, quazal::rmc::Error> {
        self.request_ticket_core(logger, ctx, ci, request)
    }
}

/// Creates a new boxed `TicketGrantingProtocolServer` instance.
///
/// This function is typically used to register the ticket-granting protocol
/// with the server's protocol dispatcher.
pub fn new_protocol<T: 'static>(storage: Arc<Storage>) -> Box<dyn Protocol<T>> {
    Box::new(TicketGrantingProtocolServer::new(TicketGrantingProtocolServerImpl { storage }))
}

#[cfg(test)]
mod ticket_binding_commit_tests {
    use super::bind_after_ticket;

    #[test]
    fn failed_plain_ticket_does_not_claim_or_replace_transport_user() {
        for initial in [None, Some(7)] {
            let mut bound = initial;
            let mut calls = 0;
            let result: Result<u8, &str> = bind_after_ticket(&mut bound, 11, || {
                calls += 1;
                Err("approved run missing")
            });
            assert!(result.is_err());
            assert_eq!(calls, 1);
            assert_eq!(bound, initial);
        }
    }

    #[test]
    fn successful_ticket_commits_exactly_after_issue() {
        let mut bound = Some(7);
        let mut calls = 0;
        let result: Result<u8, &str> = bind_after_ticket(&mut bound, 11, || {
            calls += 1;
            Ok(42)
        });
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls, 1);
        assert_eq!(bound, Some(11));
    }

    #[test]
    fn actual_login_handlers_preserve_transport_binding_on_missing_run() {
        use super::{TicketGrantingProtocolServerImpl, TicketGrantingProtocolServerTrait};
        use super::{LoginExRequest, LoginRequest, UbiAuthenticationLoginCustomData};
        use quazal::rmc::types::{Any, Data};
        use quazal::rmc::basic::ToStream;

        // All data is synthetic and confined to a fresh temporary SQLite fixture.
        let (storage, pool) = crate::storage::release_test_support::fixture();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            storage.register_user_with_recovery_async("ticket-approved-fixture", "fixture-only-password",
                Some("ticket-approved-id"), None).await.unwrap();
            storage.register_user_with_recovery_async("ticket-probe-fixture", "fixture-only-password",
                Some("ticket-probe-id"), None).await.unwrap();
        });
        let approved = storage.find_user_id_by_name("ticket-approved-fixture").unwrap().unwrap();
        let probe = storage.find_user_id_by_name("ticket-probe-fixture").unwrap().unwrap();
        // Exercise the real authenticated API/run path, not a fabricated row.
        assert_eq!(crate::api::maintenance_authenticated_game_login(storage.clone(),
            "ticket-approved-fixture", "scbl-lan/2/game/mod",
            "dddddddddddddddddddddddddddddddd"), approved);

        let logger = slog::Logger::root(slog::Discard, slog::o!());
        let ctx = quazal::Context::splinter_cell_blacklist();
        let clients = quazal::prudp::ClientRegistry::<()>::default();
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let service = TicketGrantingProtocolServerImpl { storage };
        let mut ci = quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
        let login_ex = |name: &str| {
            let data = UbiAuthenticationLoginCustomData {
                data: Data, user_name: name.into(), online_key: "fixture-only-online-key".into(),
                password: "fixture-only-password".into(),
            };
            LoginExRequest { str_user_name: name.into(),
                o_extra_data: Any::new("UbiAuthenticationLoginCustomData".into(), data.to_bytes()) }
        };

        let approved_result = <TicketGrantingProtocolServerImpl as TicketGrantingProtocolServerTrait<()>>::login_ex(
            &service, &logger, &ctx, &mut ci, login_ex("ticket-approved-fixture"), &clients, &socket);
        assert!(approved_result.is_ok());
        assert_eq!(ci.user_id, Some(approved));

        let rejected_plain = <TicketGrantingProtocolServerImpl as TicketGrantingProtocolServerTrait<()>>::login(
            &service, &logger, &ctx, &mut ci,
            LoginRequest { str_user_name: "ticket-probe-fixture".into() }, &clients, &socket);
        assert!(matches!(rejected_plain, Err(quazal::rmc::Error::InternalError)));
        assert_eq!(ci.user_id, Some(approved), "failed plain Login must not replace prior LoginEx binding");

        let rejected_ex = <TicketGrantingProtocolServerImpl as TicketGrantingProtocolServerTrait<()>>::login_ex(
            &service, &logger, &ctx, &mut ci, login_ex("ticket-probe-fixture"), &clients, &socket);
        assert!(matches!(rejected_ex, Err(quazal::rmc::Error::InternalError)));
        assert_eq!(ci.user_id, Some(approved), "failed LoginEx must not replace prior binding");

        let mut fresh = quazal::ClientInfo::<()>::new("127.0.0.1:30501".parse().unwrap());
        let rejected_fresh = <TicketGrantingProtocolServerImpl as TicketGrantingProtocolServerTrait<()>>::login(
            &service, &logger, &ctx, &mut fresh,
            LoginRequest { str_user_name: "ticket-probe-fixture".into() }, &clients, &socket);
        assert!(matches!(rejected_fresh, Err(quazal::rmc::Error::InternalError)));
        assert_eq!(fresh.user_id, None, "failed fresh Login must not authenticate the transport");
        assert_ne!(approved, probe);
        let probe_sessions: i64 = runtime.block_on(async {
            sqlx::query_scalar("SELECT COUNT(*) FROM user_sessions WHERE user_id=?")
                .bind(probe).fetch_one(&pool).await.unwrap()
        });
        assert_eq!(probe_sessions, 0, "denied probe received no ticket-backed session");
    }
}

impl TicketGrantingProtocolServerImpl {
    fn login_core<T>(&self, logger: &slog::Logger, ctx: &Context, ci: &mut quazal::ClientInfo<T>, request: LoginRequest) -> Result<LoginResponse, quazal::rmc::Error> {
        let Some(user_id) = self.get_pid_by_username(logger, &request.str_user_name)? else {
            warn!(logger, "Native login account not found");
            return Err(quazal::rmc::Error::AccessDenied);
        };
        let password = self.get_password_by_username(logger, &request.str_user_name)?.or_else(|| {
            warn!(logger, "Native login has no legacy password");
            None
        });
        let previous_user = ci.user_id;
        let source = ci.address().ip();
        let session_key = match bind_after_ticket(&mut ci.user_id, user_id,
            || self.get_session_key(logger, user_id, source, None))
        {
            Ok(key) => key,
            Err(error) => {
                // No username, password, user ID, address, session ID or ticket
                // in this bounded diagnostic. It distinguishes a fresh probe
                // from a request for another user on an authenticated transport.
                if PLAIN_LOGIN_DIAGNOSTICS.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
                    |count| (count < 24).then_some(count + 1)).is_ok() {
                    info!(logger, "SCBL_NATIVE_LOGIN method=plain ticket_issue_failed=true prior_authenticated={} requested_matches_prior={} binding_preserved={}",
                        previous_user.is_some(), previous_user == Some(user_id), ci.user_id == previous_user);
                }
                return Err(error);
            }
        };
        let ticket = KerberosTicket {
            session_key,
            pid: SERVER_PID,
            internal: KerberosTicketInternal {
                principle_id: user_id,
                valid_until: u64::MAX,
                session_key,
            },
        };
        Ok(LoginResponse {
            return_value: QResult::Ok,
            pid_principal: user_id,
            pbuf_response: ticket.as_bytes(user_id, password.as_deref(), &ctx.ticket_key),
            p_connection_data: get_connection_data(ctx, 2),
            str_return_msg: String::new(),
        })
    }
    fn login_ex_core<T>(&self, logger: &slog::Logger, ctx: &Context, ci: &mut quazal::ClientInfo<T>, request: LoginExRequest) -> Result<LoginExResponse, quazal::rmc::Error> {
        let _username = request.str_user_name;
        let mut registry = quazal::rmc::types::ClassRegistry::default();
        registry.register_class::<UbiAuthenticationLoginCustomData>("UbiAuthenticationLoginCustomData");
        let ubi_data = request.o_extra_data.into_inner(&registry)?;
        let ubi_data: Option<&UbiAuthenticationLoginCustomData> = ubi_data.as_any().downcast_ref();
        let Some(UbiAuthenticationLoginCustomData {
            user_name: ubi_username,
            password,
            ..
        }) = ubi_data
        else {
            error!(logger, "Error parsing UbiAuthenticationLoginCustomData");
            return Err(quazal::rmc::Error::ParsingError);
        };

        info!(logger, "Native LoginEx attempt");

        let Some(identity) = self.login(logger, ubi_username, password)? else {
            warn!(logger, "Native LoginEx authentication failed");
            return Err(quazal::rmc::Error::AccessDenied);
        };
        let user_id = identity.user_id;
        info!(logger, "Native LoginEx authentication succeeded");

        #[cfg(test)]
        self.storage.run_login_checkpoint("publication");
        let source = ci.address().ip();
        let session_key = bind_after_ticket(&mut ci.user_id, user_id,
            || self.get_session_key(logger, user_id, source, Some(&identity)))?;
        let ticket = KerberosTicket {
            session_key,
            pid: SERVER_PID,
            internal: KerberosTicketInternal {
                principle_id: user_id,
                valid_until: u64::MAX,
                session_key,
            },
        };
        Ok(LoginExResponse {
            return_value: QResult::Ok,
            pid_principal: user_id,
            pbuf_response: ticket.as_bytes(user_id, None, &ctx.ticket_key),
            p_connection_data: get_connection_data(ctx, SERVER_PID),
            str_return_msg: String::new(),
        })
    }
    fn request_ticket_core<T>(&self, logger: &slog::Logger, ctx: &Context, ci: &mut quazal::ClientInfo<T>, request: RequestTicketRequest) -> Result<RequestTicketResponse, quazal::rmc::Error> {
        let user_id = request.id_source;
        let server_id = request.id_target;
        if !matches!(ci.user_id, Some(uid) if uid == user_id) {
            warn!(logger, "Native ticket request denied: transport identity mismatch");
            return Err(quazal::rmc::Error::AccessDenied);
        }
        let session_key = self.get_session_key(logger, user_id, ci.address().ip(), None)?;
        let ticket = KerberosTicket {
            session_key,
            pid: server_id,
            internal: KerberosTicketInternal {
                principle_id: user_id,
                valid_until: u64::MAX,
                session_key,
            },
        };
        Ok(RequestTicketResponse {
            return_value: QResult::Ok,
            buf_response: ticket.as_bytes(user_id, self.get_password_by_pid(logger, user_id)?.as_deref(), &ctx.ticket_key),
        })
    }
}

/// Test-only entry to complete production method bodies. No socket or listener.
#[cfg(test)]
pub(crate) fn maintenance_ticket_core(
    storage: Arc<Storage>, method: &str, username: &str, password: &str,
    user_id: u32, bound: Option<u32>,
) -> (quazal::rmc::Result<()>, Option<u32>) {
    use quazal::rmc::basic::ToStream;
    use quazal::rmc::types::{Any, Data};
    let service = TicketGrantingProtocolServerImpl { storage };
    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let ctx = Context::splinter_cell_blacklist();
    let mut ci = quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
    ci.user_id = bound;
    let result = match method {
        "Login" => service.login_core(&logger, &ctx, &mut ci, LoginRequest { str_user_name: username.into() }).map(|_| ()),
        "LoginEx" => {
            let data = UbiAuthenticationLoginCustomData { data: Data, user_name: username.into(), online_key: "fixture-only".into(), password: password.into() };
            service.login_ex_core(&logger, &ctx, &mut ci, LoginExRequest { str_user_name: username.into(), o_extra_data: Any::new("UbiAuthenticationLoginCustomData".into(), data.to_bytes()) }).map(|_| ())
        },
        "RequestTicket" => service.request_ticket_core(&logger, &ctx, &mut ci, RequestTicketRequest { id_source: user_id, id_target: SERVER_PID }).map(|_| ()),
        _ => panic!("unknown fixture method"),
    };
    (result, ci.user_id)
}

#[cfg(test)]
#[path = "native_identity_producer_tests.rs"]
pub(crate) mod native_identity_producer_tests;
