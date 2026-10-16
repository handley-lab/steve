use codex_extension_api::FunctionCallError;
use codex_extension_api::ResponsesApiTool;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolName;
use codex_extension_api::ToolOutput;
use codex_extension_api::ToolSpec;
use codex_extension_api::parse_tool_input_schema_without_compaction;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolExposure;
use codex_tools::default_namespace_description;

use crate::schema::commands_schema;

pub(crate) const WEB_NAMESPACE: &str = "web";
pub(crate) const RUN_TOOL_NAME: &str = "run";
const WEB_RUN_DESCRIPTION: &str = include_str!("../web_run_description.md");

pub(crate) struct WebSearchTool;

impl<'call> ToolExecutor<ToolCall<'call>> for WebSearchTool {
    fn tool_name(&self) -> ToolName {
        ToolName::namespaced(WEB_NAMESPACE, RUN_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        // parse schema without compaction that removes field metadata/descriptions to match hosted tool definition
        let parameters = match parse_tool_input_schema_without_compaction(&commands_schema()) {
            Ok(parameters) => parameters,
            Err(err) => panic!("search command schema should parse: {err}"),
        };

        ToolSpec::Namespace(ResponsesApiNamespace {
            name: WEB_NAMESPACE.to_string(),
            description: default_namespace_description(WEB_NAMESPACE),
            tools: vec![ResponsesApiNamespaceTool::Function(ResponsesApiTool {
                name: RUN_TOOL_NAME.to_string(),
                description: WEB_RUN_DESCRIPTION.to_string(),
                strict: false,
                parameters,
                output_schema: None,
                defer_loading: None,
            })],
        })
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::Direct
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    fn handle<'a>(&'a self, call: ToolCall<'call>) -> codex_extension_api::ToolExecutorFuture<'a>
    where
        'call: 'a,
    {
        Box::pin(self.handle_call(call))
    }
}

impl WebSearchTool {
    async fn handle_call(
        &self,
        _call: ToolCall<'_>,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let _ = self;
        Err(FunctionCallError::Fatal(
            "web search is not available".to_string(),
        ))
    }
}
