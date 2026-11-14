use crate::config::ConfigBuilder;
use std::fs;
use std::path::Path;

pub(crate) fn write_file(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("file should have a parent")).unwrap();
    fs::write(path, contents).unwrap();
}

pub(crate) async fn load_plugins_config(codex_home: &Path) -> crate::config::Config {
    ConfigBuilder::default()
        .codex_home(codex_home.to_path_buf())
        .fallback_cwd(Some(codex_home.to_path_buf()))
        .build()
        .await
        .expect("config should load")
}
