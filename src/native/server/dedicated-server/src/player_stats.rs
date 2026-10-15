//! Implements the `PlayerStatsProtocolServer` for handling player statistics requests.

use quazal::prudp::ClientRegistry;
use quazal::rmc::types::PropertyVariant;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::ClientInfo;
use quazal::Context;
use sc_bl_protocols::player_stats_service::types::PlayerStatSet;
use sc_bl_protocols::player_stats_service::types::StatboardResult;
use slog::Logger;

use crate::login_required;
use crate::protocols::player_stats_service::player_stats_protocol::PlayerStatsProtocolServer;
use crate::protocols::player_stats_service::player_stats_protocol::PlayerStatsProtocolServerTrait;
use crate::protocols::player_stats_service::player_stats_protocol::ReadStatsByPlayersRequest;
use crate::protocols::player_stats_service::player_stats_protocol::ReadStatsByPlayersResponse;
use crate::protocols::player_stats_service::player_stats_protocol::WriteStatsRequest;
use crate::protocols::player_stats_service::player_stats_protocol::WriteStatsResponse;

/// Implementation of the `PlayerStatsProtocolServerTrait` for managing player statistics.
struct PlayerStatsProtocolServerImpl;

fn requested_player_pid(request: &ReadStatsByPlayersRequest) -> Result<u32, Error> {
    request.player_pids.first().copied().ok_or(Error::ParsingError)
}

impl<T> PlayerStatsProtocolServerTrait<T> for PlayerStatsProtocolServerImpl {
    /// Handles the `ReadStatsByPlayers` request, returning hardcoded player statistics.
    ///
    /// This function requires the client to be logged in.
    fn read_stats_by_players(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<T>,
        request: ReadStatsByPlayersRequest,
        _client_registry: &ClientRegistry<T>,
        _socket: &std::net::UdpSocket,
    ) -> Result<ReadStatsByPlayersResponse, Error> {
        // Ensure the client is logged in.
        login_required(&*ci)?;

        let player_pid = requested_player_pid(&request)?;

        Ok(ReadStatsByPlayersResponse {
            results: vec![StatboardResult {
                board_id: 1,
                context_id: 0,
                reset_frequency: 1,
                player_stat_sets: vec![PlayerStatSet {
                    player_pid,
                    player_name: "foobar".to_string(),
                    submitted_time: quazal::rmc::types::DateTime(0x1f_9635_4343),
                    stats: vec![
                        // PropertyVariant {
                        //     id: 0x87, // money
                        //     value: quazal::rmc::types::Variant::I64(10),
                        // },
                        // PropertyVariant {
                        //     id: 0x84,
                        //     value: quazal::rmc::types::Variant::I64(20),
                        // },
                        // PropertyVariant {
                        //     id: 0x85,
                        //     value: quazal::rmc::types::Variant::I64(30),
                        // },
                        // PropertyVariant {
                        //     id: 0x86,
                        //     value: quazal::rmc::types::Variant::I64(40),
                        // },
                        PropertyVariant {
                            id: 0x7e,
                            value: quazal::rmc::types::Variant::I64(1),
                        },
                        PropertyVariant {
                            id: 0x7a,
                            value: quazal::rmc::types::Variant::I64(1),
                        },
                        PropertyVariant {
                            id: 0x7c,
                            value: quazal::rmc::types::Variant::I64(1),
                        },
                        PropertyVariant {
                            id: 0xc9,
                            value: quazal::rmc::types::Variant::I64(1),
                        },
                        PropertyVariant {
                            id: 0xcc,
                            value: quazal::rmc::types::Variant::I64(1),
                        },
                        PropertyVariant {
                            id: 0xcd,
                            value: quazal::rmc::types::Variant::I64(1),
                        },
                        PropertyVariant {
                            id: 0xc8,
                            value: quazal::rmc::types::Variant::I64(1),
                        },
                        PropertyVariant {
                            id: 122, // wins
                            value: quazal::rmc::types::Variant::I64(1337),
                        },
                        PropertyVariant {
                            id: 182,
                            value: quazal::rmc::types::Variant::I64(1337),
                        },
                    ]
                    .into(),
                }]
                .into(),
                default_stat_values: vec![
                    // PropertyVariant {
                    //     id: 0x87,
                    //     value: quazal::rmc::types::Variant::I64(0),
                    // },
                    // PropertyVariant {
                    //     id: 0x84,
                    //     value: quazal::rmc::types::Variant::I64(0),
                    // },
                    // PropertyVariant {
                    //     id: 0x85,
                    //     value: quazal::rmc::types::Variant::I64(0),
                    // },
                    // PropertyVariant {
                    //     id: 0x86,
                    //     value: quazal::rmc::types::Variant::I64(0),
                    // },
                ]
                .into(),
            }]
            .into(),
        })
    }

    /// Handles the `WriteStats` request.
    ///
    /// This function requires the client to be logged in.
    fn write_stats(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<T>,
        _request: WriteStatsRequest,
        _client_registry: &ClientRegistry<T>,
        _socket: &std::net::UdpSocket,
    ) -> Result<WriteStatsResponse, Error> {
        // Ensure the client is logged in.
        login_required(&*ci)?;
        Ok(WriteStatsResponse)
    }
}

/// Creates a new boxed `PlayerStatsProtocolServer` instance.
///
/// This function is typically used to register the player stats protocol
/// with the server's protocol dispatcher.
pub fn new_protocol<T: 'static>() -> Box<dyn Protocol<T>> {
    Box::new(PlayerStatsProtocolServer::new(PlayerStatsProtocolServerImpl))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_player_list_is_rejected_as_a_protocol_parse_error() {
        let request = ReadStatsByPlayersRequest {
            player_pids: Vec::new().into(),
            queries: Vec::new().into(),
        };
        assert!(matches!(requested_player_pid(&request), Err(Error::ParsingError)));
    }

    #[test]
    fn first_requested_player_is_preserved() {
        let request = ReadStatsByPlayersRequest {
            player_pids: vec![41, 42].into(),
            queries: Vec::new().into(),
        };
        assert_eq!(requested_player_pid(&request).unwrap(), 41);
    }
}
