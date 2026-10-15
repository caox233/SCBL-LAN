//! Implements the `GameSessionProtocolServer` for managing game sessions.

use std::net::IpAddr;
use std::sync::Arc;

use quazal::prudp::ClientRegistry;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::ClientInfo;
use quazal::Context;
use sc_bl_protocols::game_session_service::game_session_protocol::JoinSessionRequest;
use sc_bl_protocols::game_session_service::game_session_protocol::JoinSessionResponse;
use sc_bl_protocols::game_session_service::game_session_protocol::RemoveParticipantsRequest;
use sc_bl_protocols::game_session_service::game_session_protocol::RemoveParticipantsResponse;
use slog::Logger;

use crate::login_required;
use crate::protocols::game_session_service::game_session_protocol::AbandonSessionRequest;
use crate::protocols::game_session_service::game_session_protocol::AbandonSessionResponse;
use crate::protocols::game_session_service::game_session_protocol::AddParticipantsRequest;
use crate::protocols::game_session_service::game_session_protocol::AddParticipantsResponse;
use crate::protocols::game_session_service::game_session_protocol::CreateSessionRequest;
use crate::protocols::game_session_service::game_session_protocol::CreateSessionResponse;
use crate::protocols::game_session_service::game_session_protocol::DeleteSessionRequest;
use crate::protocols::game_session_service::game_session_protocol::DeleteSessionResponse;
use crate::protocols::game_session_service::game_session_protocol::GameSessionProtocolServer;
use crate::protocols::game_session_service::game_session_protocol::GameSessionProtocolServerTrait;
use crate::protocols::game_session_service::game_session_protocol::LeaveSessionRequest;
use crate::protocols::game_session_service::game_session_protocol::LeaveSessionResponse;
use crate::protocols::game_session_service::game_session_protocol::MigrateSessionHostRequest;
use crate::protocols::game_session_service::game_session_protocol::MigrateSessionHostResponse;
use crate::protocols::game_session_service::game_session_protocol::MigrateSessionRequest;
use crate::protocols::game_session_service::game_session_protocol::MigrateSessionResponse;
use crate::protocols::game_session_service::game_session_protocol::RegisterUrLsRequest;
use crate::protocols::game_session_service::game_session_protocol::RegisterUrLsResponse;
use crate::protocols::game_session_service::game_session_protocol::ReportUnsuccessfulJoinSessionsRequest;
use crate::protocols::game_session_service::game_session_protocol::ReportUnsuccessfulJoinSessionsResponse;
use crate::protocols::game_session_service::game_session_protocol::SearchSessionsWithParticipantsRequest;
use crate::protocols::game_session_service::game_session_protocol::SearchSessionsWithParticipantsResponse;
use crate::protocols::game_session_service::game_session_protocol::SplitSessionRequest;
use crate::protocols::game_session_service::game_session_protocol::SplitSessionResponse;
use crate::protocols::game_session_service::game_session_protocol::UpdateSessionRequest;
use crate::protocols::game_session_service::game_session_protocol::UpdateSessionResponse;
use crate::protocols::game_session_service::types::GameSessionKey;
use crate::protocols::game_session_service::types::GameSessionSearchResult;
use crate::protocols::game_session_service::types::GameSessionSearchWithParticipantsResult;
use crate::storage::DeleteGameSessionOutcome;
use crate::storage::Storage;
use crate::storage::INVITE_KIND_LOBBY_PARTY;
use crate::storage::INVITE_KIND_LOBBY_RESTORE;
use crate::storage::INVITE_KIND_PARTY_FOLLOW;
use crate::storage::INVITE_KIND_PRIVATE_ROOM;

struct GameSessionProtocolServerImpl {
    storage: Arc<Storage>,
}

fn observed_private_ipv4_override(registered_address: &str, observed_address: IpAddr) -> Option<String> {
    let IpAddr::V4(observed_address) = observed_address else {
        return None;
    };
    if observed_address.is_unspecified() || observed_address.is_multicast() || observed_address.is_broadcast() {
        return None;
    }
    let Ok(IpAddr::V4(registered_address)) = registered_address.parse::<IpAddr>() else {
        return None;
    };
    (registered_address != observed_address).then(|| observed_address.to_string())
}

