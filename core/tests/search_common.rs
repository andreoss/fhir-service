use fhir_core::search::{common, lookup, Filter, SearchValue, Target, ValueType};
use fhir_core::{FhirInstant, ResourceId};
use serde_json::json;

fn filter(name: &str, raw: &str) -> Filter {
    let def = lookup(None, name).expect("the common parameter is published");
    Filter::new(
        name,
        def.target.clone(),
        raw.split(',')
            .map(|part| SearchValue::parse(def.value_type, part).expect("the value parses"))
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
fn the_id_parameter_selects_by_the_logical_id_and_never_by_the_body() {
    assert!(matches("_id", "pt-1"));
    assert!(!matches("_id", "pt-2"));
    assert!(matches("_id", "pt-2,pt-1"));
    assert!(!matches("_id", "Patient/pt-1"));
    let elsewhere = json!({"resourceType": "Patient", "id": "pt-9"});
    let id = ResourceId::parse("pt-1").unwrap();
    let updated = FhirInstant::parse("2026-09-06T04:00:00Z").unwrap();
    assert!(
        filter("_id", "pt-1").matches(&id, &updated, &elsewhere),
        "the id the store holds decides, not the one the body carries"
    );
}

#[test]
fn the_last_updated_parameter_compares_against_the_instant_the_store_stamped() {
    assert!(matches("_lastUpdated", "2026-09-06"));
    assert!(matches("_lastUpdated", "2026-09-06T04:00:00Z"));
    assert!(matches("_lastUpdated", "ge2026-09-06T04:00:00Z"));
    assert!(matches("_lastUpdated", "le2026-09-06T04:00:00Z"));
    assert!(matches("_lastUpdated", "gt2026-09-05"));
    assert!(!matches("_lastUpdated", "lt2026-09-05"));
    assert!(!matches("_lastUpdated", "gt2026-09-06"));
    assert!(matches("_lastUpdated", "ne2025"));
    assert!(!matches("_lastUpdated", "ne2026-09-06"));
}

#[test]
fn the_profile_parameter_matches_a_canonical_address_verbatim() {
    assert!(matches(
        "_profile",
        "http://example.org/StructureDefinition/vip"
    ));
    assert!(!matches(
        "_profile",
        "http://example.org/StructureDefinition/other"
    ));
    assert!(!matches(
        "_profile",
        "http://example.org/StructureDefinition"
    ));
}

#[test]
fn tag_and_security_match_the_way_the_system_was_supplied() {
    assert!(matches("_tag", "urn:tags|gold"));
    assert!(matches("_tag", "gold"));
    assert!(matches("_tag", "urn:tags|"));
    assert!(!matches("_tag", "|gold"));
    assert!(!matches("_tag", "urn:other|gold"));
    assert!(!matches("_tag", "urn:tags|silver"));
    assert!(matches("_security", "urn:sec|R"));
    assert!(!matches("_security", "urn:sec|N"));
    assert!(!matches("_security", "urn:tags|gold"));
}

#[test]
fn the_common_parameters_are_the_ones_the_specification_gives_every_type() {
    let published: &[(&str, ValueType)] = &[
        ("_id", ValueType::Token),
        ("_lastUpdated", ValueType::Date),
        ("_profile", ValueType::Uri),
        ("_tag", ValueType::Token),
        ("_security", ValueType::Token),
        ("_text", ValueType::String),
    ];
    for (name, value_type) in published {
        let def = lookup(None, name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(def.value_type, *value_type, "{name}");
        assert!(
            lookup(Some("Observation".parse().unwrap()), name).is_some(),
            "{name} applies to every type"
        );
    }
    assert_eq!(common().len(), published.len());
    assert_eq!(
        lookup(None, "_text").map(|def| def.target.clone()),
        Some(Target::Path(vec!["text".to_owned()]))
    );
    let narrative = json!({
        "resourceType": "Patient",
        "id": "pt-n",
        "text": {"status": "generated", "div": "<div><p>Fever and chills</p></div>"}
    });
    let id = ResourceId::parse("pt-n").unwrap();
    let updated = FhirInstant::parse("2026-09-06T04:00:00Z").unwrap();
    assert!(
        filter("_text", "fever AND chills").matches(&id, &updated, &narrative),
        "the narrative pools its words"
    );
    assert!(
        !filter("_text", "rash").matches(&id, &updated, &narrative),
        "only the narrative's words answer"
    );
    assert!(matches!(
        lookup(None, "_id").map(|def| def.target.clone()),
        Some(Target::Id)
    ));
    assert!(matches!(
        lookup(None, "_lastUpdated").map(|def| def.target.clone()),
        Some(Target::LastUpdated)
    ));
    assert_eq!(
        lookup(None, "_profile").map(|def| def.paths()),
        Some(vec!["meta.profile".to_owned()])
    );
    assert!(lookup(None, "_nonesuch").is_none());
    assert!(lookup(None, "name").is_none());
}
