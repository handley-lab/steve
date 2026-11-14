use std::sync::Arc;

use codex_config::LoaderOverrides;
use codex_core::config::Config;
use codex_core::config::ConfigBuilder;
use codex_core::config::TokenBudgetConfig;
use codex_extension_api::ConversationHistory;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionRegistry;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::NoopTurnItemEmitter;
use codex_extension_api::ThreadStartInput;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolCallSource;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolName;
use codex_extension_api::ToolPayload;
use codex_history_notes_extension::install;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TruncationPolicy;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

type TestResult = Result<(), Box<dyn std::error::Error>>;
const THREAD_HINT: &str = "Recent notes (up to 5, most-recent first):\n- /root/worker/notes/latest.md (2 lines, 14 UTF-8 bytes)";

#[tokio::test]
async fn history_notes_use_explicitly_enabled_configured_providers() -> TestResult {
    for custom_provider in [false, true] {
        let server = MockServer::start().await;
        let token = if custom_provider {
            "custom-notes-key"
        } else {
            "test-api-key"
        };
        for (route, response) in [
            ("thread_hint", json!({"text": THREAD_HINT})),
            ("read_file", json!({"text": "local note"})),
        ] {
            Mock::given(method("POST"))
                .and(path(format!("/v1/alpha/notes/v2/{route}")))
                .and(header("authorization", format!("Bearer {token}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(response))
                .expect(1)
                .mount(&server)
                .await;
        }
        let codex_home = TempDir::new()?;
        std::fs::write(
            codex_home.path().join("config.toml"),
            "model_provider = 'notes-test'\n[model_providers.notes-test]\nname = 'Configured notes'\nrequires_openai_auth = true\n",
        )?;
        let mut config = ConfigBuilder::default()
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .codex_home(codex_home.path().to_path_buf())
            .build()
            .await?;
        config.model_provider.base_url = Some(format!("{}/v1", server.uri()));
        if custom_provider {
            config.model_provider.name = "Local notes".into();
            config.model_provider.requires_openai_auth = false;
            config.model_provider.experimental_bearer_token = Some(token.into());
        }
        let mut builder = ExtensionRegistryBuilder::<Config>::new();
        install(
            &mut builder,
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test-api-key")),
        );
        let registry = builder.build();
        let session_store = ExtensionData::new("session-123");
        let thread_store = ExtensionData::new("thread-123");
        for contributor in registry.thread_lifecycle_contributors() {
            contributor
                .on_thread_start(ThreadStartInput {
                    config: &config,
                    session_source: &SessionSource::Cli,
                    persistent_thread_state_available: true,
                    environments: &[],
                    mcp_resource_client: None,
                    extension_metrics: None,
                    session_store: &session_store,
                    thread_store: &thread_store,
                })
                .await;
        }
        assert!(exposed_tools(&registry, &session_store, &thread_store).is_empty());
        assert!(
            registry.context_contributors()[0]
                .contribute_thread_context(&session_store, &thread_store)
                .await
                .is_empty()
        );
        assert!(server.received_requests().await.unwrap().is_empty());

        let mut enabled = config.clone();
        enabled.token_budget = Some(TokenBudgetConfig {
            use_history_notes_extension: true,
            ..TokenBudgetConfig::default()
        });
        for contributor in registry.config_contributors() {
            contributor.on_config_changed(&session_store, &thread_store, &config, &enabled);
        }
        let tools = exposed_tools(&registry, &session_store, &thread_store);
        let tool = tools
            .iter()
            .find(|tool| tool.tool_name() == ToolName::namespaced("notes", "read_file"))
            .expect("configured notes provider should expose read_file");
        let output = tool
            .handle(tool_call(
                ToolName::namespaced("notes", "read_file"),
                json!({"path":"notes.md"}),
            ))
            .await?;
        assert_eq!(
            output.log_output(),
            json!({"text":"local note"}).to_string()
        );
        let hints = registry.context_contributors()[0]
            .contribute_thread_context(&session_store, &thread_store)
            .await;
        assert_eq!(hints.len(), 1);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests {
            let body: serde_json::Value = serde_json::from_slice(&request.body)?;
            assert_eq!(
                body["context"],
                json!({"session_id":"session-123", "current_agent_name":"/root"})
            );
        }
        for contributor in registry.config_contributors() {
            contributor.on_config_changed(&session_store, &thread_store, &enabled, &config);
        }
        assert!(exposed_tools(&registry, &session_store, &thread_store).is_empty());
        assert!(
            registry.context_contributors()[0]
                .contribute_thread_context(&session_store, &thread_store)
                .await
                .is_empty()
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        server.verify().await;
    }
    Ok(())
}

fn exposed_tools(
    registry: &ExtensionRegistry<Config>,
    session_store: &ExtensionData,
    thread_store: &ExtensionData,
) -> Vec<Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>> {
    registry
        .tool_contributors()
        .iter()
        .flat_map(|contributor| contributor.tools(session_store, thread_store))
        .collect()
}

fn tool_call(tool_name: ToolName, arguments: serde_json::Value) -> ToolCall<'static> {
    ToolCall {
        turn_id: "turn-1".to_string(),
        call_id: "call-read-file".to_string(),
        tool_name,
        model: "gpt-test".to_string(),
        codex_turn_metadata: None,
        truncation_policy: TruncationPolicy::Bytes(1024),
        source: ToolCallSource::Direct,
        conversation_history: ConversationHistory::default(),
        turn_item_emitter: Arc::new(NoopTurnItemEmitter),
        attachment_store: codex_core::passthrough_image_store(),
        ephemeral: false,
        environments: Vec::new(),
        payload: ToolPayload::Function {
            arguments: arguments.to_string(),
        },
    }
}
