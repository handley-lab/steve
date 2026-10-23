use std::collections::BTreeMap;
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::time::Duration;

use codex_model_provider_info::ModelProviderCapabilities;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::RemoteCompactionSupport;
use codex_model_provider_info::WireApi;
use codex_protocol::config_types::ModelProviderAuthInfo;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_redacted_string::RedactedString;

use super::SessionThreadConfig;
use super::ThreadConfigContext;
use super::ThreadConfigLoadError;
use super::ThreadConfigLoadErrorCode;
use super::ThreadConfigLoader;
use super::ThreadConfigLoaderFuture;
use super::ThreadConfigSource;
use super::UserThreadConfig;
use proto::thread_config_loader_client::ThreadConfigLoaderClient;

#[path = "proto/codex.thread_config.v1.rs"]
mod proto;

const REMOTE_THREAD_CONFIG_LOAD_TIMEOUT: Duration = Duration::from_secs(5);

/// gRPC-backed [`ThreadConfigLoader`] implementation.
#[derive(Clone, Debug)]
pub struct RemoteThreadConfigLoader {
    endpoint: String,
}

impl RemoteThreadConfigLoader {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }

    async fn client(
        &self,
    ) -> Result<ThreadConfigLoaderClient<tonic::transport::Channel>, ThreadConfigLoadError> {
        ThreadConfigLoaderClient::connect(self.endpoint.clone())
            .await
            .map_err(|err| {
                ThreadConfigLoadError::new(
                    ThreadConfigLoadErrorCode::RequestFailed,
                    /*status_code*/ None,
                    format!("failed to connect to remote thread config loader: {err}"),
                )
            })
    }

    async fn load(
        &self,
        context: ThreadConfigContext,
    ) -> Result<Vec<ThreadConfigSource>, ThreadConfigLoadError> {
        let response = self
            .client()
            .await?
            .load(load_thread_config_request(context))
            .await
            .map_err(remote_status_to_error)?
            .into_inner();

        response
            .sources
            .into_iter()
            .map(thread_config_source_from_proto)
            .collect()
    }
}

impl ThreadConfigLoader for RemoteThreadConfigLoader {
    fn load(
        &self,
        context: ThreadConfigContext,
    ) -> ThreadConfigLoaderFuture<'_, Vec<ThreadConfigSource>> {
        Box::pin(RemoteThreadConfigLoader::load(self, context))
    }
}

fn load_thread_config_request(
    context: ThreadConfigContext,
) -> tonic::Request<proto::LoadThreadConfigRequest> {
    let mut request = tonic::Request::new(proto::LoadThreadConfigRequest {
        thread_id: context.thread_id,
        cwd: context.cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
    });
    request.set_timeout(REMOTE_THREAD_CONFIG_LOAD_TIMEOUT);
    request
}

fn remote_status_to_error(status: tonic::Status) -> ThreadConfigLoadError {
    let code = match status.code() {
        tonic::Code::Unauthenticated | tonic::Code::PermissionDenied => {
            ThreadConfigLoadErrorCode::Auth
        }
        tonic::Code::DeadlineExceeded => ThreadConfigLoadErrorCode::Timeout,
        tonic::Code::Ok
        | tonic::Code::Cancelled
        | tonic::Code::Unknown
        | tonic::Code::InvalidArgument
        | tonic::Code::NotFound
        | tonic::Code::AlreadyExists
        | tonic::Code::ResourceExhausted
        | tonic::Code::FailedPrecondition
        | tonic::Code::Aborted
        | tonic::Code::OutOfRange
        | tonic::Code::Unimplemented
        | tonic::Code::Internal
        | tonic::Code::Unavailable
        | tonic::Code::DataLoss => ThreadConfigLoadErrorCode::RequestFailed,
    };
    ThreadConfigLoadError::new(
        code,
        /*status_code*/ None,
        format!("remote thread config request failed: {status}"),
    )
}

