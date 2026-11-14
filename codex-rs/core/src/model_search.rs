//! Resolve one search provider for the sampling step, preserving the host's search policy.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

use codex_extension_api::ExtensionData;
use codex_history::InitialHistory;
use codex_history::ResponseItemEnvelope;
use codex_history::RolloutItem;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::openai_models::WebSearchProvider;
use codex_tools::ToolSpec;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::session::step_settings::ResolvedStepSettings;
use crate::session::turn_context::TurnContext;
use crate::turn_metadata::ExecutionMetadata;

#[derive(Clone)]
pub(crate) enum ModelSearch {
    ClientMcp,
    Native { mode: WebSearchMode, fallback: bool },
    Disabled,
}

#[derive(Default)]
pub(crate) struct SearchTurnUsage(AtomicU8);

impl SearchTurnUsage {
    const MCP: u8 = 1;
    const NATIVE: u8 = 2;

    pub(crate) fn record(&self, turn: &TurnContext, provider: WebSearchProvider) {
        let (flag, label) = match provider {
            WebSearchProvider::SearchMcp => (Self::MCP, "search_mcp"),
            WebSearchProvider::Standalone => (Self::NATIVE, "native"),
        };
        let previous = self.0.fetch_or(flag, Ordering::Relaxed);
        if previous & flag == 0 {
            turn.session_telemetry.counter(
                "codex.model_search.turn",
                /*inc*/ 1,
                &[("provider", label)],
            );
            if previous != 0 {
                turn.session_telemetry.counter(
                    "codex.model_search.turn",
                    /*inc*/ 1,
                    &[("provider", "mixed")],
                );
            }
        }
    }
}

/// Concurrent calls on the same provider share a reservation. An actual search
/// keeps it for the thread. An unused offer releases it when sampling finishes.
pub(crate) struct SearchOwner(WebSearchProvider);

impl SearchOwner {
    fn reserve(store: &ExtensionData, provider: WebSearchProvider) -> Option<Arc<Self>> {
        let slot = store.get_or_init(Mutex::<Weak<Self>>::default);
        let mut slot = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(owner) = slot.upgrade() {
            return (owner.0 == provider).then_some(owner);
        }
        let owner = Arc::new(Self(provider));
        *slot = Arc::downgrade(&owner);
        Some(owner)
    }

    pub(crate) fn admit(store: &ExtensionData, provider: WebSearchProvider) -> bool {
        let Some(owner) = Self::reserve(store, provider) else {
            return false;
        };
        store.insert_arc(owner);
        true
    }

    pub(crate) fn reserve_native_fallback(
        step: &ExtensionData,
        thread: &ExtensionData,
    ) -> Result<Option<Arc<Self>>, &'static str> {
        if matches!(
            step.get::<ModelSearch>().as_deref(),
            Some(ModelSearch::Native { fallback: true, .. })
        ) {
            return Self::reserve(thread, WebSearchProvider::Standalone)
                .map(Some)
                .ok_or("Search MCP is already in use in this thread");
        }
        Ok(None)
    }

    // Responses can execute its built-in search before the harness receives the
    // first search event. Reserve it while that request can still issue a call.
    pub(crate) fn reserve_server_fallback(
        step: &ExtensionData,
        thread: &ExtensionData,
        tools: &mut Arc<[ToolSpec]>,
    ) -> Option<Arc<Self>> {
        let is_search = |tool: &ToolSpec| matches!(tool, ToolSpec::WebSearch { .. });
        if !tools.iter().any(is_search) {
            return None;
        }
        match Self::reserve_native_fallback(step, thread) {
            Ok(owner) => owner,
            Err(_) => {
                *tools = tools
                    .iter()
                    .filter(|tool| !is_search(tool))
                    .cloned()
                    .collect();
                None
            }
        }
    }

    fn current(store: &ExtensionData) -> Option<WebSearchProvider> {
        store.get::<Self>().map(|owner| owner.0)
    }

    pub(crate) fn restore(store: &ExtensionData, history: &InitialHistory) {
        let used = |item: &ResponseItemEnvelope| {
            item.metadata
                .as_ref()
                .is_some_and(|meta| meta.native_search_fallback_used)
        };
        if history.scan_rollout_items(|item| match item {
            RolloutItem::ResponseItem(item) => used(item),
            RolloutItem::Compacted(item) => item.replacement_history.iter().flatten().any(used),
            _ => false,
        }) {
            Self::admit(store, WebSearchProvider::Standalone);
        }
    }

    pub(crate) fn checkpoint(store: &ExtensionData, items: &mut [ResponseItemEnvelope]) {
        if Self::current(store) == Some(WebSearchProvider::Standalone) {
            for item in items {
                item.metadata
                    .get_or_insert_default()
                    .native_search_fallback_used = true;
            }
        }
    }
}

