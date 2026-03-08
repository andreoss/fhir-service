use serde_json::Value;
use std::path::PathBuf;

fn record(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("doc")
        .join("ops")
        .join("records")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("the recorded move {name} is missing: {error}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("the recorded move {name} is not json: {error}"))
}

fn count(value: &Value, field: &str) -> u64 {
    value
        .get(field)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("the recorded move names no {field}: {value}"))
}

fn empty(value: &Value, field: &str) {
    let held = value
        .get(field)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the recorded move names no {field}: {value}"));
    assert!(held.is_empty(), "{field}: {held:?}");
}

#[test]
fn the_recorded_move_carries_every_version_and_marker_across() {
    let pull = record("move-c400-pull.json");
    let read = count(&pull, "read");
    assert_eq!(count(&pull, "written"), read);
    assert_eq!(count(&pull, "skipped"), 0);
    empty(&pull, "failures");
    assert_eq!(count(&pull, "deleted"), 0);
    let by_type = pull
        .get("byType")
        .and_then(Value::as_object)
        .expect("the recorded move names the types it carried");
    let carried: u64 = by_type.values().filter_map(Value::as_u64).sum();
    assert_eq!(carried, read);
    assert_eq!(by_type.get("Patient").and_then(Value::as_u64), Some(400));
    assert_eq!(
        by_type.get("Observation").and_then(Value::as_u64),
        Some(1200)
    );
    assert_eq!(by_type.get("Procedure").and_then(Value::as_u64), Some(400));
    assert!(count(&pull, "millis") > 0);
}

#[test]
fn the_recorded_move_reconciles_both_ways() {
    let pull = record("move-c400-pull.json");
    let read = count(&pull, "read");
    let reconcile = record("move-c400-reconcile.json");
    assert_eq!(count(&reconcile, "source"), read);
    assert_eq!(count(&reconcile, "target"), read);
    assert_eq!(count(&reconcile, "kept"), read);
    empty(&reconcile, "mismatched");
    empty(&reconcile, "extra");
    assert!(count(&reconcile, "millis") > 0);
}

#[test]
fn the_recorded_move_moves_the_data_back() {
    let pull = record("move-c400-pull.json");
    let read = count(&pull, "read");
    let back = record("move-c400-back.json");
    assert_eq!(count(&back, "read"), read);
    assert_eq!(count(&back, "written"), read);
    assert_eq!(count(&back, "skipped"), 0);
    empty(&back, "failures");
    let reconcile = record("move-c400-back-reconcile.json");
    assert_eq!(count(&reconcile, "source"), read);
    assert_eq!(count(&reconcile, "kept"), read);
    empty(&reconcile, "mismatched");
    empty(&reconcile, "extra");
}

#[test]
fn the_recorded_rollback_leaves_the_source_serving_everything() {
    let pull = record("move-c400-pull.json");
    let read = count(&pull, "read");
    let rollback = record("move-c400-rollback.json");
    assert_eq!(count(&rollback, "sourceTotal"), read);
    assert_eq!(count(&rollback, "copyTotal"), read);
    assert_eq!(
        rollback.get("copyDropped").and_then(Value::as_bool),
        Some(true)
    );
    let after = rollback
        .get("schemasAfter")
        .and_then(Value::as_array)
        .expect("the recorded rollback names the namespaces left");
    let held: Vec<&str> = after.iter().filter_map(Value::as_str).collect();
    assert!(!held.contains(&"move_dst"));
    assert!(held.contains(&"move_src"));
    assert!(count(&rollback, "millis") > 0);
}
