#[cfg(any(unix, windows))]
use std::process::Stdio;
use std::time::Duration;

use pretty_assertions::assert_eq;
use tempfile::TempDir;
#[cfg(any(unix, windows))]
use tokio::time::sleep;

use super::PidBackend;
use super::PidCommandKind;
use super::PidLogTail;
use super::PidRecord;
#[cfg(unix)]
use super::read_process_start_time;
use super::read_stderr_log_tail;
use super::stderr_log_file_for_pid_file;

#[cfg(windows)]
fn is_elevated_test_process() -> anyhow::Result<bool> {
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)",
        ])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "administrator membership query failed"
    );
    match String::from_utf8(output.stdout)?.trim() {
        "True" => Ok(true),
        "False" => Ok(false),
        other => anyhow::bail!("unexpected administrator membership: {other}"),
    }
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn shutdown_grace_child() {
    let Some(ready) = std::env::var_os("STEVE_TEST_SHUTDOWN_GRACE_READY") else {
        return;
    };
    let ready = std::path::PathBuf::from(ready);
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install shutdown handler");
    tokio::fs::write(&ready, "").await.expect("ready marker");
    // Outlive the parent's stop deadline so this watchdog cannot look like a forced stop.
    let _ = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        #[cfg(unix)]
        terminate.recv().await;
        #[cfg(windows)]
        loop {
            if ready.with_extension("shutdown").exists() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
        if std::env::var_os("STEVE_TEST_SHUTDOWN_GRACE_EXIT").is_some() {
            sleep(Duration::from_millis(150)).await;
            tokio::fs::write(ready.with_extension("exited"), "")
                .await
                .expect("graceful exit marker");
            return;
        }
        std::future::pending::<()>().await;
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_updater_signals_its_installer_process_group() {
    use std::os::unix::process::CommandExt;

    let temp = TempDir::new().expect("temp dir");
    let ready = temp.path().join("installer.ready");
    let stopped = temp.path().join("installer.stopped");
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args([
            "-c",
            "sh -c 'trap \"touch $STOP_MARKER; exit 0\" TERM; touch $READY_MARKER; while :; do sleep 1; done' & wait",
        ])
        .env("READY_MARKER", &ready)
        .env("STOP_MARKER", &stopped)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn updater shim");
    let pid = child.id();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !ready.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "installer did not start"
        );
        sleep(Duration::from_millis(10)).await;
    }
    let pid_file = temp.path().join("updater.pid");
    tokio::fs::write(
        &pid_file,
        serde_json::to_vec(&PidRecord {
            pid,
            process_start_time: read_process_start_time(pid).await.expect("start time"),
            process_identity: None,
            executable_identity: None,
        })
        .expect("serialize pid"),
    )
    .await
    .expect("write pid file");
    let backend = PidBackend::new_update_loop(
        temp.path().join("steve"),
        pid_file,
        /*restore_release*/ None,
    );
    backend.stop().await.expect("stop updater");
    // The backend normally reaps the shim, so a second wait may return ECHILD.
    let _ = child.wait();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !stopped.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "installer was not stopped"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn exited_unreaped_updater_is_reaped() {
    let temp = TempDir::new().expect("temp dir");
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn updater shim");
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let process_identity = Some(
        super::identity::read_process_details(child.id())
            .await
            .unwrap()
            .1,
    );
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let process_identity = None;
    let record = PidRecord {
        pid: child.id(),
        process_start_time: read_process_start_time(child.id())
            .await
            .expect("start time"),
        process_identity,
        executable_identity: None,
    };
    let backend = PidBackend::new_update_loop(
        temp.path().join("steve"),
        temp.path().join("updater.pid"),
        /*restore_release*/ None,
    );
    child.kill().expect("terminate updater shim");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let result = loop {
        let result = backend.record_is_active(&record).await;
        if matches!(&result, Ok(false)) || tokio::time::Instant::now() >= deadline {
            break result;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(!result.expect("check zombie updater"));
    assert_eq!(
        child
            .wait()
            .expect_err("updater shim was already reaped")
            .raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn update_loop_uses_hidden_app_server_subcommand() {
    let backend = PidBackend {
        feature_overrides: Default::default(),
        codex_bin: "steve".into(),
        pid_file: "updater.pid".into(),
        lock_file: "updater.pid.lock".into(),
        command_kind: PidCommandKind::UpdateLoop {
            restore_release: None,
        },
    };

    assert_eq!(
        backend.command_args(),
        vec!["app-server", "daemon", "pid-update-loop"]
    );
}

#[tokio::test]
async fn read_stderr_log_tail_returns_recent_complete_lines() {
    let temp_dir = TempDir::new().expect("temp dir");
    let pid_file = temp_dir.path().join("app-server.pid");
    let log_file = stderr_log_file_for_pid_file(&pid_file);
    let contents = format!("{}\nrecent error\nusage", "x".repeat(4100));
    tokio::fs::write(&log_file, contents)
        .await
        .expect("write stderr log");

    assert_eq!(
        read_stderr_log_tail(&pid_file)
            .await
            .expect("read stderr log"),
        Some(PidLogTail {
            path: log_file,
            contents: "recent error\nusage".to_string(),
        })
    );
}

#[cfg(windows)]
#[tokio::test]
async fn managed_children_launch_in_workdir_without_changing_private_state_directory() {
    if is_elevated_test_process().expect("query administrator membership") {
        return;
    }
    let temp = TempDir::new().expect("temp dir");
    let state_dir = temp.path().join("state");
    let codex_bin = temp.path().join("codex.cmd");
    tokio::fs::write(
        &codex_bin,
        "@echo off\r\n\
         if \"%3\"==\"--help\" exit /b 1\r\n\
         echo ready > launched.cwd\r\n\
         if defined STEVE_DAEMON_SHUTDOWN_FILE (for %%F in (\"%STEVE_DAEMON_SHUTDOWN_FILE%\") do type nul > \"%%~dpnF.ready\")\r\n\
         for /l %%i in (1,1,10000000) do @rem\r\n",
    )
    .await
    .expect("write daemon fixture");
    let backends = [
        PidBackend::new(
            codex_bin.clone(),
            state_dir.join("app-server.pid"),
            /*remote_control_enabled*/ false,
        ),
        PidBackend::new_update_loop(
            codex_bin,
            state_dir.join("updater.pid"),
            /*restore_release*/ None,
        ),
    ];
    for backend in backends {
        backend.start().await.expect("launch managed child");
        let marker = state_dir.join("workdir/launched.cwd");
        let launched_in_workdir = tokio::time::timeout(Duration::from_secs(5), async {
            while !marker.exists() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        backend
            .stop_with_grace(/*grace_seconds*/ 0)
            .await
            .expect("stop managed child");
        launched_in_workdir.expect("child did not write its relative marker in workdir");
        codex_uds::prepare_private_socket_directory(&state_dir)
            .await
            .expect("state directory remains private");
        assert!(!backend.pid_file.exists());
        tokio::fs::remove_file(marker)
            .await
            .expect("remove marker before next child launch");
    }
}

#[cfg(windows)]
#[tokio::test]
async fn failed_updater_handoff_preserves_predecessor_record() {
    let elevated = is_elevated_test_process().expect("query administrator membership");
    let temp = TempDir::new().expect("temp");
    let state_dir = temp.path().join("state");
    codex_uds::prepare_private_socket_directory(&state_dir)
        .await
        .expect("private state directory");
    let backend = PidBackend::new_update_loop(
        temp.path().join("missing-steve.exe"),
        state_dir.join("updater.pid"),
        /*restore_release*/ None,
    );
    let record = PidRecord {
        pid: std::process::id(),
        process_start_time: super::read_process_start_time(std::process::id())
            .await
            .unwrap(),
        process_identity: None,
        executable_identity: None,
    };
    for record in [
        record.clone(),
        PidRecord {
            process_start_time: "stale".into(),
            ..record
        },
    ] {
        tokio::fs::write(&backend.pid_file, serde_json::to_vec(&record).unwrap())
            .await
            .unwrap();
        let error = backend.replace_current_updater().await.unwrap_err();
        if elevated {
            assert!(error.to_string().contains("non-elevated terminal"));
        } else {
            // Windows may reject breakaway before reporting the missing executable.
            // Both must reach process I/O rather than rejecting stale ownership.
            assert!(error.root_cause().is::<std::io::Error>(), "{error:#}");
        }
        assert_eq!(
            backend.read_pid_file_state().await.unwrap(),
            PidFileState::Running(record)
        );
    }
}

#[cfg(windows)]
#[tokio::test]
async fn updater_readiness_and_post_publication_failure_preserve_ownership() {
    use futures::FutureExt;
    use tokio::io::AsyncWriteExt;
    use windows_sys::Win32::Security::ImpersonateAnonymousToken;
    use windows_sys::Win32::Security::RevertToSelf;
    use windows_sys::Win32::System::Threading::GetCurrentThread;

    let temp = TempDir::new().expect("temp");
    let backend = PidBackend::new_update_loop(
        temp.path().join("steve.exe"),
        temp.path().join("updater.pid"),
        /*restore_release*/ None,
    );
    let _lock = backend
        .acquire_reservation_lock()
        .await
        .expect("reservation lock");
    let mut predecessor_stderr = backend.open_stderr_log().await.expect("predecessor log");
    predecessor_stderr
        .write_all(&vec![b'x'; 512 * 1024])
        .await
        .unwrap();
    predecessor_stderr.flush().await.unwrap();
    let successor_stderr = backend.open_stderr_log().await.expect("successor log");
    let ready = backend.pid_file.with_extension("ready");
    let mut child = tokio::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::Error.WriteLine('successor startup'); [IO.File]::WriteAllText($env:STEVE_TEST_UPDATER_READY, ''); Start-Sleep 60",
        ])
        .env("STEVE_TEST_UPDATER_READY", &ready)
        .stderr(Stdio::from(successor_stderr.into_std().await))
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .kill_on_drop(true)
        .spawn()
        .expect("successor fixture");
    let pid = child.id().expect("pid");
    let successor = PidRecord {
        pid,
        process_start_time: super::read_process_start_time(pid)
            .await
            .expect("creation time"),
        process_identity: None,
        executable_identity: None,
    };
    let predecessor = PidRecord {
        pid: std::process::id(),
        process_start_time: super::read_process_start_time(std::process::id())
            .await
            .expect("creation time"),
        process_identity: None,
        executable_identity: None,
    };
    tokio::fs::write(&backend.pid_file, serde_json::to_vec(&successor).unwrap())
        .await
        .unwrap();
    backend
        .finish_updater_start(&successor, Some(&predecessor))
        .await
        .expect("ready successor");
    predecessor_stderr
        .write_all(b"predecessor handoff complete\n")
        .await
        .unwrap();
    predecessor_stderr.flush().await.unwrap();
    let log_path = stderr_log_file_for_pid_file(&backend.pid_file);
    let log = tokio::fs::read(&log_path).await.unwrap();
    assert!(log.len() < 256 * 1024);
    let log = String::from_utf8(log).expect("no stale-offset gap");
    assert!(log.contains("successor startup"));
    assert!(log.contains("predecessor handoff complete"));
    assert!(!ready.exists());
    assert_eq!(
        backend.read_pid_file_state().await.unwrap(),
        PidFileState::Running(successor.clone())
    );

    // If cleanup cannot even query the successor, leave its ownership intact.
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
                let result = backend
                    .finish_updater_start(&successor, Some(&predecessor))
                    .now_or_never();
                let reverted = unsafe { RevertToSelf() };
                assert_ne!(reverted, 0);
                let error = result
                    .expect("denied cleanup must not suspend")
                    .unwrap_err();
                assert_eq!(
                    error
                        .downcast_ref::<std::io::Error>()
                        .unwrap()
                        .raw_os_error(),
                    Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32)
                );
            })
            .join()
            .expect("anonymous cleanup check");
    });
    assert_eq!(
        backend.read_pid_file_state().await.unwrap(),
        PidFileState::Running(successor.clone())
    );

    let reused_pid = PidRecord {
        process_start_time: "stale".into(),
        ..successor.clone()
    };
    assert!(
        backend
            .finish_updater_start(&reused_pid, Some(&predecessor))
            .await
            .is_err()
    );
    assert_eq!(
        backend.read_pid_file_state().await.unwrap(),
        PidFileState::Running(predecessor.clone())
    );
    assert!(
        child.try_wait().unwrap().is_none(),
        "PID reuse must not terminate another process"
    );
    tokio::fs::write(&backend.pid_file, serde_json::to_vec(&successor).unwrap())
        .await
        .unwrap();
    // An invalid readiness path triggers cleanup while the successor is alive.
    tokio::fs::create_dir(&ready).await.unwrap();
    assert!(
        backend
            .finish_updater_start(&successor, Some(&predecessor))
            .await
            .is_err()
    );
    assert!(
        child.try_wait().unwrap().is_some(),
        "successor must exit before rollback"
    );
    assert_eq!(
        backend.read_pid_file_state().await.unwrap(),
        PidFileState::Running(predecessor)
    );
}

