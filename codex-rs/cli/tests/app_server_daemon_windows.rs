//! Exercises daemon installation from the Windows WinGet archive layout.
#![cfg(windows)]

use anyhow::Result;
use anyhow::ensure;
use codex_utils_cargo_bin::copy_executable;
use pretty_assertions::assert_eq;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use std::process::Command;

#[test]
fn winget_package_starts_and_reselects_daemon() -> Result<()> {
    if codex_app_server_daemon::is_elevated()? {
        eprintln!("requires a non-elevated Windows runner");
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let package = temp.path().join("WinGet/Packages/OpenAI.Steve");
    let target = format!("{}-pc-windows-msvc", std::env::consts::ARCH);
    let entrypoint = format!("codex-{target}.exe");
    let codex = package.join(&entrypoint);
    for directory in ["bin", "steve-path", "steve-resources/voice"] {
        std::fs::create_dir_all(package.join(directory))?;
    }
    copy_executable(&codex_utils_cargo_bin::cargo_bin("steve")?, &codex)?;
    for helper in [
        "steve-code-mode-host.exe",
        "steve-path/rg.exe",
        "steve-resources/steve-command-runner.exe",
        "steve-resources/steve-windows-sandbox-setup.exe",
    ] {
        std::fs::write(package.join(helper), b"runtime fixture")?;
    }
    let mut metadata = serde_json::json!({
        "layoutVersion": 1, "version": env!("CARGO_PKG_VERSION"),
        "target": target, "entrypoint": entrypoint
    });
    let manifest_bytes = serde_json::to_vec(&metadata)?;
    std::fs::write(package.join("steve-package.json"), &manifest_bytes)?;
    let checksum = format!("{:x}", Sha256::digest(std::fs::read(&codex)?));
    let voice_bytes = serde_json::to_vec(&serde_json::json!({
        "sha256": { &entrypoint: checksum, "steve-resources/voice/runtime": "untouched" }
    }))?;
    std::fs::write(
        package.join("steve-resources/voice/manifest.json"),
        &voice_bytes,
    )?;

    let current = home.join("packages/app-server-daemon/current");
    // Drop before temp so assertion failures also release the managed executable.
    struct StopDaemon(Command);
    impl Drop for StopDaemon {
        fn drop(&mut self) {
            let _ = self.0.output();
        }
    }
    let mut stop = Command::new(&codex);
    stop.env("STEVE_HOME", &home)
        .args(["app-server", "daemon", "stop"]);
    let mut daemon = StopDaemon(stop);
    for args in [
        vec!["start"],
        vec!["update", "--from-cli", "--yes"],
        vec!["update", "--from-cli", "--yes"],
    ] {
        let output = Command::new(&codex)
            .env("STEVE_HOME", &home)
            .args(["app-server", "daemon"])
            .args(&args)
            .output()?;
        ensure!(
            output.status.success(),
            "daemon {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let selected_metadata: Value =
        serde_json::from_slice(&std::fs::read(current.join("steve-package.json"))?)?;
    metadata["entrypoint"] = "bin/steve.exe".into();
    assert_eq!(selected_metadata, metadata);
    let selected_voice: Value = serde_json::from_slice(&std::fs::read(
        current.join("steve-resources/voice/manifest.json"),
    )?)?;
    assert_eq!(
        selected_voice,
        serde_json::json!({
            "sha256": {
                "bin/steve.exe": checksum,
                "steve-resources/voice/runtime": "untouched"
            }
        })
    );
    assert_eq!(
        std::fs::read(current.join("bin/steve-code-mode-host.exe"))?,
        b"runtime fixture"
    );
    let selected = current.canonicalize()?;
    std::fs::remove_file(package.join("steve-code-mode-host.exe"))?;
    let invalid = Command::new(&codex)
        .env("STEVE_HOME", &home)
        .args(["app-server", "daemon", "update", "--from-cli", "--yes"])
        .output()?;
    ensure!(
        !invalid.status.success(),
        "an incomplete WinGet package was accepted"
    );
    assert_eq!(current.canonicalize()?, selected);
    assert_eq!(
        std::fs::read(package.join("steve-package.json"))?,
        manifest_bytes
    );
    assert_eq!(
        std::fs::read(package.join("steve-resources/voice/manifest.json"))?,
        voice_bytes
    );
    let stopped = daemon.0.output()?;
    ensure!(
        stopped.status.success(),
        "failed to stop daemon: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    Ok(())
}
