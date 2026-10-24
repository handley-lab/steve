use std::path::Path;

use anyhow::Result;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("steve")?);
    cmd.env("STEVE_HOME", codex_home);
    Ok(cmd)
}

#[test]
fn debug_models_default_prints_json_without_auth() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd
        .args([
            "-c",
            "model_provider=\"openai\"",
            "-c",
            "model=\"test-model\"",
            "-c",
            "features.api_key_model_discovery=false",
            "debug",
            "models",
        ])
        .output()?;

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    let value: serde_json::Value = serde_json::from_str(&stdout)?;
    assert!(value["models"].is_array());
    assert!(value["models"].as_array().unwrap().is_empty());

    Ok(())
}
