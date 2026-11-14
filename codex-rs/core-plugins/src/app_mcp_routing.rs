use crate::manifest::PluginManifestFormat;

pub(crate) fn plugin_supports_apps(format: PluginManifestFormat) -> bool {
    // Keep portable app declarations inactive for Agent Plugins.
    format == PluginManifestFormat::Legacy
}
