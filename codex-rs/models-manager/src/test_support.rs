//! Test-only helpers exposed for dependent crate tests.
//!
//! Production code should not depend on this module.

use crate::ModelsManagerConfig;
use crate::manager::construct_model_info_from_candidates;
use codex_protocol::openai_models::ModelInfo;

/// Get model identifier without consulting remote state or cache.
pub fn get_model_offline_for_tests(model: Option<&str>) -> String {
    model.unwrap_or("gpt-5.5").to_string()
}

/// Build `ModelInfo` without consulting remote state or cache.
pub fn construct_model_info_offline_for_tests(
    model: &str,
    config: &ModelsManagerConfig,
) -> ModelInfo {
    let candidates: &[ModelInfo] = if let Some(model_catalog) = config.model_catalog.as_ref() {
        &model_catalog.models
    } else {
        &[]
    };
    construct_model_info_from_candidates(model, candidates, config)
}

/// Construct one visible catalogue entry for a test-owned model.
pub fn model_info(slug: &str) -> ModelInfo {
    let mut model = crate::model_info::model_info_from_slug(slug);
    model.visibility = codex_protocol::openai_models::ModelVisibility::List;
    model.used_fallback_model_metadata = false;
    model
}
