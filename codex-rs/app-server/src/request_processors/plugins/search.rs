use super::*;
use codex_app_server_protocol::PluginSearchParams;
use codex_app_server_protocol::PluginSearchResponse;
use codex_app_server_protocol::PluginSearchResult;
use codex_app_server_protocol::PluginSearchScope;
use codex_core_plugins::OPENAI_BUNDLED_MARKETPLACE_NAME;

const MAX_LOCAL_PLUGIN_SEARCH_RESULTS: usize = 100;

impl PluginRequestProcessor {
    pub(crate) async fn plugin_search(
        &self,
        params: PluginSearchParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.plugin_search_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    async fn plugin_search_response(
        &self,
        params: PluginSearchParams,
    ) -> Result<PluginSearchResponse, JSONRPCErrorError> {
        let PluginSearchParams {
            search_term,
            scope,
            cwds,
            cursor,
            limit: _,
        } = params;
        let search_term = search_term.trim();
        let empty_response = || PluginSearchResponse {
            data: Vec::new(),
            next_cursor: None,
        };
        if search_term.is_empty() {
            return Ok(empty_response());
        }

        let config = self
            .load_catalog_config(cwds.as_deref().unwrap_or_default())
            .await?;
        if !config.features.enabled(Feature::Plugins) {
            return Ok(empty_response());
        }
        // Every result fits on the first page.
        if cursor.is_some() {
            return Ok(empty_response());
        }

        let normalized_search_term = normalize_search_text(search_term);
        let mut data = Vec::new();
        if scope != Some(PluginSearchScope::Workspace) {
            let roots = cwds.unwrap_or_default();
            let plugins_input = config.plugins_config_input();
            let plugins_manager = self.thread_manager.plugins_manager();
            let outcome = tokio::task::spawn_blocking(move || {
                plugins_manager.list_marketplaces_for_config(
                    &plugins_input,
                    &roots,
                    /*include_openai_curated*/ true,
                )
            })
            .await
            .map_err(|err| internal_error(format!("failed to list marketplace plugins: {err}")))?
            .map_err(|err| Self::marketplace_error(err, "list marketplace plugins"))?;

            for error in outcome.errors {
                warn!(
                    marketplace_path = %error.path.as_path().display(),
                    error = %error.message,
                    "plugin/search skipped a local marketplace that could not be loaded"
                );
            }

            let mut seen_plugin_identities = HashSet::new();
            for marketplace in outcome.marketplaces {
                if !marketplace_matches_search_scope(&marketplace.name, scope) {
                    continue;
                }

                for plugin in marketplace.plugins {
                    let plugin = convert_configured_marketplace_plugin_to_plugin_summary(plugin);
                    if plugin_search_match_rank(&plugin, &normalized_search_term).is_none() {
                        continue;
                    }
                    let identity = if is_openai_curated_marketplace_name(&marketplace.name) {
                        format!("curated:{}", plugin.name)
                    } else {
                        format!("plugin:{}", plugin.id)
                    };
                    if !seen_plugin_identities.insert(identity) {
                        continue;
                    }
                    data.push(plugin_search_result(
                        plugin,
                        marketplace.name.clone(),
                        Some(marketplace.path.clone()),
                    ));
                }
            }
        }

        data.sort_by_key(|result| {
            plugin_search_match_rank(&result.plugin, &normalized_search_term)
        });
        data.truncate(MAX_LOCAL_PLUGIN_SEARCH_RESULTS);

        Ok(PluginSearchResponse {
            data,
            next_cursor: None,
        })
    }
}

/// Plugin discovery does not resolve effective activation, so all results explicitly report
/// `enabled: false`, regardless of their source, installation state, or page.
fn plugin_search_result(
    mut plugin: PluginSummary,
    marketplace_name: String,
    marketplace_path: Option<AbsolutePathBuf>,
) -> PluginSearchResult {
    plugin.enabled = false;

    PluginSearchResult {
        plugin,
        marketplace_name,
        marketplace_path,
    }
}

fn marketplace_matches_search_scope(
    marketplace_name: &str,
    scope: Option<PluginSearchScope>,
) -> bool {
    let is_built_in = is_openai_curated_marketplace_name(marketplace_name)
        || matches!(
            marketplace_name,
            OPENAI_BUNDLED_MARKETPLACE_NAME
                | "openai-bundled-alpha"
                | "codex-official"
                | "openai-primary-runtime"
        );

    match scope {
        None => true,
        Some(PluginSearchScope::Global) => is_built_in,
        Some(PluginSearchScope::Workspace) => false,
        Some(PluginSearchScope::Personal) => !is_built_in,
    }
}

fn normalize_search_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    for character in value.to_lowercase().chars() {
        if character.is_alphanumeric() {
            normalized.push(character);
        } else if !normalized.is_empty() && !normalized.ends_with(' ') {
            normalized.push(' ');
        }
    }
    if normalized.ends_with(' ') {
        normalized.pop();
    }
    normalized
}

fn plugin_search_match_rank(plugin: &PluginSummary, normalized_query: &str) -> Option<usize> {
    if normalized_query.is_empty() {
        return None;
    }

    let visible_name = normalize_search_text(
        plugin
            .interface
            .as_ref()
            .and_then(|interface| interface.display_name.as_deref())
            .unwrap_or_default(),
    );
    let internal_name = normalize_search_text(&plugin.name);
    let names = [&visible_name, &internal_name];
    let keywords = plugin
        .keywords
        .iter()
        .map(|keyword| normalize_search_text(keyword))
        .collect::<Vec<_>>();
    let joined_search_values = normalize_search_text(&format!(
        "{internal_name} {visible_name} {}",
        keywords.join(" ")
    ));

    [
        visible_name == normalized_query,
        internal_name == normalized_query,
        names.iter().any(|name| name.starts_with(normalized_query)),
        names.iter().any(|name| name.contains(normalized_query)),
        keywords.iter().any(|keyword| keyword == normalized_query),
        keywords
            .iter()
            .any(|keyword| keyword.contains(normalized_query))
            || joined_search_values.contains(normalized_query),
    ]
    .iter()
    .position(|matches| *matches)
}
