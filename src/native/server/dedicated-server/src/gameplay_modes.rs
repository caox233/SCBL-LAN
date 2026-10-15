//! One authoritative compatibility boundary around native room protocols.
//! Private-room filters and the native business handlers remain unchanged.
use crate::protocols::game_session_ex_service::game_session_ex_protocol as extra;
use crate::protocols::game_session_service::game_session_protocol as game;
use crate::protocols::game_session_service::types::GameSessionKey;
use crate::storage::Storage;
use quazal::prudp::ClientRegistry;
use quazal::rmc::basic::{FromStream, ToStream};
use quazal::rmc::{Error, Protocol, Request};
use quazal::{ClientInfo, Context};
use server_api::gameplay_contract::{ClientContract, GameplayMode};
use std::sync::Arc;

pub(crate) fn mode_code(product: &str) -> Option<i64> {
    match ClientContract::parse(product) {
        Some(ClientContract::Game(GameplayMode::Official)) => Some(1),
        Some(ClientContract::Game(GameplayMode::Mod)) => Some(2),
        _ => None,
    }
}
struct ModeProtocol<T> {
    storage: Arc<Storage>,
    inner: Box<dyn Protocol<T>>,
}
pub(crate) fn protect<T: 'static>(
    storage: Arc<Storage>,
    inner: Box<dyn Protocol<T>>,
) -> Box<dyn Protocol<T>> {
    Box::new(ModeProtocol { storage, inner })
}
impl<T> Protocol<T> for ModeProtocol<T> {
    fn id(&self) -> u16 {
        self.inner.id()
    }
    fn name(&self) -> String {
        self.inner.name()
    }
    fn num_methods(&self) -> u32 {
        self.inner.num_methods()
    }
    fn method_name(&self, id: u32) -> Option<String> {
        self.inner.method_name(id)
    }
    fn handle(
        &self,
        logger: &slog::Logger,
        ctx: &Context,
        ci: &mut ClientInfo<T>,
        request: &Request,
        clients: &ClientRegistry<T>,
        socket: &std::net::UdpSocket,
    ) -> Result<Vec<u8>, Error> {
        // PRUDP handlers run on their dedicated synchronous server thread. Login
        // takes the write side, preventing a run switch halfway through admission.
        let _generation = self.storage.mode_gate.blocking_read();
        let user = crate::login_required(ci)?;
        let run = self
            .storage
            .native_game_run(user, ci.ticket_fingerprint, ci.address().ip())
            .map_err(|_| Error::InternalError)?
            .ok_or(Error::AccessDenied)?;
        let matches = |key: &GameSessionKey| {
            self.storage
                .room_matches_mode(key.type_id, key.session_id, run.mode)
                .map_err(|_| Error::InternalError)
        };
        if self.id() == 42 {
            // All implemented keyed mutation paths start with GameSessionKey;
            // UpdateSession wraps that same key first in GameSessionUpdate.
            if matches!(request.method_id, 2 | 3 | 4 | 5 | 8 | 9 | 22 | 23 | 27 | 28) {
                let key = GameSessionKey::from_bytes(&request.parameters)
                    .map_err(|_| Error::ParsingError)?;
                if !matches(&key)? {
                    return Err(Error::AccessDenied);
                }
            }
            if request.method_id == 8 {
                let add = game::AddParticipantsRequest::from_bytes(&request.parameters)
                    .map_err(|_| Error::ParsingError)?;
                for id in add
                    .public_participant_ids
                    .0
                    .iter()
                    .chain(&add.private_participant_ids.0)
                {
                    let mode = crate::storage::mode_current_sync(&self.storage, *id)
                        .map_err(|_| Error::InternalError)?;
                    if mode != Some(run.mode) {
                        return Err(Error::AccessDenied);
                    }
                }
            }
        }
        let bytes = self
            .inner
            .handle(logger, ctx, ci, request, clients, socket)?;
        if self.id() == 123 && request.method_id == 1 {
            let mut response = extra::SearchSessionsResponse::from_bytes(&bytes)
                .map_err(|_| Error::InternalError)?;
            let mut keep = Vec::new();
            for item in response.search_results.0 {
                if matches(&item.game_session_search_result.session_key)? {
                    keep.push(item);
                }
            }
            response.search_results.0 = keep;
            return Ok(response.try_to_bytes()?);
        }
        if self.id() == 42 && request.method_id == 24 {
            let mut response = game::SearchSessionsWithParticipantsResponse::from_bytes(&bytes)
                .map_err(|_| Error::InternalError)?;
            let mut keep = Vec::new();
            for item in response.search_results.0 {
                if matches(&item.game_session_search_result.session_key)? {
                    keep.push(item);
                }
            }
            response.search_results.0 = keep;
            return Ok(response.try_to_bytes()?);
        }
        Ok(bytes)
    }
}
