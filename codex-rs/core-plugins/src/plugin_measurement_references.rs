//! Local files backing one plugin measurement reference.

use crate::TrustedPluginRoots;
use tempfile::TempDir;

/// Owns the immutable files backing its roots. Keep the reference alive for the
/// full command/turn that consumes those roots, including asynchronous commands.
pub struct PluginMeasurementReference {
    roots: TrustedPluginRoots,
    home: Option<TempDir>,
}

impl Drop for PluginMeasurementReference {
    fn drop(&mut self) {
        let Some(home) = self.home.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || drop(home));
        } else {
            drop(home);
        }
    }
}

impl PluginMeasurementReference {
    pub fn roots(&self) -> &TrustedPluginRoots {
        &self.roots
    }
}

#[cfg(test)]
#[path = "plugin_measurement_references_tests.rs"]
mod tests;