fn log_session_capacity(logger: &Logger, storage: &Storage, event: &str, type_id: u32, session_id: u32, user_id: Option<u32>) {
    match storage.game_session_diagnostics(type_id, session_id) {
        Ok(Some((creator_id, attributes, participant_ids))) => {
            info!(
                logger,
                "SCBL_SESSION_CAPACITY event={} type_id={} session_id={} user_id={:?} creator_id={} participant_count={} participant_ids={:?} attributes={}",
                event,
                type_id,
                session_id,
                user_id,
                creator_id,
                participant_ids.len(),
                participant_ids,
                attributes
            );
        }
        Ok(None) => {
            warn!(
                logger,
                "SCBL_SESSION_CAPACITY event={} type_id={} session_id={} user_id={:?} session_not_found=true", event, type_id, session_id, user_id
            );
        }
        Err(error) => {
            warn!(
                logger,
                "SCBL_SESSION_CAPACITY event={} type_id={} session_id={} user_id={:?} diagnostic_error={}", event, type_id, session_id, user_id, error
            );
        }
    }
}

fn split_session_result_allowed(migrated: bool, restored: bool) -> bool {
    migrated || restored
}

fn participant_search_matches_invited_session(requested: &[u32], invited: &[u32]) -> bool {
    requested.iter().any(|requested_id| invited.contains(requested_id))
}

fn invitation_can_route_participant_search(kind: i32) -> bool {
    matches!(
        kind,
        INVITE_KIND_PRIVATE_ROOM | INVITE_KIND_LOBBY_PARTY | INVITE_KIND_PARTY_FOLLOW | INVITE_KIND_LOBBY_RESTORE
    )
}

impl<CI> GameSessionProtocolServerTrait<CI> for GameSessionProtocolServerImpl {
    fn create_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: CreateSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<CreateSessionResponse, Error> {
        info!(logger, "Client creates session: {:?}", request);
        let user_id = login_required(&*ci)?;
        let attributes = request
            .game_session
            .attributes
            .0
            .into_iter()
            .map(|property| format!("{} => {}", property.id, property.value))
            .collect::<Vec<_>>()
            .join(";");
        let session_type = request.game_session.type_id;
        let session_id = rmc_err!(self.storage.create_game_session(user_id, session_type, attributes), logger, "error creating game session")?;
        log_session_capacity(logger, &self.storage, "create", session_type, session_id, Some(user_id));
        Ok(CreateSessionResponse {
            game_session_key: GameSessionKey {
                type_id: session_type,
                session_id,
            },
        })
    }

    fn update_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: UpdateSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<UpdateSessionResponse, Error> {
        let caller_id = login_required(&*ci)?;
        info!(logger, "Client updates session: {:?}", request);
        let attributes = request
            .game_session_update
            .attributes
            .0
            .into_iter()
            .map(|property| format!("{} => {}", property.id, property.value))
            .collect::<Vec<_>>()
            .join(";");
        let type_id = request.game_session_update.session_key.type_id;
        let session_id = request.game_session_update.session_key.session_id;
        let updated = rmc_err!(self.storage.update_game_session(caller_id, type_id, session_id, attributes), logger, "error updating game session")?;
        if !updated {
            return Err(Error::AccessDenied);
        }
        log_session_capacity(logger, &self.storage, "update", type_id, session_id, Some(caller_id));
        Ok(UpdateSessionResponse)
    }

    fn delete_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: DeleteSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<DeleteSessionResponse, Error> {
        let user_id = login_required(&*ci)?;
        let outcome = rmc_err!(
            self.storage
                .delete_game_session(user_id, request.game_session_key.type_id, request.game_session_key.session_id),
            logger,
            "error deleting session"
        )?;
        match outcome {
            DeleteGameSessionOutcome::NotFound => warn!(logger, "Session delete rejected for non-owner or missing session"),
            DeleteGameSessionOutcome::Destroyed => {}
            DeleteGameSessionOutcome::MigrationPending { remaining_participants } => info!(
                logger,
                "HostLeavePreservedForMigration";
                "old_host_id" => user_id,
                "session_id" => request.game_session_key.session_id,
                "session_type" => request.game_session_key.type_id,
                "remaining_participants" => remaining_participants,
            ),
        }
        Ok(DeleteSessionResponse)
    }

    fn migrate_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: MigrateSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<MigrateSessionResponse, Error> {
        let user_id = login_required(&*ci)?;
        let migrated = rmc_err!(
            self.storage
                .migrate_game_session_host(user_id, request.game_session_key.type_id, request.game_session_key.session_id),
            logger,
            "error migrating game session"
        )?;
        if !migrated {
            return Err(Error::AccessDenied);
        }
        Ok(MigrateSessionResponse {
            game_session_key_migrated: request.game_session_key,
        })
    }

