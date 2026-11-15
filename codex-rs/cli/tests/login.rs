use std::path::Path;

use anyhow::Result;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("steve")?);
    cmd.env("STEVE_HOME", codex_home);
    Ok(cmd)
}

fn write_file_auth_config(codex_home: &Path) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        "cli_auth_credentials_store = \"file\"\n",
    )?;
    Ok(())
}

fn read_auth_json(codex_home: &Path) -> Result<Value> {
    let auth_json = std::fs::read_to_string(codex_home.join("auth.json"))?;
    Ok(serde_json::from_str(&auth_json)?)
}

#[test]
fn api_key_auth_commands_work_when_log_dir_is_unavailable() -> Result<()> {
    let codex_home = TempDir::new()?;
    let log_dir = codex_home.path().join("unavailable-logs");
    std::fs::write(&log_dir, "not a directory")?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!("cli_auth_credentials_store = \"file\"\nlog_dir = {log_dir:?}\n"),
    )?;

    codex_command(codex_home.path())?
        .args(["login", "--with-api-key"])
        .write_stdin("sk-test\n")
        .assert()
        .success()
        .stderr(contains("Warning: failed to create login log directory"));
    assert_eq!(
        read_auth_json(codex_home.path())?["OPENAI_API_KEY"],
        "sk-test"
    );

    codex_command(codex_home.path())?
        .args(["login", "status"])
        .assert()
        .success()
        .stderr(contains("API key"));

    codex_command(codex_home.path())?
        .arg("logout")
        .assert()
        .success()
        .stderr(contains("Successfully logged out"));
    assert!(!codex_home.path().join("auth.json").exists());

    Ok(())
}

#[test]
fn login_status_reports_auth_storage_errors() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    std::fs::write(codex_home.path().join("auth.json"), "{invalid json")?;

    codex_command(codex_home.path())?
        .args(["login", "status"])
        .assert()
        .failure()
        .stderr(contains("Error checking login status:"));

    Ok(())
}

#[test]
fn account_login_options_are_not_exposed() -> Result<()> {
    let codex_home = TempDir::new()?;
    for args in [
        vec!["login", "--device-auth"],
        vec!["login", "--experimental_issuer", "http://localhost:30000"],
        vec!["login", "--experimental_client-id", "client-id"],
    ] {
        codex_command(codex_home.path())?
            .args(args)
            .assert()
            .failure()
            .stderr(contains("unexpected argument"));
    }
    assert!(!codex_home.path().join("auth.json").exists());
    Ok(())
}

#[test]
fn login_requires_supplied_credentials() -> Result<()> {
    let codex_home = TempDir::new()?;
    codex_command(codex_home.path())?
        .arg("login")
        .assert()
        .failure()
        .stderr(contains("Supply credentials with --with-api-key."));
    assert!(!codex_home.path().join("auth.json").exists());
    Ok(())
}