fn thread_config_source_from_proto(
    source: proto::ThreadConfigSource,
) -> Result<ThreadConfigSource, ThreadConfigLoadError> {
    match source.source {
        Some(proto::thread_config_source::Source::Session(config)) => {
            session_thread_config_from_proto(config).map(ThreadConfigSource::Session)
        }
        Some(proto::thread_config_source::Source::User(_)) => {
            Ok(ThreadConfigSource::User(UserThreadConfig::default()))
        }
        None => Err(parse_error("remote thread config omitted source payload")),
    }
}

fn session_thread_config_from_proto(
    config: proto::SessionThreadConfig,
) -> Result<SessionThreadConfig, ThreadConfigLoadError> {
    let model_providers = config
        .model_providers
        .into_iter()
        .map(model_provider_from_proto)
        .collect::<Result<HashMap<_, _>, _>>()?;

    Ok(SessionThreadConfig {
        model_provider: config.model_provider,
        model_providers,
        features: config.features.into_iter().collect::<BTreeMap<_, _>>(),
    })
}

fn model_provider_from_proto(
    provider: proto::ModelProvider,
) -> Result<(String, ModelProviderInfo), ThreadConfigLoadError> {
    if provider.id.is_empty() {
        return Err(parse_error(
            "remote thread config returned model provider without an id",
        ));
    }
    let id = provider.id;
    let wire_api = match proto::WireApi::try_from(provider.wire_api) {
        Ok(proto::WireApi::Responses) => WireApi::Responses,
        Ok(proto::WireApi::Unspecified) => {
            return Err(parse_error("remote thread config omitted wire_api"));
        }
        Err(_) => {
            return Err(parse_error(format!(
                "remote thread config returned unknown wire_api: {}",
                provider.wire_api
            )));
        }
    };
    let info = ModelProviderInfo {
        name: provider.name,
        base_url: provider.base_url,
        model_catalog_url: provider.model_catalog_url.map(Into::into),
        env_key: provider.env_key,
        env_key_instructions: provider.env_key_instructions,
        experimental_bearer_token: provider.experimental_bearer_token.map(Into::into),
        auth: provider
            .auth
            .map(model_provider_auth_from_proto)
            .transpose()?,
        gateway_oauth: None,
        aws: None,
        wire_api,
        query_params: provider.query_params.map(redacted_string_map),
        http_headers: provider.http_headers.map(redacted_string_map),
        env_http_headers: provider.env_http_headers.map(|map| map.values),
        request_max_retries: provider.request_max_retries,
        stream_max_retries: provider.stream_max_retries,
        stream_idle_timeout_ms: provider.stream_idle_timeout_ms,
        websocket_connect_timeout_ms: provider.websocket_connect_timeout_ms,
        requires_openai_auth: provider.requires_openai_auth,
        supports_websockets: provider.supports_websockets,
        supports_standalone_web_search: provider.supports_standalone_web_search,
        capabilities: remote_compaction_from_proto(provider.remote_compaction)?.map(
            |remote_compaction| ModelProviderCapabilities {
                external_web_access: None,
                remote_compaction: Some(remote_compaction),
            },
        ),
        accepts_response_continuation: provider.accepts_response_continuation,
        include_internal_metadata: provider.include_internal_metadata,
    };
    Ok((id, info))
}

fn remote_compaction_from_proto(
    value: Option<i32>,
) -> Result<Option<RemoteCompactionSupport>, ThreadConfigLoadError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match proto::RemoteCompaction::try_from(value) {
        Ok(proto::RemoteCompaction::Unsupported) => Ok(Some(RemoteCompactionSupport::Unsupported)),
        Ok(proto::RemoteCompaction::V2) => Ok(Some(RemoteCompactionSupport::V2)),
        Ok(proto::RemoteCompaction::Unspecified) => Err(parse_error(
            "remote thread config returned unspecified remote_compaction",
        )),
        Err(_) => Err(parse_error(format!(
            "remote thread config returned unknown remote_compaction: {value}"
        ))),
    }
}

#[cfg(test)]
fn config_layer(
    source: ThreadConfigSource,
) -> Result<Option<crate::ConfigLayerEntry>, ThreadConfigLoadError> {
    super::thread_config_source_to_layer(source)
}