    fn migrate_session_host(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: MigrateSessionHostRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<MigrateSessionHostResponse, Error> {
        let user_id = login_required(&*ci)?;
        let migrated = rmc_err!(
            self.storage
                .migrate_game_session_host(user_id, request.game_session_key.type_id, request.game_session_key.session_id),
            logger,
            "error migrating game session host"
        )?;
        if !migrated {
            return Err(Error::AccessDenied);
        }
        Ok(MigrateSessionHostResponse)
    }

    fn leave_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: LeaveSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<LeaveSessionResponse, Error> {
        let user_id = login_required(&*ci)?;
        info!(logger, "Client leaves session: {:?}", request);
        let type_id = request.game_session_key.type_id;
        let session_id = request.game_session_key.session_id;
        rmc_err!(self.storage.leave_game_session(user_id, type_id, session_id), logger, "error leaving game session")?;
        log_session_capacity(logger, &self.storage, "leave", type_id, session_id, Some(user_id));
        Ok(LeaveSessionResponse)
    }

    fn add_participants(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: AddParticipantsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<AddParticipantsResponse, Error> {
        let caller_id = login_required(&*ci)?;
        info!(logger, "Client adds participants: {:?}", request);
        let type_id = request.game_session_key.type_id;
        let session_id = request.game_session_key.session_id;
        rmc_err!(
            self.storage.add_participants(
                caller_id,
                type_id,
                session_id,
                request.private_participant_ids.0.clone(),
                request.public_participant_ids.0.clone()
            ),
            logger,
            "error adding participants"
        )?;
        log_session_capacity(logger, &self.storage, "add_participants", type_id, session_id, Some(caller_id));
        Ok(AddParticipantsResponse)
    }

    fn remove_participants(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: RemoveParticipantsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<RemoveParticipantsResponse, Error> {
        let caller_id = login_required(&*ci)?;
        info!(logger, "Client removes participants: {:?}", request);
        let type_id = request.game_session_key.type_id;
        let session_id = request.game_session_key.session_id;
        let removed = rmc_err!(
            self.storage.remove_participants(caller_id, type_id, session_id, request.participant_ids.0.clone()),
            logger,
            "error removing participants"
        )?;
        if !removed {
            warn!(
                logger,
                "Rejected participant removal by non-owner or for missing session";
                "caller_id" => caller_id,
                "type_id" => type_id,
                "session_id" => session_id,
            );
            return Err(Error::AccessDenied);
        }
        log_session_capacity(logger, &self.storage, "remove_participants", type_id, session_id, Some(caller_id));
        Ok(RemoveParticipantsResponse)
    }

    fn abandon_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: AbandonSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<AbandonSessionResponse, Error> {
        let user_id = login_required(&*ci)?;
        info!(logger, "Client abandons session: {:?}", request);
        let type_id = request.game_session_key.type_id;
        let session_id = request.game_session_key.session_id;
        rmc_err!(self.storage.abandon_game_session(user_id, type_id, session_id), logger, "error abandoning game session")?;
        log_session_capacity(logger, &self.storage, "abandon", type_id, session_id, Some(user_id));
        Ok(AbandonSessionResponse)
    }

    fn register_urls(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: RegisterUrLsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<RegisterUrLsResponse, Error> {
        let user_id = login_required(&*ci)?;
        info!(logger, "Client registers urls: {:?}", request);
        let observed_address = ci.address().ip();
        let mut rewrites = Vec::new();
        let station_urls = request
            .station_urls
            .0
            .into_iter()
            .map(|mut station_url| {
                if let Some(address) = observed_private_ipv4_override(&station_url.address, observed_address) {
                    rewrites.push((station_url.address.clone(), address.clone()));
                    station_url.address = address;
                }
                station_url.to_string()
            })
            .collect();
        if !rewrites.is_empty() {
            info!(
                logger,
                "PrivateLanStationUrlsNormalized";
                "user_id" => user_id,
                "observed_address" => observed_address.to_string(),
                "rewrites" => format!("{:?}", rewrites),
            );
        }
        rmc_err!(self.storage.register_urls(user_id, station_urls), logger, "error adding participants")?;
        Ok(RegisterUrLsResponse)
    }

    fn search_sessions_with_participants(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: SearchSessionsWithParticipantsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<SearchSessionsWithParticipantsResponse, Error> {
        let user_id = login_required(&*ci)?;
        info!(logger, "Searches for sessions with {request:?}");
        let session_type = request.game_session_type_id;
        let requested_participants = request.participant_ids.0;
        let invited = self.storage.find_accepted_invited_session(user_id, session_type).map_err(|error| {
            error!(logger, "Error resolving accepted invitation: {error}");
            Error::InternalError
        })?;
        let sessions = if let Some(invited) = invited {
            let invited_participants = invited.session.participants.iter().map(|participant| participant.user_id).collect::<Vec<_>>();
            if invitation_can_route_participant_search(invited.kind) && participant_search_matches_invited_session(&requested_participants, &invited_participants) {
                info!(
                    logger,
                    "InviteParticipantSearchRouted";
                    "user_id" => user_id,
                    "invite_id" => invited.invite_id,
                    "kind" => invited.kind,
                    "session_type" => invited.session.session_type,
                    "session_id" => invited.session.session_id,
                    "requested_participants" => format!("{:?}", requested_participants),
                );
                vec![invited.session]
            } else {
                warn!(
                    logger,
                    "Accepted invitation did not match the requested participant; using normal search";
                    "user_id" => user_id,
                    "invite_id" => invited.invite_id,
                    "kind" => invited.kind,
                    "requested_participants" => format!("{:?}", requested_participants),
                    "invited_participants" => format!("{:?}", invited_participants),
                );
                self.storage
                    .search_sessions_with_participants(session_type, requested_participants.as_slice())
                    .map_err(|error| {
                        error!(logger, "Error searching game sessions: {error}");
                        Error::InternalError
                    })?
            }
        } else {
            self.storage
                .search_sessions_with_participants(session_type, requested_participants.as_slice())
                .map_err(|error| {
                    error!(logger, "Error searching game sessions: {error}");
                    Error::InternalError
                })?
        };
        info!(logger, "Found sessions: {sessions:#?}");
        Ok(SearchSessionsWithParticipantsResponse {
            search_results: sessions
                .into_iter()
                .filter_map(|session| {
                    let Some(host) = session.participants.iter().find(|participant| participant.user_id == session.creator_id) else {
                        warn!(logger, "Ignoring session without a valid host participant"; "session_id" => session.session_id);
                        return None;
                    };
                    if host.station_urls.is_empty() {
                        warn!(logger, "Ignoring session whose host has no registered endpoint"; "session_id" => session.session_id, "host_pid" => host.user_id);
                        return None;
                    }
                    let Ok(host_urls) = host.station_urls.clone().try_into() else {
                        warn!(logger, "Ignoring session with invalid host endpoints"; "session_id" => session.session_id, "host_pid" => host.user_id);
                        return None;
                    };
                    let Ok(attributes) = session.attributes.as_str().parse() else {
                        warn!(logger, "Ignoring session with invalid attributes"; "session_id" => session.session_id);
                        return None;
                    };
                    Some(GameSessionSearchWithParticipantsResult {
                        game_session_search_result: GameSessionSearchResult {
                            session_key: GameSessionKey {
                                type_id: session.session_type,
                                session_id: session.session_id,
                            },
                            host_pid: host.user_id,
                            host_urls,
                            attributes,
                        },
                        participant_ids: session.participants.into_iter().map(|participant| participant.user_id).collect(),
                    })
                })
                .collect(),
        })
    }

    fn split_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: SplitSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<SplitSessionResponse, Error> {
        let user_id = login_required(&*ci)?;
        info!(logger, "Client migrates session host: {:?}", request);
        let migrated = rmc_err!(
            self.storage
                .migrate_game_session_host(user_id, request.game_session_key.type_id, request.game_session_key.session_id),
            logger,
            "error migrating game session host"
        )?;
        let restored = if migrated {
            false
        } else {
            let session_active = rmc_err!(
                self.storage.game_session_is_active(request.game_session_key.type_id, request.game_session_key.session_id),
                logger,
                "error checking split session state"
            )?;
            if session_active {
                false
            } else {
                rmc_err!(
                    self.storage
                        .restore_abandoned_game_session_for_split(user_id, request.game_session_key.type_id, request.game_session_key.session_id),
                    logger,
                    "error restoring abandoned split session"
                )?
            }
        };
        if !split_session_result_allowed(migrated, restored) {
            warn!(
                logger,
                "Rejected game session host migration";
                "session_id" => request.game_session_key.session_id,
                "type_id" => request.game_session_key.type_id,
                "new_host_pid" => user_id,
            );
            return Err(Error::AccessDenied);
        }
        if restored {
            info!(
                logger,
                "SplitSessionRestoredAbandonedLobby";
                "session_id" => request.game_session_key.session_id,
                "type_id" => request.game_session_key.type_id,
                "user_id" => user_id,
            );
            log_session_capacity(
                logger,
                &self.storage,
                "split_restore",
                request.game_session_key.type_id,
                request.game_session_key.session_id,
                Some(user_id),
            );
        }
        Ok(SplitSessionResponse {
            game_session_key_migrated: request.game_session_key,
        })
    }

    fn report_unsuccessful_join_sessions(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: ReportUnsuccessfulJoinSessionsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<ReportUnsuccessfulJoinSessionsResponse, Error> {
        let user_id = login_required(&*ci)?;
        info!(
            logger,
            "Client reported unsuccessful join sessions";
            "user_id" => user_id,
            "count" => request.unsuccessful_join_sessions.0.len(),
            "details" => format!("{:?}", request.unsuccessful_join_sessions),
        );
        Ok(ReportUnsuccessfulJoinSessionsResponse)
    }

    fn join_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: JoinSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<JoinSessionResponse, Error> {
        let user_id = login_required(&*ci)?;
        let type_id = request.game_session_key.type_id;
        let session_id = request.game_session_key.session_id;
        info!(logger, "Client records unbounded session join: {:?}", request);
        let joined = rmc_err!(
            self.storage.join_game_session_unbounded(user_id, type_id, session_id),
            logger,
            "error recording game session join"
        )?;
        if !joined {
            warn!(logger, "Rejected join for missing or destroyed session"; "session_id" => session_id, "type_id" => type_id, "user_id" => user_id);
            return Err(Error::AccessDenied);
        }
        self.storage.clear_manual_invitation_search(user_id, type_id);
        log_session_capacity(logger, &self.storage, "join_recorded_unbounded", type_id, session_id, Some(user_id));
        Ok(JoinSessionResponse)
    }
}

#[cfg(test)]
mod tests {
    use super::invitation_can_route_participant_search;
    use super::observed_private_ipv4_override;
    use super::participant_search_matches_invited_session;
    use super::split_session_result_allowed;
    use crate::storage::INVITE_KIND_LOBBY_PARTY;
    use crate::storage::INVITE_KIND_LOBBY_RESTORE;
    use crate::storage::INVITE_KIND_PARTY_FOLLOW;
    use crate::storage::INVITE_KIND_PRIVATE_ROOM;

    #[test]
    fn private_lan_observation_replaces_a_different_registered_adapter() {
        assert_eq!(
            observed_private_ipv4_override("26.92.150.198", "192.168.1.188".parse().unwrap()),
            Some("192.168.1.188".to_string())
        );
    }

    #[test]
    fn public_observation_replaces_a_registered_address() {
        assert_eq!(observed_private_ipv4_override("10.0.0.4", "203.0.113.8".parse().unwrap()), Some("203.0.113.8".to_string()));
    }

    #[test]
    fn matching_private_observation_is_left_unchanged() {
        assert_eq!(observed_private_ipv4_override("192.168.1.188", "192.168.1.188".parse().unwrap()), None);
    }

    #[test]
    fn split_session_accepts_migration_or_restored_abandoned_session() {
        assert!(split_session_result_allowed(true, true));
        assert!(split_session_result_allowed(false, true));
    }

    #[test]
    fn split_session_rejects_unmigrated_unrestored_session() {
        assert!(!split_session_result_allowed(false, false));
    }

    #[test]
    fn accepted_invitation_routes_participant_search_to_exact_target() {
        assert!(participant_search_matches_invited_session(&[1003], &[1003, 1019]));
        assert!(!participant_search_matches_invited_session(&[1003], &[1004, 1019]));
    }

    #[test]
    fn accepted_invites_route_participant_search_without_query_prewarm() {
        assert!(invitation_can_route_participant_search(INVITE_KIND_PRIVATE_ROOM));
        assert!(invitation_can_route_participant_search(INVITE_KIND_LOBBY_PARTY));
        assert!(invitation_can_route_participant_search(INVITE_KIND_PARTY_FOLLOW));
        assert!(!invitation_can_route_participant_search(0));
        assert!(!invitation_can_route_participant_search(99));
    }

    #[test]
    fn automatic_lobby_restore_routes_participant_search() {
        assert!(invitation_can_route_participant_search(INVITE_KIND_LOBBY_RESTORE));
    }
}

pub fn new_protocol<T: 'static>(storage: Arc<Storage>) -> Box<dyn Protocol<T>> {
    crate::gameplay_modes::protect(storage.clone(),Box::new(GameSessionProtocolServer::new(GameSessionProtocolServerImpl { storage })))
}
