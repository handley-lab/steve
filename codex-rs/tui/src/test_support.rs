//! Test-only helpers shared across the TUI crate.

use std::sync::LazyLock;

use codex_protocol::openai_models::ModelPreset;
pub(crate) use codex_utils_absolute_path::test_support::PathBufExt;
pub(crate) use codex_utils_absolute_path::test_support::test_path_buf;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub(crate) struct TestHome {
    pub(crate) home: tempfile::TempDir,
    pub(crate) _models: wiremock::MockServer,
}

impl TestHome {
    pub(crate) fn path(&self) -> &std::path::Path {
        self.home.path()
    }
}

pub(crate) static TEST_MODEL_PRESETS: LazyLock<Vec<ModelPreset>> = LazyLock::new(|| {
    let model = codex_models_manager::test_support::model_info("gpt-5.5");
    let mut presets = vec![ModelPreset::from(model)];
    ModelPreset::mark_default_by_picker_visibility(&mut presets);
    presets
});

pub(crate) fn test_path_display(path: &str) -> String {
    test_path_buf(path).display().to_string()
}

pub(crate) fn session_source_cli<T>() -> T
where
    T: DeserializeOwned,
{
    from_app_server_wire(codex_app_server_protocol::SessionSource::Cli)
}

pub(crate) fn skill_scope_user<T>() -> T
where
    T: DeserializeOwned,
{
    from_app_server_wire(codex_app_server_protocol::SkillScope::User)
}

pub(crate) fn skill_scope_repo<T>() -> T
where
    T: DeserializeOwned,
{
    from_app_server_wire(codex_app_server_protocol::SkillScope::Repo)
}

fn from_app_server_wire<T>(value: impl Serialize) -> T
where
    T: DeserializeOwned,
{
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .unwrap_or_else(|err| {
            panic!("app-server wire value should map to legacy helper type: {err}")
        })
}

pub(crate) use crate::boxed_future;

pub(crate) async fn provider_test_home() -> std::io::Result<TestHome> {
    let home = TestHome {
        home: tempfile::TempDir::new()?,
        _models: wiremock::MockServer::start().await,
    };
    wiremock::Mock::given(wiremock::matchers::path("/models"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
            codex_protocol::openai_models::ModelsResponse {
                models: vec![codex_models_manager::test_support::model_info("gpt-test")],
            },
        ))
        .mount(&home._models)
        .await;
    seed_test_provider(home.path(), &home)?;
    Ok(home)
}

// Persist the provider because embedded app servers reload their fixture home.
pub(crate) fn seed_test_provider(
    home: &std::path::Path,
    fixture: &TestHome,
) -> std::io::Result<()> {
    let config_path = home.join("config.toml");
    let existing = match std::fs::read_to_string(&config_path) {
        Ok(existing) => existing,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let config: toml::Value = toml::from_str(&existing).map_err(std::io::Error::other)?;
    if config.get("model_provider").is_some() {
        return Ok(());
    }
    let model = if config.get("model").is_none() {
        "model = \"gpt-test\"\n"
    } else {
        ""
    };
    std::fs::write(
        config_path,
        format!(
            r#"{model}model_provider = "test-provider"
{existing}
[model_providers.test-provider]
name = "Test provider"
base_url = "{0}/v1"
model_catalog_url = "{0}/models"
default_model = "gpt-test"
requires_openai_auth = false
"#,
            fixture._models.uri(),
        ),
    )
}

/// Keep a replacement fixture file on its existing mock provider and model.
pub(crate) fn seed_test_provider_from_config(
    path: &std::path::Path,
    config: &crate::legacy_core::config::Config,
) -> std::io::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(existing) => existing,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let parsed: toml::Value = toml::from_str(&existing).map_err(std::io::Error::other)?;
    let provider = config.model_provider_id.as_str();
    if parsed
        .get("model_provider")
        .and_then(toml::Value::as_str)
        .is_some_and(|selected| selected != provider)
    {
        return Ok(());
    }
    let mut prefix = String::new();
    if parsed.get("model_provider").is_none() {
        prefix.push_str(&format!(
            "model_provider = {}\n",
            toml::Value::String(provider.to_string())
        ));
    }
    if parsed.get("model").is_none()
        && let Some(model) = config.model.as_deref()
    {
        let model = crate::temporary_structured_request::native_model_id(model, provider);
        prefix.push_str(&format!(
            "model = {}\n",
            toml::Value::String(model.to_string())
        ));
    }
    let definition = if parsed
        .get("model_providers")
        .and_then(|providers| providers.get(provider))
        .is_none()
    {
        format!(
            "\n[model_providers.{}]\n{}",
            toml::Value::String(provider.to_string()),
            toml::to_string(&config.model_provider).map_err(std::io::Error::other)?
        )
    } else {
        String::new()
    };
    std::fs::write(path, format!("{prefix}{existing}{definition}"))
}