#[cfg(test)]
fn remote_compaction_to_proto(capabilities: Option<ModelProviderCapabilities>) -> Option<i32> {
    capabilities
        .and_then(|capabilities| capabilities.remote_compaction)
        .map(|remote_compaction| match remote_compaction {
            RemoteCompactionSupport::Unsupported => proto::RemoteCompaction::Unsupported,
            RemoteCompactionSupport::V2 => proto::RemoteCompaction::V2,
        } as i32)
}

#[cfg(test)]
fn model_provider_to_proto(
    id: impl Into<String>,
    provider: ModelProviderInfo,
) -> proto::ModelProvider {
    let ModelProviderInfo {
        name,
        base_url,
        model_catalog_url,
        env_key,
        env_key_instructions,
        experimental_bearer_token,
        auth,
        gateway_oauth: _,
        aws: _,
        wire_api,
        query_params,
        http_headers,
        env_http_headers,
        request_max_retries,
        stream_max_retries,
        stream_idle_timeout_ms,
        websocket_connect_timeout_ms,
        requires_openai_auth,
        supports_websockets,
        supports_standalone_web_search,
        accepts_response_continuation,
        include_internal_metadata,
        capabilities,
    } = provider;

    proto::ModelProvider {
        id: id.into(),
        name,
        base_url,
        model_catalog_url: model_catalog_url.map(RedactedString::into_inner),
        env_key,
        env_key_instructions,
        experimental_bearer_token: experimental_bearer_token.map(RedactedString::into_inner),
        auth: auth.map(model_provider_auth_to_proto),
        wire_api: proto_wire_api(wire_api).into(),
        query_params: query_params.map(proto_string_map),
        http_headers: http_headers.map(proto_string_map),
        env_http_headers: env_http_headers.map(|values| proto::StringMap { values }),
        request_max_retries,
        stream_max_retries,
        stream_idle_timeout_ms,
        websocket_connect_timeout_ms,
        requires_openai_auth,
        supports_websockets,
        supports_standalone_web_search,
        accepts_response_continuation,
        include_internal_metadata,
        remote_compaction: remote_compaction_to_proto(capabilities),
    }
}

fn model_provider_auth_from_proto(
    auth: proto::ModelProviderAuthInfo,
) -> Result<ModelProviderAuthInfo, ThreadConfigLoadError> {
    let timeout_ms = NonZeroU64::new(auth.timeout_ms)
        .ok_or_else(|| parse_error("remote thread config returned zero auth timeout_ms"))?;
    let cwd = AbsolutePathBuf::from_absolute_path_checked(&auth.cwd).map_err(|err| {
        parse_error(format!(
            "remote thread config returned invalid auth cwd {:?}: {err}",
            auth.cwd
        ))
    })?;

    Ok(ModelProviderAuthInfo {
        command: auth.command,
        args: auth.args.into_iter().map(RedactedString::from).collect(),
        timeout_ms,
        refresh_interval_ms: auth.refresh_interval_ms,
        cwd,
    })
}

#[cfg(test)]
fn model_provider_auth_to_proto(auth: ModelProviderAuthInfo) -> proto::ModelProviderAuthInfo {
    let ModelProviderAuthInfo {
        command,
        args,
        timeout_ms,
        refresh_interval_ms,
        cwd,
    } = auth;

    proto::ModelProviderAuthInfo {
        command,
        args: args.into_iter().map(RedactedString::into_inner).collect(),
        timeout_ms: timeout_ms.get(),
        refresh_interval_ms,
        cwd: cwd.to_string_lossy().into_owned(),
    }
}

fn redacted_string_map(map: proto::StringMap) -> HashMap<String, RedactedString> {
    map.values
        .into_iter()
        .map(|(name, value)| (name, value.into()))
        .collect()
}

#[cfg(test)]
fn proto_string_map(values: HashMap<String, RedactedString>) -> proto::StringMap {
    proto::StringMap {
        values: values
            .into_iter()
            .map(|(name, value)| (name, value.into_inner()))
            .collect(),
    }
}

