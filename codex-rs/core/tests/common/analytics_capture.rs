use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;

use serde_json::Value;

pub fn analytics_capture_path(codex_home: &Path) -> PathBuf {
    codex_home.join("analytics-events.jsonl")
}

fn capture_paths() -> &'static Mutex<HashMap<String, PathBuf>> {
    static PATHS: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    PATHS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_analytics_capture(server_uri: &str, codex_home: &Path) -> PathBuf {
    let path = analytics_capture_path(codex_home);
    capture_paths()
        .lock()
        .expect("analytics capture map")
        .insert(server_uri.to_string(), path.clone());
    path
}

pub fn analytics_capture_path_for(server_uri: &str) -> PathBuf {
    capture_paths()
        .lock()
        .expect("analytics capture map")
        .get(server_uri)
        .cloned()
        .unwrap_or_else(|| panic!("analytics capture was not mounted for {server_uri}"))
}

pub fn read_analytics_lines(path: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

pub fn read_analytics_events(path: &Path) -> Vec<Value> {
    read_analytics_lines(path)
        .into_iter()
        .flat_map(|payload| payload["events"].as_array().cloned().unwrap_or_default())
        .collect()
}
