use fhir_core::search::{lookup, Filter, SearchValue, Target, ValueType};
use fhir_core::{FhirInstant, ResourceId};
use serde_json::json;

fn filter(name: &str, raw: &str) -> Filter {
    let def = lookup(None, name).expect("common parameter is registered");
    Filter::new(
        name,
        def.target.clone(),
        raw.split(',')
            .map(|part| SearchValue::parse(def.value_type, part).expect("value parses"))
            .collect(),
    )
}

fn body() -> serde_json::Value {
    json!({
        "resourceType": "Patient",
        "id": "pt-1",
        "meta": {
            "versionId": "1",
            "lastUpdated": "2026-09-06T04:00:00Z",
            "profile": ["http://example.org/StructureDefinition/vip"],
            "tag": [{"system": "urn:tags", "code": "gold"}],
            "security": [{"system": "urn:sec", "code": "R"}]
        }
    })
}

fn matches(name: &str, raw: &str) -> bool {
    let id = ResourceId::parse("pt-1").unwrap();
    let updated = FhirInstant::parse("2026-09-06T04:00:00Z").unwrap();
    filter(name, raw).matches(&id, &updated, &body())
}

#[test]
fn the_id_parameter_selects_by_resource_id() {
    assert!(matches("_id", "pt-1"));
    assert!(!matches("_id", "pt-2"));
    assert!(matches("_id", "pt-2,pt-1"));
}

#[test]
fn the_last_updated_parameter_compares_chronologically() {
    assert!(matches("_lastUpdated", "2026-09-06"));
    assert!(matches("_lastUpdated", "ge2026-09-06T04:00:00Z"));
    assert!(matches("_lastUpdated", "gt2026-09-05"));
    assert!(!matches("_lastUpdated", "lt2026-09-05"));
    assert!(matches("_lastUpdated", "ne2025"));
    assert!(!matches("_lastUpdated", "ne2026-09-06"));
}

#[test]
fn the_profile_parameter_matches_a_canonical_url() {
    assert!(matches("_profile", "http://example.org/StructureDefinition/vip"));
    assert!(!matches("_profile", "http://example.org/StructureDefinition/other"));
}

#[test]
fn tag_and_security_match_system_and_code() {
    assert!(matches("_tag", "urn:tags|gold"));
    assert!(matches("_tag", "gold"));
    assert!(matches("_tag", "urn:tags|"));
    assert!(!matches("_tag", "|gold"));
    assert!(!matches("_tag", "urn:other|gold"));
    assert!(matches("_security", "urn:sec|R"));
}

#[test]
fn a_parameter_outside_the_registry_is_not_found() {
    assert!(lookup(None, "_nonesuch").is_none());
    assert_eq!(lookup(None, "_id").map(|def| def.value_type), Some(ValueType::Token));
    assert!(matches!(lookup(None, "_lastUpdated").map(|def| def.target.clone()), Some(Target::LastUpdated)));
}