#[cfg(test)]
fn proto_wire_api(wire_api: WireApi) -> proto::WireApi {
    match wire_api {
        WireApi::Responses => proto::WireApi::Responses,
    }
}

fn parse_error(message: impl Into<String>) -> ThreadConfigLoadError {
    ThreadConfigLoadError::new(
        ThreadConfigLoadErrorCode::Parse,
        /*status_code*/ None,
        message.into(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::HashMap;
    use std::num::NonZeroU64;

    use codex_model_provider_info::ModelProviderCapabilities;
    use codex_model_provider_info::ModelProviderInfo;
    use codex_model_provider_info::RemoteCompactionSupport;
    use codex_model_provider_info::WireApi;
    use codex_protocol::config_types::ModelProviderAuthInfo;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use tonic::Request;
    use tonic::Response;
    use tonic::Status;
    use tonic::transport::Server;

    use super::proto::thread_config_loader_server;
    use super::proto::thread_config_loader_server::ThreadConfigLoaderServer;
    use super::*;
    use crate::SessionThreadConfig;
    use crate::UserThreadConfig;

    struct TestServer {
        sources: Vec<proto::ThreadConfigSource>,
        expected_cwd: String,
    }

    impl TestServer {
        async fn load(
            &self,
            request: Request<proto::LoadThreadConfigRequest>,
        ) -> Result<Response<proto::LoadThreadConfigResponse>, Status> {
            assert_eq!(
                request.into_inner(),
                proto::LoadThreadConfigRequest {
                    thread_id: Some("thread-1".to_string()),
                    cwd: Some(self.expected_cwd.clone()),
                }
            );

            Ok(Response::new(proto::LoadThreadConfigResponse {
                sources: self.sources.clone(),
            }))
        }
    }

    impl thread_config_loader_server::ThreadConfigLoader for TestServer {
        fn load<'a, 'async_trait>(
            &'a self,
            request: Request<proto::LoadThreadConfigRequest>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<Response<proto::LoadThreadConfigResponse>, Status>,
                    > + Send
                    + 'async_trait,
            >,
        >
        where
            'a: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(TestServer::load(self, request))
        }
    }

    #[tokio::test]
    async fn load_thread_config_calls_remote_service() {
        let cwd = workspace_dir().join("project");
        let expected_cwd = cwd.to_string_lossy().into_owned();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server addr");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(ThreadConfigLoaderServer::new(TestServer {
                    sources: proto_sources(),
                    expected_cwd,
                }))
                .serve_with_incoming_shutdown(
                    tokio_stream::wrappers::TcpListenerStream::new(listener),
                    async {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
        });

        let loader = RemoteThreadConfigLoader::new(format!("http://{addr}"));
        let loaded = loader
            .load(ThreadConfigContext {
                thread_id: Some("thread-1".to_string()),
                cwd: Some(cwd),
            })
            .await;

        let _ = shutdown_tx.send(());
        server.await.expect("join server").expect("server");

        assert_eq!(loaded.expect("load thread config"), expected_sources());
    }

    #[test]
    fn load_thread_config_request_sets_timeout() {
        let request = load_thread_config_request(ThreadConfigContext::default());

        assert_eq!(
            request
                .metadata()
                .get("grpc-timeout")
                .and_then(|value| value.to_str().ok()),
            Some("5000000u")
        );
    }

    #[test]
    fn model_provider_proto_roundtrips_through_domain_type() {
        let mut expected = expected_provider();
        expected.auth = None;
        expected.experimental_bearer_token = Some("synthetic-provider-token".into());
        let proto = model_provider_to_proto("local", expected.clone());
        assert!(proto.supports_standalone_web_search);
        let (id, actual) = model_provider_from_proto(proto).expect("model provider from proto");

        assert_eq!(id, "local");
        assert_eq!(actual, expected);
    }

    #[test]
    fn model_provider_proto_defaults_standalone_web_search_to_false() {
        let expected = ModelProviderInfo {
            supports_standalone_web_search: false,
            ..expected_provider()
        };
        let proto = model_provider_to_proto("local", expected.clone());
        assert!(!proto.supports_standalone_web_search);
        let (id, actual) = model_provider_from_proto(proto).expect("model provider from proto");

        assert_eq!(id, "local");
        assert_eq!(actual, expected);
    }

    fn proto_sources() -> Vec<proto::ThreadConfigSource> {
        let workspace_cwd = workspace_dir().to_string_lossy().into_owned();
        vec![
            proto::ThreadConfigSource {
                source: Some(proto::thread_config_source::Source::Session(
                    proto::SessionThreadConfig {
                        model_provider: Some("local".to_string()),
                        model_providers: vec![proto::ModelProvider {
                            id: "local".to_string(),
                            name: "Local".to_string(),
                            base_url: Some("http://127.0.0.1:8061/api/codex".to_string()),
                            model_catalog_url: Some(
                                "http://127.0.0.1:8061/api/codex/models".to_string(),
                            ),
                            env_key: None,
                            env_key_instructions: None,
                            experimental_bearer_token: None,
                            auth: Some(proto::ModelProviderAuthInfo {
                                command: "token-helper".to_string(),
                                args: vec!["--json".to_string()],
                                timeout_ms: 5_000,
                                refresh_interval_ms: 300_000,
                                cwd: workspace_cwd,
                            }),
                            wire_api: proto::WireApi::Responses.into(),
                            query_params: Some(proto::StringMap {
                                values: HashMap::from([(
                                    "api-version".to_string(),
                                    "2026-04-16".to_string(),
                                )]),
                            }),
                            http_headers: Some(proto::StringMap {
                                values: HashMap::from([(
                                    "X-Test".to_string(),
                                    "enabled".to_string(),
                                )]),
                            }),
                            env_http_headers: Some(proto::StringMap {
                                values: HashMap::from([(
                                    "X-Env".to_string(),
                                    "LOCAL_HEADER".to_string(),
                                )]),
                            }),
                            request_max_retries: Some(7),
                            stream_max_retries: Some(8),
                            stream_idle_timeout_ms: Some(9_000),
                            websocket_connect_timeout_ms: Some(10_000),
                            requires_openai_auth: false,
                            supports_websockets: true,
                            supports_standalone_web_search: true,
                            accepts_response_continuation: false,
                            include_internal_metadata: false,
                            remote_compaction: None,
                        }],
                        features: HashMap::from([
                            ("plugins".to_string(), false),
                            ("tools".to_string(), true),
                        ]),
                    },
                )),
            },
            proto::ThreadConfigSource {
                source: Some(proto::thread_config_source::Source::User(
                    proto::UserThreadConfig {},
                )),
            },
        ]
    }

    fn expected_sources() -> Vec<ThreadConfigSource> {
        vec![
            ThreadConfigSource::Session(SessionThreadConfig {
                model_provider: Some("local".to_string()),
                model_providers: HashMap::from([("local".to_string(), expected_provider())]),
                features: BTreeMap::from([
                    ("plugins".to_string(), false),
                    ("tools".to_string(), true),
                ]),
            }),
            ThreadConfigSource::User(UserThreadConfig::default()),
        ]
    }

    fn expected_provider() -> ModelProviderInfo {
        ModelProviderInfo {
            name: "Local".to_string(),
            base_url: Some("http://127.0.0.1:8061/api/codex".to_string()),
            model_catalog_url: Some("http://127.0.0.1:8061/api/codex/models".into()),
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            auth: Some(ModelProviderAuthInfo {
                command: "token-helper".to_string(),
                args: vec!["--json".into()],
                timeout_ms: NonZeroU64::new(5_000).expect("non-zero timeout"),
                refresh_interval_ms: 300_000,
                cwd: workspace_dir(),
            }),
            wire_api: WireApi::Responses,
            query_params: Some(HashMap::from([(
                "api-version".to_string(),
                "2026-04-16".into(),
            )])),
            http_headers: Some(HashMap::from([("X-Test".to_string(), "enabled".into())])),
            env_http_headers: Some(HashMap::from([(
                "X-Env".to_string(),
                "LOCAL_HEADER".to_string(),
            )])),
            request_max_retries: Some(7),
            stream_max_retries: Some(8),
            stream_idle_timeout_ms: Some(9_000),
            websocket_connect_timeout_ms: Some(10_000),
            requires_openai_auth: false,
            supports_websockets: true,
            supports_standalone_web_search: true,
            gateway_oauth: None,
            aws: None,
            capabilities: None,
            accepts_response_continuation: false,
            include_internal_metadata: false,
        }
    }

    #[tokio::test]
    async fn remote_provider_controls_reach_the_config_layer() {
        let explicit = controlled_provider(true, true, Some(RemoteCompactionSupport::V2));
        let omitted = controlled_provider(false, false, None);
        let sources = vec![proto::ThreadConfigSource {
            source: Some(proto::thread_config_source::Source::Session(
                proto::SessionThreadConfig {
                    model_provider: Some("explicit".to_string()),
                    model_providers: vec![
                        model_provider_to_proto("explicit", explicit.clone()),
                        model_provider_to_proto("omitted", omitted.clone()),
                    ],
                    features: HashMap::new(),
                },
            )),
        }];
        let cwd = workspace_dir().join("project");
        let expected_cwd = cwd.to_string_lossy().into_owned();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server addr");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(ThreadConfigLoaderServer::new(TestServer {
                    sources,
                    expected_cwd,
                }))
                .serve_with_incoming_shutdown(
                    tokio_stream::wrappers::TcpListenerStream::new(listener),
                    async {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
        });
        let loaded = RemoteThreadConfigLoader::new(format!("http://{addr}"))
            .load(ThreadConfigContext {
                thread_id: Some("thread-1".to_string()),
                cwd: Some(cwd),
            })
            .await
            .expect("load thread config");
        let _ = shutdown_tx.send(());
        server.await.expect("join server").expect("server");

        let layer = config_layer(loaded.into_iter().next().expect("session source"))
            .expect("config layer")
            .expect("session layer");
        let parsed: HashMap<String, ModelProviderInfo> = layer
            .config
            .get("model_providers")
            .expect("model providers")
            .clone()
            .try_into()
            .expect("provider toml");
        assert_eq!(parsed.get("explicit"), Some(&explicit));
        assert_eq!(parsed.get("omitted"), Some(&omitted));
    }

    #[test]
    fn remote_compaction_false_and_unsupported_round_trip() {
        let provider = controlled_provider(false, false, Some(RemoteCompactionSupport::Unsupported));
        let proto = model_provider_to_proto("local", provider.clone());
        assert!(!proto.accepts_response_continuation);
        assert!(!proto.include_internal_metadata);
        assert_eq!(
            proto.remote_compaction,
            Some(proto::RemoteCompaction::Unsupported as i32)
        );
        let (id, actual) = model_provider_from_proto(proto).expect("provider");
        assert_eq!(id, "local");
        assert_eq!(actual, provider);
    }

    #[test]
    fn remote_compaction_rejects_unspecified_and_unknown_values() {
        let mut proto = model_provider_to_proto(
            "local",
            controlled_provider(false, false, None),
        );
        proto.remote_compaction = Some(proto::RemoteCompaction::Unspecified as i32);
        assert!(model_provider_from_proto(proto.clone()).is_err());
        proto.remote_compaction = Some(99);
        assert!(model_provider_from_proto(proto).is_err());
    }

    fn controlled_provider(
        continuation: bool,
        metadata: bool,
        remote_compaction: Option<RemoteCompactionSupport>,
    ) -> ModelProviderInfo {
        ModelProviderInfo {
            name: "OpenAI".to_string(),
            base_url: Some("https://configured.example/v1".to_string()),
            wire_api: WireApi::Responses,
            accepts_response_continuation: continuation,
            include_internal_metadata: metadata,
            capabilities: remote_compaction.map(|remote_compaction| ModelProviderCapabilities {
                external_web_access: None,
                remote_compaction: Some(remote_compaction),
            }),
            ..Default::default()
        }
    }

    fn workspace_dir() -> AbsolutePathBuf {
        AbsolutePathBuf::current_dir()
            .expect("current dir")
            .join("workspace")
    }
}
