pub mod cache;
pub mod collaboration_mode_presets;
pub(crate) mod config;
pub mod manager;
pub mod model_info;
pub mod model_presets;
pub mod multi_provider;
pub mod test_support;

pub use codex_protocol::auth::AuthMode;
pub use config::ModelsManagerConfig;

/// Convert the client version string to a whole version string (e.g. "1.2.3-alpha.4" -> "1.2.3").
pub fn client_version_to_whole() -> String {
    format!(
        "{}.{}.{}",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH")
    )
}
