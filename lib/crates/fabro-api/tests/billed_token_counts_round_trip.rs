use std::any::{TypeId, type_name};

use fabro_api::types::BilledTokenCounts as ApiBilledTokenCounts;
use fabro_types::BilledTokenCounts;
use serde_json::json;

#[test]
fn billed_token_counts_reuses_canonical_type() {
    assert_same_type::<ApiBilledTokenCounts, BilledTokenCounts>();
}

#[test]
fn billed_token_counts_json_matches_openapi_shape() {
    let counts = BilledTokenCounts {
        input_tokens: 10,
        output_tokens: 20,
        total_tokens: 35,
        reasoning_tokens: 3,
        cache_read_tokens: 1,
        cache_write_tokens: 1,
        total_usd_micros: Some(42),
    };

    let json = serde_json::to_value(&counts).unwrap();
    assert_eq!(json["input_tokens"], 10);
    assert_eq!(json["output_tokens"], 20);
    assert_eq!(json["total_tokens"], 35);
    assert_eq!(json["reasoning_tokens"], 3);
    assert_eq!(json["cache_read_tokens"], 1);
    assert_eq!(json["cache_write_tokens"], 1);
    assert_eq!(json["total_usd_micros"], 42);

    let round_trip: ApiBilledTokenCounts = serde_json::from_value(json).unwrap();
    assert_eq!(round_trip, counts);
}

#[test]
fn billed_token_counts_keeps_zero_counts_present() {
    let json = serde_json::to_value(BilledTokenCounts::default()).unwrap();
    assert_eq!(json["reasoning_tokens"], 0);
    assert_eq!(json["cache_read_tokens"], 0);
    assert_eq!(json["cache_write_tokens"], 0);
    assert_eq!(json.get("total_usd_micros"), None);

    let round_trip: ApiBilledTokenCounts = serde_json::from_value(json!({
        "input_tokens": 0,
        "output_tokens": 0,
        "total_tokens": 0,
        "reasoning_tokens": 0,
        "cache_read_tokens": 0,
        "cache_write_tokens": 0
    }))
    .unwrap();
    assert_eq!(round_trip, BilledTokenCounts::default());
}

fn assert_same_type<T: 'static, U: 'static>() {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<U>(),
        "{} should be the same type as {}",
        type_name::<T>(),
        type_name::<U>()
    );
}
