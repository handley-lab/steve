#[cfg(target_os = "macos")]
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

use anyhow::Context as _;
use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::write_chatgpt_auth;
use codex_config::types::AuthCredentialsStoreMode;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[cfg(target_os = "macos")]
#[test]
fn doctor_reports_macos_system_proxy_configuration_and_policy() -> Result<()> {
    let codex_home = TempDir::new()?;
    let report = doctor_report(codex_home.path())?;
    let details = &report["checks"]["network.env"]["details"];

    assert_eq!(details["respect system proxy"], json!("disabled"));
    assert!(matches!(
        details["system proxy"].as_str(),
        Some("automatic (PAC)" | "manual" | "direct" | "unavailable")
    ));

    std::fs::write(
        codex_home.path().join("config.toml"),
        "[features]\nrespect_system_proxy = true\n",
    )?;
    let report = doctor_report(codex_home.path())?;
    assert_eq!(
        report["checks"]["network.env"]["details"]["respect system proxy"],
        json!("enabled")
    );

    Ok(())
}

#[cfg(target_os = "macos")]
fn doctor_report(codex_home: &Path) -> Result<Value> {
    let output = Command::new(codex_utils_cargo_bin::cargo_bin("steve")?)
        .args(["doctor", "--json"])
        .env("STEVE_HOME", codex_home)
        .stdin(Stdio::null())
        .output()
        .context("failed to run the doctor")?;

    serde_json::from_slice(&output.stdout).context("doctor did not emit a valid json report")
}
