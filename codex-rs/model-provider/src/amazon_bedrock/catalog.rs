use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::WebSearchToolType;

pub(super) fn normalize_bedrock_catalog(mut catalog: ModelsResponse) -> ModelsResponse {
    // Custom catalogs own their tier definitions; only normalize wire compatibility here.
    for model in &mut catalog.models {
        // Bedrock rejects the `search_content_types` field used by multimodal search.
        model.web_search_tool_type = WebSearchToolType::Text;
    }
    catalog
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::MultiAgentVersion;
    use pretty_assertions::assert_eq;

    #[test]
    fn configured_bedrock_catalogs_normalize_unsupported_model_capabilities() {
        let models = [
            Some(MultiAgentVersion::V2),
            Some(MultiAgentVersion::V1),
            Some(MultiAgentVersion::Disabled),
            None,
        ]
        .into_iter()
        .map(|version| {
            let mut model = codex_models_manager::test_support::model_info("configured-model");
            model.multi_agent_version = version;
            model.web_search_tool_type = WebSearchToolType::TextAndImage;
            model
        })
        .collect::<Vec<_>>();
        let mut expected = models.clone();
        for model in &mut expected {
            model.web_search_tool_type = WebSearchToolType::Text;
        }
        assert_eq!(
            normalize_bedrock_catalog(ModelsResponse { models }),
            ModelsResponse { models: expected }
        );
    }
}
