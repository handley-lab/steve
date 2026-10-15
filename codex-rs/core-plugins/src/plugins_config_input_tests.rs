//! Verify connection reuse without retaining request metadata or crossing config lifetimes.

use super::PluginsConfigInput;
use crate::test_support::test_http_client_factory;
use codex_config::ConfigLayerStack;
use pretty_assertions::assert_eq;
use std::sync::Arc;

#[test]
fn repeated_service_configs_and_early_clones_share_one_lazy_pool() {
    let input = PluginsConfigInput::new(
        ConfigLayerStack::default(),
        Default::default(),
        "openai".to_string(),
        /*plugins_enabled*/ true,
        /*remote_plugin_enabled*/ true,
        "https://chatgpt.com/backend-api".to_string(),
        test_http_client_factory(),
        /*product_sku*/ None,
    );
    let clone = input.clone();
    assert!(input.remote_http_clients.get().is_none());
    let first = input.remote_plugin_service_config();
    let reuse: Vec<_> = (0..220)
        .map(|_| {
            Arc::ptr_eq(
                &first.http_clients,
                &clone.remote_plugin_service_config().http_clients,
            )
        })
        .collect();
    let new_input = PluginsConfigInput::new(
        input.config_layer_stack.clone(),
        input.plugins.clone(),
        input.model_provider_id.clone(),
        input.plugins_enabled,
        input.remote_plugin_enabled,
        input.chatgpt_base_url.clone(),
        input.http_client_factory.clone(),
        input.product_sku,
    );
    assert_eq!(
        (
            reuse,
            Arc::ptr_eq(
                &first.http_clients,
                &new_input.remote_plugin_service_config().http_clients,
            ),
        ),
        (vec![true; 220], false)
    );
}

#[test]
fn reused_pool_uses_current_endpoint_product_and_authentication() {
    let mut input = PluginsConfigInput::new(
        ConfigLayerStack::default(),
        Default::default(),
        "openai".to_string(),
        /*plugins_enabled*/ true,
        /*remote_plugin_enabled*/ true,
        "https://first.example/backend-api".to_string(),
        test_http_client_factory(),
        /*product_sku*/ None,
    );
    let first = input.remote_plugin_service_config();
    input.chatgpt_base_url = "https://second.example/backend-api".to_string();
    input.product_sku = Some("updated".to_string());
    let second = input.remote_plugin_service_config();
    assert!(Arc::ptr_eq(&first.http_clients, &second.http_clients));
}