#[cfg(windows)]
#[test]
fn inaccessible_pid_preserves_identity_check_error() {
    use futures::FutureExt;
    use windows_sys::Win32::Security::ImpersonateAnonymousToken;
    use windows_sys::Win32::Security::RevertToSelf;
    use windows_sys::Win32::System::Threading::GetCurrentThread;

    // Isolate impersonation from Tokio workers and other tests. Query our own
    // process anonymously instead of assuming a system PID is inaccessible.
    std::thread::spawn(|| {
        let record = PidRecord {
            pid: std::process::id(),
            process_start_time: "stale".into(),
            process_identity: None,
            executable_identity: None,
        };
        assert!(
            crate::backend::windows::Process::open(record.pid)
                .unwrap()
                .is_some()
        );
        assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
        let opened = crate::backend::windows::Process::open(record.pid);
        // Windows identity checks are synchronous; never suspend while impersonating.
        let matches = super::process_matches_record(&record).now_or_never();
        let reverted = unsafe { RevertToSelf() };
        assert_ne!(reverted, 0);

        let error = match opened {
            Err(error) => error,
            Ok(_) => panic!("anonymous caller unexpectedly queried the test process"),
        };
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32),
        );
        assert_eq!(
            matches
                .expect("identity check must not suspend")
                .unwrap_err()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32),
        );
    })
    .join()
    .expect("anonymous identity check");
}

#[tokio::test]
async fn stderr_handoff_appends_after_truncation_without_gaps() -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let home = TempDir::new()?;
    let backend = PidBackend::new_update_loop(
        home.path().join("steve"),
        home.path().join("updater.pid"),
        /*restore_release*/ None,
    );
    let mut predecessor = backend.open_stderr_log().await?;
    predecessor.write_all(&vec![b'x'; 512 * 1024]).await?;
    predecessor.flush().await?;
    let mut successor = backend.open_stderr_log().await?;
    successor.write_all(b"successor startup failure\n").await?;
    successor.flush().await?;
    predecessor
        .write_all(b"predecessor handoff complete\n")
        .await?;
    predecessor.flush().await?;
    assert_eq!(
        tokio::fs::read(stderr_log_file_for_pid_file(&backend.pid_file)).await?,
        b"successor startup failure\npredecessor handoff complete\n"
    );
    Ok(())
}
