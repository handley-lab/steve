//! Exact decimal preservation and sorting for consumer task credits.

use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn credits_preserve_precision_and_order_signed_decimal_strings() {
    let mut amounts: Vec<TaskCredits> = serde_json::from_value(json!([
        "1.000000000000000002",
        "-0.00025",
        "0",
        "1.000000000000000001",
        "1e-20",
        "-1E+3"
    ]))
    .unwrap();
    amounts.sort();
    assert_eq!(
        amounts.iter().map(TaskCredits::as_str).collect::<Vec<_>>(),
        vec![
            "-1E+3",
            "-0.00025",
            "0",
            "1e-20",
            "1.000000000000000001",
            "1.000000000000000002"
        ]
    );
    for invalid in ["NaN", "inf", "1..0", "", "1e999999999999"] {
        assert!(serde_json::from_value::<TaskCredits>(json!(invalid)).is_err());
    }
}
