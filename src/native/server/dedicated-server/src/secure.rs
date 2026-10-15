use quazal::prudp::ClientRegistry;
use quazal::rmc::types::QResult;
use quazal::rmc::types::StationURL;
use quazal::rmc::Protocol;

use crate::login_required;
use crate::protocols::secure_connection_service::secure_connection_protocol::RegisterExRequest;
use crate::protocols::secure_connection_service::secure_connection_protocol::RegisterExResponse;
use crate::protocols::secure_connection_service::secure_connection_protocol::RegisterRequest;
use crate::protocols::secure_connection_service::secure_connection_protocol::RegisterResponse;
use crate::protocols::secure_connection_service::secure_connection_protocol::SecureConnectionProtocolServer;
use crate::protocols::secure_connection_service::secure_connection_protocol::SecureConnectionProtocolServerTrait;

/// Implementation of the `SecureConnectionProtocolServerTrait` for handling secure connection requests.
struct SecureConnectionProtocolServerImpl {storage:std::sync::Arc<crate::storage::Storage>}

fn registration_url_summary(urls: &[crate::protocols::secure_connection_service::types::UntrustedRegistrationUrl]) -> (usize, usize, usize) {
    let missing_address = urls
        .iter()
        .filter(|url| {
            !url.as_str().split_once(":/").is_some_and(|(_, params)| {
                params
                    .split(';')
                    .any(|parameter| parameter.split_once('=').is_some_and(|(key, value)| key == "address" && !value.is_empty()))
            })
        })
        .count();
    let missing_port = urls
        .iter()
        .filter(|url| {
            !url.as_str().split_once(":/").is_some_and(|(_, params)| {
                params
                    .split(';')
                    .any(|parameter| parameter.split_once('=').is_some_and(|(key, value)| key == "port" && !value.is_empty()))
            })
        })
        .count();
    (urls.len(), missing_address, missing_port)
}

fn observed_public_url(address: &std::net::SocketAddr, sid: u16, connection_type: u8) -> Result<StationURL, quazal::rmc::Error> {
    format!("prudp:/address={};port={};sid={sid};type={connection_type}", address.ip(), address.port())
        .parse()
        .map_err(|_| quazal::rmc::Error::InternalError)
}

impl<T> SecureConnectionProtocolServerTrait<T> for SecureConnectionProtocolServerImpl {
    /// Handles the `Register` request, establishing a secure connection.
    ///
    /// This function requires the client to be logged in. It returns the connection ID
    /// and the public URL of the client.
    fn register(
        &self,
        logger: &slog::Logger,
        _ctx: &quazal::Context,
        ci: &mut quazal::ClientInfo<T>,
        request: RegisterRequest,
        _client_registry: &ClientRegistry<T>,
        _socket: &std::net::UdpSocket,
    ) -> Result<RegisterResponse, quazal::rmc::Error> {
        let user_id = login_required(&*ci)?;
        let _generation=self.storage.mode_gate.blocking_read();
        if !self.storage.mark_secure_ticket(user_id,ci.ticket_fingerprint).map_err(|_|quazal::rmc::Error::InternalError)? {return Err(quazal::rmc::Error::AccessDenied);}
        let (request_url_count, missing_address_count, missing_port_count) = registration_url_summary(&request.vec_my_urls);
        info!(logger, "Secure registration request accepted";
            "request_url_count" => request_url_count,
            "missing_address_count" => missing_address_count,
            "missing_port_count" => missing_port_count,
            "connection_id_present" => ci.connection_id.is_some(),
            "method" => "Register");
        let response = RegisterResponse {
            return_value: QResult::Ok,
            pid_connection_id: ci.connection_id.ok_or(quazal::rmc::Error::InternalError)?.into(),
            // Client-provided URLs are untrusted claims.  The public URL is
            // always derived from the authenticated socket observed here.
            url_public: observed_public_url(ci.address(), 14, 2)?,
        };
        info!(logger, "Secure registration response constructed";
            "qresult" => "Ok",
            "url_source" => "observed_socket",
            "url_has_address" => !response.url_public.address.is_empty(),
            "port_nonzero" => response.url_public.port != 0,
            "sid" => 14,
            "connection_type" => 2,
            "method" => "Register");
        Ok(response)
    }

    /// Handles the `RegisterEx` request, establishing an extended secure connection.
    ///
    /// This function requires the client to be logged in. It returns the connection ID
    /// and the public URL of the client.
    fn register_ex(
        &self,
        logger: &slog::Logger,
        _ctx: &quazal::Context,
        ci: &mut quazal::ClientInfo<T>,
        request: RegisterExRequest,
        _client_registry: &ClientRegistry<T>,
        _socket: &std::net::UdpSocket,
    ) -> Result<RegisterExResponse, quazal::rmc::Error> {
        let user_id = login_required(&*ci)?;
        let _generation=self.storage.mode_gate.blocking_read();
        if !self.storage.mark_secure_ticket(user_id,ci.ticket_fingerprint).map_err(|_|quazal::rmc::Error::InternalError)? {return Err(quazal::rmc::Error::AccessDenied);}
        let (request_url_count, missing_address_count, missing_port_count) = registration_url_summary(&request.vec_my_urls);
        info!(logger, "Secure registration request accepted";
            "request_url_count" => request_url_count,
            "missing_address_count" => missing_address_count,
            "missing_port_count" => missing_port_count,
            "connection_id_present" => ci.connection_id.is_some(),
            "method" => "RegisterEx");
        let response = RegisterExResponse {
            return_value: QResult::Ok,
            pid_connection_id: ci.connection_id.ok_or(quazal::rmc::Error::InternalError)?.into(),
            url_public: observed_public_url(ci.address(), 15, 3)?,
        };
        info!(logger, "Secure registration response constructed";
            "qresult" => "Ok",
            "url_source" => "observed_socket",
            "url_has_address" => !response.url_public.address.is_empty(),
            "port_nonzero" => response.url_public.port != 0,
            "sid" => 15,
            "connection_type" => 3,
            "method" => "RegisterEx");
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_registration_url_uses_the_observed_socket_endpoint() {
        let observed = "10.66.0.3:45678".parse().unwrap();
        let url = observed_public_url(&observed, 14, 2).unwrap();
        assert_eq!(url.address, "10.66.0.3");
        assert_eq!(url.port, 45678);
        assert_eq!(url.params.get("sid").map(String::as_str), Some("14"));
        assert_eq!(url.params.get("type").map(String::as_str), Some("2"));
    }

    #[test]
    fn registration_summary_counts_missing_addresses_without_exposing_urls() {
        let missing = crate::protocols::secure_connection_service::types::UntrustedRegistrationUrl::parse("prudps:/port=3074;sid=15;type=3".to_string()).unwrap();
        let present =
            crate::protocols::secure_connection_service::types::UntrustedRegistrationUrl::parse("prudps:/address=198.51.100.10;port=3074;sid=15;type=3".to_string()).unwrap();
        let missing_endpoint = crate::protocols::secure_connection_service::types::UntrustedRegistrationUrl::parse("prudps:/sid=15;type=3".to_string()).unwrap();
        assert_eq!(registration_url_summary(&[missing, present, missing_endpoint]), (3, 2, 1));
    }
}

/// Creates a new boxed `SecureConnectionProtocolServer` instance.
///
/// This function is typically used to register the secure connection protocol
/// with the server's protocol dispatcher.
pub fn new_protocol<T: 'static>(storage:std::sync::Arc<crate::storage::Storage>) -> Box<dyn Protocol<T>> {
    Box::new(SecureConnectionProtocolServer::new(SecureConnectionProtocolServerImpl{storage}))
}
