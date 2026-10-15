use super::PluginMeasurementReference;
use crate::TrustedPluginRoots;
use anyhow::Result;
use std::path::Path;
use std::time::Duration;

async fn wait_until_removed(path: &Path) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}

#[test]
fn final_store_cleanup_runs_outside_the_async_worker() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let blocker = tokio::task::spawn_blocking(move || {
            let _ = started.send(());
            let _ = wait.recv();
        });
        ready.await?;
        let home = tempfile::tempdir()?;
        let path = home.path().to_owned();
        drop(PluginMeasurementReference {
            roots: TrustedPluginRoots::default(),
            home: Some(home),
        });
        assert!(path.exists(), "cleanup must wait for the blocking worker");
        release.send(())?;
        blocker.await?;
        wait_until_removed(&path).await
    })?;

    // References may also outlive their runtime and still need ordinary cleanup.
    let home = tempfile::tempdir()?;
    let path = home.path().to_owned();
    drop(runtime);
    drop(PluginMeasurementReference {
        roots: TrustedPluginRoots::default(),
        home: Some(home),
    });
    assert!(!path.exists());
    Ok(())
}
