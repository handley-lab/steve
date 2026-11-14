use codex_utils_absolute_path::AbsolutePathBuf;
use tempfile::TempDir;

use super::has_only_search_config_override;
use super::should_delay_startup_composer_for_first_login;

#[test]
fn startup_keeps_composer_when_home_state_cannot_be_confirmed() -> std::io::Result<()> {
    let temporary_directory = TempDir::new()?;
    let codex_home = temporary_directory.path().join("codex-home");
    let system_config_path =
        AbsolutePathBuf::from_absolute_path(temporary_directory.path().join("system.toml"))?;

    std::fs::create_dir(&codex_home)?;
    assert!(should_delay_startup_composer_for_first_login(
        &codex_home,
        Ok(system_config_path),
        || Ok(false),
    ));

    #[cfg(unix)]
    {
        let system_config_path =
            AbsolutePathBuf::from_absolute_path(temporary_directory.path().join("system.toml"))?;
        let daemon_directory = codex_home.join("app-server-control");
        std::fs::write(&daemon_directory, "not a directory")?;
        assert!(!should_delay_startup_composer_for_first_login(
            &codex_home,
            Ok(system_config_path.clone()),
            || panic!("ambiguous daemon state should not probe managed configuration"),
        ));
        std::fs::remove_file(&daemon_directory)?;

        let blocking_parent = temporary_directory.path().join("blocking-home-parent");
        std::fs::write(&blocking_parent, "not a directory")?;
        assert!(!should_delay_startup_composer_for_first_login(
            &blocking_parent.join("codex-home"),
            Ok(system_config_path),
            || panic!("ambiguous home state should not probe managed configuration"),
        ));
    }
    Ok(())
}

#[test]
fn startup_only_accepts_the_synthetic_search_override() {
    let search = (
        "web_search".to_string(),
        toml::Value::String("live".to_string()),
    );

    assert!(!has_only_search_config_override(&[]));
    assert!(has_only_search_config_override(std::slice::from_ref(
        &search
    )));
    assert!(!has_only_search_config_override(&[(
        "web_search".to_string(),
        toml::Value::String("cached".to_string()),
    )]));
    assert!(!has_only_search_config_override(&[(
        "model_provider".to_string(),
        toml::Value::String("live".to_string()),
    )]));
    assert!(!has_only_search_config_override(&[
        search,
        (
            "model".to_string(),
            toml::Value::String("custom".to_string())
        ),
    ]));
}

#[test]
fn startup_keeps_composer_when_system_configuration_is_possible() -> std::io::Result<()> {
    let temporary_directory = TempDir::new()?;
    let codex_home = temporary_directory.path().join("codex-home");
    let system_config_path =
        AbsolutePathBuf::from_absolute_path(temporary_directory.path().join("system.toml"))?;

    assert!(!should_delay_startup_composer_for_first_login(
        &codex_home,
        Err(std::io::Error::other(
            "system configuration path is unavailable"
        )),
        || Ok(false),
    ));

    #[cfg(unix)]
    {
        let blocking_parent = temporary_directory.path().join("blocking-parent");
        std::fs::write(&blocking_parent, "not a directory")?;
        assert!(!should_delay_startup_composer_for_first_login(
            &codex_home,
            AbsolutePathBuf::from_absolute_path(blocking_parent.join("system.toml")),
            || Ok(false),
        ));
    }

    std::fs::write(system_config_path.as_path(), "model_provider = 'local'")?;
    assert!(!should_delay_startup_composer_for_first_login(
        &codex_home,
        Ok(system_config_path),
        || Ok(false),
    ));
    Ok(())
}

#[test]
fn startup_keeps_composer_when_managed_configuration_is_possible() -> std::io::Result<()> {
    let temporary_directory = TempDir::new()?;
    let codex_home = temporary_directory.path().join("codex-home");
    let system_config_path =
        AbsolutePathBuf::from_absolute_path(temporary_directory.path().join("system.toml"))?;

    assert!(!should_delay_startup_composer_for_first_login(
        &codex_home,
        Ok(system_config_path.clone()),
        || Ok(true),
    ));
    assert!(!should_delay_startup_composer_for_first_login(
        &codex_home,
        Ok(system_config_path),
        || Err(std::io::Error::other(
            "managed configuration is inaccessible"
        )),
    ));
    Ok(())
}
