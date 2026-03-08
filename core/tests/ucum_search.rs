use fhir_core::search::{SearchValue, ValueType};
use serde_json::json;

fn matches(search: &str, stored: serde_json::Value) -> bool {
    SearchValue::parse(ValueType::Quantity, search)
        .expect("a quantity search value")
        .matches(&stored)
}

fn quantity(value: f64, code: &str) -> serde_json::Value {
    json!({"value": value, "system": "http://unitsofmeasure.org", "code": code})
}

#[test]
fn a_search_in_one_unit_finds_a_value_in_another_of_the_same_dimension() {
    assert!(matches(
        "2|http://unitsofmeasure.org|kg",
        quantity(2000.0, "g")
    ));
    assert!(matches(
        "2000|http://unitsofmeasure.org|g",
        quantity(2.0, "kg")
    ));
}

#[test]
fn a_unit_of_another_dimension_is_not_found() {
    assert!(!matches(
        "2|http://unitsofmeasure.org|kg",
        quantity(2000.0, "m")
    ));
}

#[test]
fn a_comparator_holds_across_the_conversion() {
    assert!(matches(
        "gt1|http://unitsofmeasure.org|kg",
        quantity(1500.0, "g")
    ));
    assert!(!matches(
        "gt2|http://unitsofmeasure.org|kg",
        quantity(1500.0, "g")
    ));
    assert!(matches(
        "lt2|http://unitsofmeasure.org|kg",
        quantity(1500.0, "g")
    ));
}

#[test]
fn the_same_unit_still_matches_itself() {
    assert!(matches(
        "2|http://unitsofmeasure.org|kg",
        quantity(2.0, "kg")
    ));
}

#[test]
fn a_unit_outside_the_table_is_compared_as_written() {
    let held = json!({"value": 2.0, "system": "urn:local", "code": "widgets"});
    assert!(matches("2|urn:local|widgets", held.clone()));
    assert!(!matches("2|urn:local|gadgets", held));
}

#[test]
fn a_search_naming_no_unit_still_matches_on_the_number() {
    assert!(matches("2", quantity(2.0, "kg")));
}
