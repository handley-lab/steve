//! Verify product attribution remains per thread.

use super::sample_regular_track_event;
use crate::product_attribution::ThreadProducts;
use pretty_assertions::assert_eq;

#[test]
fn product_registry_clears_unset_threads_and_bounds_retention() {
    let mut products = ThreadProducts::default();
    products.register("unset".to_string(), Some("aeon".to_string()));
    products.register("unset".to_string(), /*product*/ None);
    for index in 0..=4096 {
        products.register(format!("thread-{index}"), Some("tpp".to_string()));
    }
    let batches = crate::product_attribution::product_event_batches(
        vec![
            sample_regular_track_event("unset"),
            sample_regular_track_event("thread-0"),
            sample_regular_track_event("thread-4096"),
        ],
        &products,
    );
    assert_eq!(
        batches
            .into_iter()
            .map(|(product, events)| (product, events.len()))
            .collect::<Vec<_>>(),
        vec![(None, 2), (Some("tpp"), 1)]
    );
    for index in 0..17 {
        products.register(format!("thread-{index}"), Some(format!("product-{index}")));
    }
    let events = (0..17)
        .map(|index| sample_regular_track_event(&format!("thread-{index}")))
        .collect::<Vec<_>>();
    let expected = serde_json::json!([[null, &events]]);
    assert_eq!(
        serde_json::to_value(crate::product_attribution::product_event_batches(
            events, &products
        ))
        .unwrap(),
        expected
    );
}
