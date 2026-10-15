//! Implements the `OverlordCoreProtocol` for handling core server functionalities
//! and configuration requests.

use quazal::prudp::ClientRegistry;
use quazal::rmc::basic::ToStream;
use quazal::rmc::types::Variant;
use quazal::rmc::Protocol;
use quazal::Context;

use crate::login_required;

#[allow(clippy::module_name_repetitions)]
/// Implements the `Protocol` trait for the Overlord Core protocol.
pub struct OverlordCoreProtocol;

impl<T> Protocol<T> for OverlordCoreProtocol {
    /// Returns the unique ID of this protocol.
    fn id(&self) -> u16 {
        5003
    }

    /// Returns the name of this protocol.
    fn name(&self) -> String {
        "OverlordCoreProtocol".into()
    }

    /// Returns the number of methods implemented by this protocol.
    fn num_methods(&self) -> u32 {
        1
    }

    /// Handles incoming Remote Method Call (RMC) requests for the Overlord Core protocol.
    ///
    /// This function dispatches requests based on their method ID.
    fn handle(
        &self,
        _logger: &slog::Logger,
        _ctx: &Context,
        ci: &mut quazal::ClientInfo<T>,
        request: &quazal::rmc::Request,
        _client_registry: &ClientRegistry<T>,
        _socket: &std::net::UdpSocket,
    ) -> std::result::Result<Vec<u8>, quazal::rmc::Error> {
        #[allow(clippy::enum_glob_use)]
        use Variant::*;

        // Ensure the client is logged in before processing the request.
        login_required(&*ci)?;
        // Only method ID 1 (fetch_config) is supported by this protocol.
        if request.method_id != 1 {
            return Err(quazal::rmc::Error::UnknownMethod);
        }

        // Hardcoded configuration values returned by the server.
        let cfg: Vec<(std::string::String, Variant)> = vec![
            ("VER_SERVER_STAGE".to_owned(), I64(10)),
            ("2593515025".to_owned(), I64(1)),
            ("2626349757".to_owned(), I64(1)),
            ("NC_CONNECTION_TICKET_TIMEOUT".to_owned(), F64(10.0)),
            ("NC_CONNECTION_HANDSHAKING_TIMEOUT".to_owned(), F64(5.0)),
            ("314486871".to_owned(), I64(1)),
            ("3759634546".to_owned(), I64(1)),
            ("NC_MAIN_PORT_RANGE".to_owned(), I64(32)),
            ("2449304206".to_owned(), I64(1)),
            ("OVERLORD_VERSION".to_owned(), String("0.8.0.0".into())),
            ("2685611256".to_owned(), I64(1)),
            ("3376331533".to_owned(), I64(1)),
            ("VER_SERVER_CODE".to_owned(), I64(3007)),
            ("FILESERVICE_UNLOCKS_UPLOAD_ENABLED".to_owned(), I64(1)),
            ("2739184075".to_owned(), I64(1)),
            ("2942435614".to_owned(), I64(1)),
            ("SN_FRIENDCHALLENGES_MAX_READ_INTERVAL".to_owned(), F64(900.0)),
            ("NC_CONNECTION_JOIN_TIMEOUT".to_owned(), F64(15.0)),
            ("1027449109".to_owned(), I64(1)),
            ("4175756708".to_owned(), I64(1)),
            ("NC_CONNECTION_INACTIVITY_THRESHOLD".to_owned(), F64(8.0)),
            ("FILESERVICE_ADMIN_RDVID".to_owned(), I64(1119)),
            ("SN_WEEKLYCHALLENGES_ENABLE".to_owned(), I64(1)),
            ("1597953054".to_owned(), I64(1)),
            ("2156388390".to_owned(), I64(1)),
            ("3785106560".to_owned(), I64(1)),
            ("STATS_WRITE_INTERVAL".to_owned(), F64(1.0)),
            ("3835530207".to_owned(), I64(1)),
            ("1492891464".to_owned(), I64(1)),
            ("SN_DAILYCHALLENGES_ENABLE".to_owned(), I64(1)),
            ("2505766166".to_owned(), I64(1)),
            ("11866509".to_owned(), I64(1)),
            ("SN_FRIENDCHALLENGES_ENABLE".to_owned(), I64(1)),
            ("FILESERVICE_UNLOCKS_UPLOAD_INTERVAL".to_owned(), F64(3600.0)),
            ("2524360986".to_owned(), I64(1)),
            ("721797971".to_owned(), I64(1)),
            ("1525666223".to_owned(), I64(1)),
            ("COMMUNITYEVENT_DOUBLECASH".to_owned(), I64(0)),
            ("UPLAY_MAX_RANK_LIMITED_MODE".to_owned(), I64(5)),
            ("SN_GONEDARKCHALLENGES_ENABLE".to_owned(), I64(1)),
            ("_OSDK_VERSION".to_owned(), String("1.4.16.32918".into())),
            ("NC_MAIN_PORT".to_owned(), I64(13000)),
            ("COMMUNITYEVENT_DOUBLEXP".to_owned(), I64(0)),
            ("3804368594".to_owned(), I64(1)),
            ("NC_CONNECTION_CLOSING_TIMEOUT".to_owned(), F64(2.0)),
            ("NC_CONNECTION_ESTABLISHED_TIMEOUT".to_owned(), F64(10.0)),
        ];

        /*
        // Alternative implementation using a HashMap for configuration values.
        // This could be used for more dynamic configuration loading.
         let cfg: std::collections::HashMap<std::string::String, Variant> = [
            ...
        ].iter()
        .cloned()
        .collect();
        */
        Ok(cfg.try_to_bytes()?)
    }

    /// Returns the name of the method corresponding to the given `method_id`.
    ///
    /// Returns `None` if the `method_id` is not recognized.
    fn method_name(&self, method_id: u32) -> Option<String> {
        if method_id == 1 {
            Some("fetch_config".into())
        } else {
            None
        }
    }
}

/// Creates a new boxed `OverlordCoreProtocol` instance.
///
/// This function is typically used to register the core protocol
/// with the server's protocol dispatcher.
pub fn new_protocol<T: 'static>() -> Box<dyn Protocol<T>> {
    Box::new(OverlordCoreProtocol)
}

