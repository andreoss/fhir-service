use fhir_core::search::index::{IndexKey, KEY_LIMIT};
use fhir_core::search::{SearchValue, ValueType};
use serde_json::json;

fn long(tail: &str) -> String {
    format!("{}{tail}", "u".repeat(KEY_LIMIT * 4))
}

#[test]
fn a_short_value_indexes_without_overflow() {
    let key = IndexKey::of("urn:mrn|12345");
    assert_eq!(key.key(), "urn:mrn|12345");
    assert!(!key.overflows());
    assert!(key.matches("urn:mrn|12345"));
    assert!(!key.matches("urn:mrn|12346"));
}

#[test]
fn a_long_value_keeps_a_bounded_key_and_its_overflow() {
    let text = long("-a");
    let key = IndexKey::of(&text);
    assert_eq!(key.key().chars().count(), KEY_LIMIT);
    assert!(key.overflows());
    assert!(key.matches(&text));
}

#[test]
fn two_long_values_differing_past_the_key_still_differ() {
    let key = IndexKey::of(&long("-a"));
    assert_eq!(key.key(), IndexKey::of(&long("-b")).key());
    assert!(!key.matches(&long("-b")));
    assert!(!key.matches(&long("")));
}

#[test]
fn a_key_never_splits_a_character() {
    let text = "ü".repeat(KEY_LIMIT * 2);
    let key = IndexKey::of(&text);
    assert_eq!(key.key().chars().count(), KEY_LIMIT);
    assert!(key.matches(&text));
    assert!(!key.matches(&"ü".repeat(KEY_LIMIT * 2 - 1)));
}

#[test]
fn a_token_longer_than_the_key_matches_exactly() {
    let stored = json!({"identifier": [{"system": "urn:mrn", "value": long("-a")}]});
    let element = &stored["identifier"];
    let wanted = SearchValue::parse(ValueType::Token, &format!("urn:mrn|{}", long("-a"))).unwrap();
    assert!(wanted.matches(element));
    let other = SearchValue::parse(ValueType::Token, &format!("urn:mrn|{}", long("-b"))).unwrap();
    assert!(!other.matches(element));
}

#[test]
fn a_system_longer_than_the_key_matches_exactly() {
    let stored = json!({"code": {"coding": [{"system": long("-a"), "code": "x"}]}});
    let element = &stored["code"];
    let wanted = SearchValue::parse(ValueType::Token, &format!("{}|x", long("-a"))).unwrap();
    assert!(wanted.matches(element));
    let other = SearchValue::parse(ValueType::Token, &format!("{}|x", long("-b"))).unwrap();
    assert!(!other.matches(element));
}