fn object(value: Option<&Value>) -> Option<Map<String, Value>> {
    match value {
        None => Some(Map::new()),
        Some(Value::Object(map)) => Some(map.clone()),
        Some(Value::String(encoded)) => serde_json::from_str(encoded).ok(),
        _ => None,
    }
}

impl crate::config::Config {
    // Read the scalar override from the highest enabled configuration layer. Hosts can
    // supply it through normal session flags without extending the shared Config struct.
    pub(crate) fn client_search_provider(&self) -> Option<WebSearchProvider> {
        self.config_layer_stack
            .layers_high_to_low()
            .find_map(|layer| layer.config.get("tools")?.get("client_search_provider"))?
            .clone()
            .try_into()
            .ok()
    }

    pub(crate) fn requires_native_search(&self) -> bool {
        let standalone = codex_features::Feature::StandaloneWebSearch;
        if !self.features.enabled(standalone) {
            return false;
        }
        let mut features = self.features.clone();
        features.disable(standalone).is_err() || features.enabled(standalone)
    }
}

fn requested_mode(context: &Map<String, Value>, default: WebSearchMode) -> Option<WebSearchMode> {
    context.get("mode").map_or(Some(default), |value| {
        serde_json::from_value(value.clone()).ok()
    })
}

// These host metadata fields do not select connectors. Preserve the installation ID
// when narrowing discovery, and validate the search context separately for native.
fn unrestricted_connectors(request: &Map<String, Value>) -> bool {
    request.keys().all(|key| {
        matches!(
            key.as_str(),
            "openai/search_context" | "openai/codex_installation_id"
        )
    })
}

fn native_search_mode(turn: &TurnContext, request: &Map<String, Value>) -> Option<WebSearchMode> {
    if !unrestricted_connectors(request) {
        return None;
    }
    let context = object(request.get("openai/search_context"))?;
    let configured = turn.config.web_search_mode.value();
    let mut mode = configured.restrict_to(requested_mode(&context, configured)?);
    // A host domain allowlist covers opens and clicks. Native filters only the query.
    for (key, value) in &context {
        match key.as_str() {
            "mode" | "telemetry_attributes" | "model_supported_tools" => {}
            "is_web_access_restricted" => {
                if value.as_bool()? {
                    mode = mode.restrict_to(WebSearchMode::Cached);
                }
            }
            "session_persistent" if value.as_bool()? || turn.config.ephemeral => {}
            _ => return None,
        }
    }
    turn.config.web_search_mode.can_set(&mode).ok()?;
    (mode != WebSearchMode::Disabled).then_some(mode)
}

pub(crate) async fn resolve(
    turn: &TurnContext,
    settings: &ResolvedStepSettings,
) -> Option<ModelSearch> {
    let native_required = turn.config.requires_native_search();
    let provider = if native_required {
        WebSearchProvider::Standalone
    } else if let Some(provider) = settings.model_info.web_search_provider {
        provider
    } else {
        match turn.config.client_search_provider()? {
            WebSearchProvider::SearchMcp => return Some(ModelSearch::ClientMcp),
            WebSearchProvider::Standalone => WebSearchProvider::Standalone,
        }
    };
    let observe = |decision, reason| {
        turn.session_telemetry.counter(
            "codex.model_search.discovery",
            /*inc*/ 1,
            &[("reason", reason)],
        );
        Some(decision)
    };
    let turn_meta = turn
        .turn_metadata_state
        .current_meta_value_for_mcp_request(ExecutionMetadata::from_settings(settings))
        .unwrap_or_else(|| json!({}));
    let Some(request) = object(turn_meta.get("mcp_request_meta")) else {
        return observe(ModelSearch::Disabled, "invalid_request_metadata");
    };
    let native_mode = native_search_mode(turn, &request);
    if native_required {
        return match native_mode {
            Some(mode) => observe(
                ModelSearch::Native {
                    mode,
                    fallback: false,
                },
                "native_required",
            ),
            None => observe(ModelSearch::Disabled, "fallback_restricted"),
        };
    }
    if provider == WebSearchProvider::Standalone {
        return Some(
            native_mode.map_or(ModelSearch::Disabled, |mode| ModelSearch::Native {
                mode,
                fallback: false,
            }),
        );
    }
    // The search connector is served by the hosted Apps server, which needs an account.
    observe(ModelSearch::Disabled, "connector_unavailable")
}
