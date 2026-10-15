use quazal::prudp::ClientRegistry;
use quazal::rmc::types::DateTime;
use quazal::rmc::types::QList;
use quazal::rmc::types::Variant;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::ClientInfo;
use quazal::Context;
use slog::Logger;

use crate::login_required;
use crate::protocols::user_storage::types::ContentProperty;
use crate::protocols::user_storage::types::UserContent;
use crate::protocols::user_storage::types::UserContentKey;
use crate::protocols::user_storage::types::UserContentURL;
use crate::protocols::user_storage::user_storage_protocol::GetContentUrlRequest;
use crate::protocols::user_storage::user_storage_protocol::GetContentUrlResponse;
use crate::protocols::user_storage::user_storage_protocol::SearchContentsRequest;
use crate::protocols::user_storage::user_storage_protocol::SearchContentsResponse;
use crate::protocols::user_storage::user_storage_protocol::UserStorageProtocolServer;
use crate::protocols::user_storage::user_storage_protocol::UserStorageProtocolServerTrait;

/// Implementation of the `UserStorageProtocolServerTrait` for handling user storage requests.
struct UserStorageProtocolServerImpl;

impl<CI> UserStorageProtocolServerTrait<CI> for UserStorageProtocolServerImpl {
    /// Handles the `SearchContents` request, returning a list of user content.
    ///
    /// This function requires the client to be logged in. It currently returns a hardcoded
    /// list of user content for a specific query type.
    fn search_contents(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: SearchContentsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<SearchContentsResponse, Error> {
        #![allow(clippy::unreadable_literal)]

        login_required(&*ci)?;
        if request.query.type_id == 0x8000_0002 {
            let search_results = QList(vec![UserContent {
                key: UserContentKey {
                    type_id: 0x8000_0002,
                    content_id: 1,
                },
                pid: 0x0000_045f,
                properties: QList(vec![
                    ContentProperty {
                        id: 6,
                        value: Variant::I64(0x274),
                    },
                    ContentProperty {
                        id: 4,
                        value: Variant::DateTime(DateTime(0x1f768edbd6)),
                    },
                    ContentProperty {
                        id: 5,
                        value: Variant::DateTime(DateTime(0x1f768f13f9)),
                    },
                    ContentProperty {
                        id: 7,
                        value: Variant::String("A6E32CFD0C2B2CFFE2D0C785830B7C49".to_string()),
                    },
                ]),
            }]);
            Ok(SearchContentsResponse { search_results })
        } else {
            Ok(SearchContentsResponse { search_results: QList::default() })
        }
    }

    /// Handles the `GetContentUrl` request, returning the URL for a piece of user content.
    ///
    /// This function requires the client to be logged in. It constructs the URL from
    /// the server's configuration.
    fn get_content_url(
        &self,
        _logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        _request: GetContentUrlRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetContentUrlResponse, Error> {
        login_required(&*ci)?;
        let protocol = ctx.settings.get("content_protocol").map_or("http://", String::as_str).to_owned();
        let host = ctx.settings.get("storage_host").cloned().ok_or(Error::InternalError)?;
        let path = ctx.settings.get("storage_path").cloned().ok_or(Error::InternalError)?;

        Ok(GetContentUrlResponse {
            download_info: UserContentURL { protocol, host, path },
        })
    }
}

/// Creates a new boxed `UserStorageProtocolServer` instance.
///
/// This function is typically used to register the user storage protocol
/// with the server's protocol dispatcher.
pub fn new_protocol<T: 'static>() -> Box<dyn Protocol<T>> {
    Box::new(UserStorageProtocolServer::new(UserStorageProtocolServerImpl))
}


#[cfg(test)]
mod configuration_tests {
    use super::*;
    fn invoke(ctx:&Context,authenticated:bool)->Result<GetContentUrlResponse,Error>{
        let logger=Logger::root(slog::Discard,slog::o!());let socket=std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut ci=ClientInfo::<()>::new(socket.local_addr().unwrap());if authenticated{ci.user_id=Some(1);}
        UserStorageProtocolServerImpl.get_content_url(&logger,ctx,&mut ci,GetContentUrlRequest{content_key:UserContentKey{type_id:0x8000_0002,content_id:1}},&ClientRegistry::default(),&socket)
    }
    #[test]
    fn missing_url_settings_return_errors_without_bypassing_login(){
        for (host,path) in [(false,true),(true,false),(false,false)]{
            let mut ctx=Context::splinter_cell_blacklist();ctx.settings.remove("storage_host");ctx.settings.remove("storage_path");
            if host{ctx.settings.insert("storage_host".into(),"127.0.0.1:18080".into());}
            if path{ctx.settings.insert("storage_path".into(),"owned.ini".into());}
            assert!(matches!(invoke(&ctx,false),Err(Error::AccessDenied)));
            assert!(matches!(invoke(&ctx,true),Err(Error::InternalError)));
        }
    }
    #[test]
    fn configured_url_and_default_scheme_values_are_unchanged(){
        for scheme in [None,Some("https://"),Some("")]{
            let mut ctx=Context::splinter_cell_blacklist();ctx.settings.remove("content_protocol");
            ctx.settings.insert("storage_host".into(),"127.0.0.1:18080".into());ctx.settings.insert("storage_path".into(),"owned.ini".into());
            if let Some(value)=scheme{ctx.settings.insert("content_protocol".into(),value.into());}
            let result=invoke(&ctx,true).unwrap().download_info;assert_eq!(result.protocol,scheme.unwrap_or("http://"));assert_eq!(result.host,"127.0.0.1:18080");assert_eq!(result.path,"owned.ini");
        }
    }
}
