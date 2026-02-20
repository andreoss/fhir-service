use fhir_core::search::index::{IndexKey, KEY_LIMIT};
use fhir_core::search::{SearchValue, ValueType};
use serde_json::json;

fn long(tail: &str) -> String {
    format!("{}{tail}", "u".repeat(KEY_LIMIT * 4))
}

#[test]
fn a_value_shorter_than_the_bound_is_held_whole() {
    let key = IndexKey::of("urn:mrn|12345");
    assert_eq!(key.key(), "urn:mrn|12345");
    assert!(!key.overflows());
    assert_eq!(key.overflow(), None);
    assert!(key.matches("urn:mrn|12345"));
    assert!(!key.matches("urn:mrn|12346"));
    assert!(!key.matches("urn:mrn|1234"));
}

#[test]
fn a_value_at_the_bound_still_needs_no_overflow() {
    let text = "x".repeat(KEY_LIMIT);
    let key = IndexKey::of(&text);
    assert_eq!(key.key().chars().count(), KEY_LIMIT);
    assert!(!key.overflows());
    assert!(key.matches(&text));
    assert!(!key.matches(&format!("{text}x")));
}

#[test]
fn a_value_past_the_bound_keeps_the_remainder_beside_the_key() {
    let text = long("-a");
    let key = IndexKey::of(&text);
    assert_eq!(key.key().chars().count(), KEY_LIMIT);
    assert!(key.overflows());
    assert_eq!(
        format!("{}{}", key.key(), key.overflow().expect("an overflow is kept")),
        text
    );
    assert!(key.matches(&text));
}

#[test]
fn two_values_that_share_the_key_are_still_told_apart() {
    let key = IndexKey::of(&long("-a"));
    assert_eq!(key.key(), IndexKey::of(&long("-b")).key());
    assert!(!key.matches(&long("-b")));
    assert!(!key.matches(&long("")));
    assert!(key.matches(&long("-a")));
}

#[test]
fn a_key_is_cut_on_a_character_and_never_inside_one() {
    let text = "ü".repeat(KEY_LIMIT * 2);
    let key = IndexKey::of(&text);
    assert_eq!(key.key().chars().count(), KEY_LIMIT);
    assert!(std::str::from_utf8(key.key().as_bytes()).is_ok());
    assert!(key.matches(&text));
    assert!(!key.matches(&"ü".repeat(KEY_LIMIT * 2 - 1)));
}

#[test]
fn a_token_longer_than_the_key_is_matched_on_the_whole_of_its_code() {
    let stored = json!({"identifier": [{"system": "urn:mrn", "value": long("-a")}]});
    let element = &stored["identifier"];
    let wanted = SearchValue::parse(ValueType::Token, &format!("urn:mrn|{}", long("-a"))).unwrap();
    assert!(wanted.matches(element));
    let other = SearchValue::parse(ValueType::Token, &format!("urn:mrn|{}", long("-b"))).unwrap();
    assert!(!other.matches(element));
    let truncated =
        SearchValue::parse(ValueType::Token, &format!("urn:mrn|{}", "u".repeat(KEY_LIMIT))).unwrap();
    assert!(!truncated.matches(element));
}

#[test]
fn a_system_longer_than_the_key_is_matched_on_the_whole_of_its_address() {
    let stored = json!({"code": {"coding": [{"system": long("-a"), "code": "x"}]}});
    let element = &stored["code"];
    let wanted = SearchValue::parse(ValueType::Token, &format!("{}|x", long("-a"))).unwrap();
    assert!(wanted.matches(element));
    let other = SearchValue::parse(ValueType::Token, &format!("{}|x", long("-b"))).unwrap();
    assert!(!other.matches(element));
}
