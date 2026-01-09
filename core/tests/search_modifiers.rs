use fhir_core::search::{lookup, Filter, Modifier, SearchValue};
use fhir_core::{FhirInstant, ResourceId};
use serde_json::{json, Value};

fn id() -> ResourceId {
    ResourceId::parse("p-1").unwrap()
}

fn updated() -> FhirInstant {
    FhirInstant::parse("2026-09-06T04:00:00Z").unwrap()
}

fn filter(type_name: &str, name: &str, modifier: Modifier, raw: &str) -> Filter {
    let resource_type = type_name.parse().unwrap();
    let def = lookup(Some(resource_type), name).unwrap();
    let values: Vec<SearchValue> = raw
        .split(',')
        .map(|part| def.value_with(&modifier, part).unwrap())
        .collect();
    Filter {
        name: name.to_owned(),
        target: def.target.clone(),
        modifier,
        index: None,
        values,
    }
}

fn holds(type_name: &str, name: &str, modifier: Modifier, raw: &str, body: &Value) -> bool {
    filter(type_name, name, modifier, raw).matches(&id(), &updated(), body)
}

#[test]
fn missing_selects_resources_carrying_no_value() {
    let with = json!({"gender": "female"});
    let without = json!({"active": true});
    assert!(holds("Patient", "gender", Modifier::Missing, "true", &without));
    assert!(!holds("Patient", "gender", Modifier::Missing, "true", &with));
    assert!(holds("Patient", "gender", Modifier::Missing, "false", &with));
    assert!(!holds("Patient", "gender", Modifier::Missing, "false", &without));
}

#[test]
fn exact_and_contains_narrow_a_string() {
    let body = json!({"name": [{"family": "Sørensen"}]});
    assert!(holds("Patient", "family", Modifier::Exact, "Sørensen", &body));
    assert!(!holds("Patient", "family", Modifier::Exact, "sørensen", &body));
    assert!(!holds("Patient", "family", Modifier::Exact, "Sør", &body));
    assert!(holds("Patient", "family", Modifier::Contains, "rens", &body));
    assert!(holds("Patient", "family", Modifier::Contains, "RENS", &body));
    assert!(!holds("Patient", "family", Modifier::Contains, "zzz", &body));
}

#[test]
fn not_needs_every_value_of_the_element_to_differ() {
    let body = json!({"gender": "male"});
    assert!(holds("Patient", "gender", Modifier::Not, "female", &body));
    assert!(!holds("Patient", "gender", Modifier::Not, "male", &body));
    let repeated = json!({"category": [{"coding": [{"code": "vital-signs"}, {"code": "exam"}]}]});
    assert!(!holds("Observation", "category", Modifier::Not, "exam", &repeated));
    assert!(holds("Observation", "category", Modifier::Not, "survey", &repeated));
}

#[test]
fn text_matches_the_narrative_of_a_coded_element() {
    let body = json!({"code": {"text": "Body Temperature", "coding": [{"code": "8310-5"}]}});
    assert!(holds("Observation", "code", Modifier::Text, "temperature", &body));
    assert!(!holds("Observation", "code", Modifier::Text, "8310-5", &body));
    let display = json!({"code": {"coding": [{"code": "x", "display": "Heart rate"}]}});
    assert!(holds("Observation", "code", Modifier::Text, "heart", &display));
}

#[test]
fn membership_of_a_code_set_is_tested_in_both_directions() {
    let body = json!({"code": {"coding": [{"system": "urn:s", "code": "a"}]}});
    let inside = filter("Observation", "code", Modifier::In, "http://x/vs").resolved(&[
        SearchValue::parse(fhir_core::search::ValueType::Token, "urn:s|a").unwrap(),
    ]);
    assert!(inside.matches(&id(), &updated(), &body));
    let outside = filter("Observation", "code", Modifier::NotIn, "http://x/vs").resolved(&[
        SearchValue::parse(fhir_core::search::ValueType::Token, "urn:s|a").unwrap(),
    ]);
    assert!(!outside.matches(&id(), &updated(), &body));
}

#[test]
fn below_and_above_walk_a_hierarchy() {
    let code = json!({"code": {"coding": [{"code": "a.b.c"}]}});
    assert!(holds("Observation", "code", Modifier::Below, "a.b", &code));
    assert!(holds("Observation", "code", Modifier::Below, "a.b.c", &code));
    assert!(!holds("Observation", "code", Modifier::Below, "a.d", &code));
    assert!(holds("Observation", "code", Modifier::Above, "a.b.c.d", &code));
    assert!(!holds("Observation", "code", Modifier::Above, "a", &code));
    let uri = json!({"url": "http://x/base/part"});
    assert!(holds("ValueSet", "url", Modifier::Below, "http://x/base", &uri));
    assert!(!holds("ValueSet", "url", Modifier::Below, "http://x/other", &uri));
    assert!(holds("ValueSet", "url", Modifier::Above, "http://x/base/part/deep", &uri));
}

#[test]
fn a_type_modifier_restricts_a_reference() {
    let body = json!({"subject": {"reference": "Patient/p-9"}});
    let patient = Modifier::Type("Patient".parse().unwrap());
    let group = Modifier::Type("Group".parse().unwrap());
    assert!(holds("Observation", "subject", patient, "p-9", &body));
    assert!(!holds("Observation", "subject", group, "p-9", &body));
}

#[test]
fn an_identifier_modifier_reads_the_identifier_of_a_reference() {
    let body = json!({"subject": {"identifier": {"system": "urn:mrn", "value": "42"}}});
    assert!(holds("Observation", "subject", Modifier::Identifier, "urn:mrn|42", &body));
    assert!(!holds("Observation", "subject", Modifier::Identifier, "urn:mrn|43", &body));
}

#[test]
fn of_type_matches_an_identifier_by_its_type() {
    let body = json!({
        "identifier": [{
            "type": {"coding": [{"system": "urn:t", "code": "MR"}]},
            "value": "12345"
        }]
    });
    assert!(holds("Patient", "identifier", Modifier::OfType, "urn:t|MR|12345", &body));
    assert!(!holds("Patient", "identifier", Modifier::OfType, "urn:t|MR|99", &body));
    assert!(!holds("Patient", "identifier", Modifier::OfType, "urn:t|SB|12345", &body));
}

#[test]
fn a_modifier_the_value_type_forbids_is_rejected() {
    let def = lookup(Some("Patient".parse().unwrap()), "gender").unwrap();
    assert!(def.value_with(&Modifier::Exact, "male").is_err());
    assert!(def.value_with(&Modifier::Missing, "perhaps").is_err());
    let family = lookup(Some("Patient".parse().unwrap()), "family").unwrap();
    assert!(family.value_with(&Modifier::OfType, "a|b|c").is_err());
    assert!(family.value_with(&Modifier::Exact, "Ann").is_ok());
}
