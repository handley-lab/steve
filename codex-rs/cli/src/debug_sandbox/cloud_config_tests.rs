use anyhow::Result;
use codex_config::LoaderOverrides;

use super::super::DebugSandboxConfigOptions;
use super::super::ManagedRequirementsMode;
use super::bootstrap_cloud_config_bundle;

#[tokio::test]
async fn sandbox_state_skips_cloud_bootstrap() -> Result<()> {
    let options = DebugSandboxConfigOptions {
        sandbox_state: crate::SandboxStateArgs {
            sandbox_state_json: Some("{}".into()),
            ..Default::default()
        },
        permissions_profile: None,
        cwd: None,
        managed_requirements_mode: ManagedRequirementsMode::Include,
        loader_overrides: LoaderOverrides::without_managed_config_for_tests(),
    };
    bootstrap_cloud_config_bundle(
        &[],
        &options,
        || panic!("supplied state must skip STEVE_HOME resolution"),
        /*strict_config*/ false,
    )
    .await?;
    Ok(())
}
