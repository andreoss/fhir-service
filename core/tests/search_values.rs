use fhir_core::search::{lookup, Filter, ValueType};
use fhir_core::{FhirInstant, ResourceId};
use serde_json::{json, Value};

fn filter(type_name: &str, name: &str, raw: &str) -> Filter {
    let resource_type = type_name.parse().unwrap();
    let def = lookup(Some(resource_type), name).expect("parameter is registered");
    Filter::new(
        name,
        def.target,
        raw.split(',')
            .map(|part| def.value(part).expect("value parses"))
            .collect(),
    )
}

fn matches(type_name: &str, name: &str, raw: &str, body: &Value) -> bool {
    let id = ResourceId::parse("r-1").unwrap();
    let updated = FhirInstant::parse("2026-09-06T04:00:00Z").unwrap();
    filter(type_name, name, raw).matches(&id, &updated, body)
}

fn observation() -> Value {
    json!({
        "resourceType": "Observation",
        "id": "ob-1",
        "status": "final",
        "code": {"coding": [{"system": "http://loinc.org", "code": "8867-4"}]},
        "subject": {"reference": "Patient/pt-1"},
        "effectiveDateTime": "2026-09-06T04:00:00Z",
        "valueQuantity": {"value": 72.5, "system": "http://unitsofmeasure.org", "code": "/min"},
        "component": [{
            "code": {"coding": [{"system": "http://loinc.org", "code": "8480-6"}]},
            "valueQuantity": {"value": 120, "system": "http://unitsofmeasure.org", "code": "mm[Hg]"}
        }]
    })
}

fn patient() -> Value {
    json!({
        "resourceType": "Patient",
        "id": "pt-1",
        "name": [{"family": "de la Cruz", "given": ["Ana", "Maria"]}],
        "birthDate": "1980-04-01",
        "managingOrganization": {"reference": "Organization/org-1"}
    })
}

#[test]
fn a_string_value_matches_a_case_insensitive_prefix() {
    assert!(matches("Patient", "family", "de la", &patient()));
    assert!(matches("Patient", "family", "DE LA CRUZ", &patient()));
    assert!(!matches("Patient", "family", "Cruz", &patient()));
    assert!(matches("Patient", "given", "ana", &patient()));
    assert!(matches("Patient", "name", "Maria", &patient()));
}

#[test]
fn a_date_value_matches_a_stored_date() {
    assert!(matches("Patient", "birthdate", "1980-04-01", &patient()));
    assert!(matches("Patient", "birthdate", "lt1990", &patient()));
    assert!(!matches("Patient", "birthdate", "gt1990", &patient()));
    assert!(matches("Observation", "date", "2026-09-06", &observation()));
}

#[test]
fn a_reference_value_matches_by_type_and_id() {
    assert!(matches("Observation", "subject", "Patient/pt-1", &observation()));
    assert!(matches("Observation", "patient", "pt-1", &observation()));
    assert!(!matches("Observation", "subject", "Patient/pt-2", &observation()));
    assert!(matches("Patient", "organization", "Organization/org-1", &patient()));
}

#[test]
fn a_token_value_matches_a_coding_and_a_code() {
    assert!(matches("Observation", "code", "http://loinc.org|8867-4", &observation()));
    assert!(matches("Observation", "status", "final", &observation()));
    assert!(!matches("Observation", "status", "amended", &observation()));
}

#[test]
fn a_quantity_value_matches_value_system_and_unit() {
    assert!(matches("Observation", "value-quantity", "72.5", &observation()));
    assert!(matches("Observation", "value-quantity", "72.5|http://unitsofmeasure.org|/min", &observation()));
    assert!(matches("Observation", "value-quantity", "gt70", &observation()));
    assert!(!matches("Observation", "value-quantity", "lt70", &observation()));
    assert!(!matches("Observation", "value-quantity", "72.5||mg", &observation()));
    assert!(matches("Observation", "component-value-quantity", "120", &observation()));
}

#[test]
fn a_number_value_carries_the_precision_it_was_written_with() {
    let assessment = json!({
        "resourceType": "RiskAssessment",
        "id": "ra-1",
        "prediction": [{"probabilityDecimal": 0.42}]
    });
    assert!(matches("RiskAssessment", "probability", "0.42", &assessment));
    assert!(matches("RiskAssessment", "probability", "0.4", &assessment));
    assert!(!matches("RiskAssessment", "probability", "0.43", &assessment));
    assert!(matches("RiskAssessment", "probability", "lt1", &assessment));
}

#[test]
fn a_composite_value_pairs_two_components_of_one_element() {
    let body = observation();
    assert!(matches("Observation", "code-value-quantity", "http://loinc.org|8867-4$72.5", &body));
    assert!(!matches("Observation", "code-value-quantity", "http://loinc.org|8867-4$99", &body));
    assert!(matches("Observation", "component-code-value-quantity", "8480-6$120", &body));
    assert!(!matches("Observation", "component-code-value-quantity", "8867-4$120", &body));
}

#[test]
fn a_uri_value_is_matched_verbatim() {
    let value = json!({"resourceType": "ValueSet", "id": "vs-1", "url": "http://x/vs"});
    assert!(matches("ValueSet", "url", "http://x/vs", &value));
    assert!(!matches("ValueSet", "url", "http://x/other", &value));
}

#[test]
fn every_value_type_is_reachable_from_the_registry() {
    let observation = Some("Observation".parse().unwrap());
    assert_eq!(lookup(observation, "code").map(|def| def.value_type), Some(ValueType::Token));
    assert_eq!(lookup(observation, "date").map(|def| def.value_type), Some(ValueType::Date));
    assert_eq!(lookup(observation, "subject").map(|def| def.value_type), Some(ValueType::Reference));
    assert_eq!(
        lookup(observation, "value-quantity").map(|def| def.value_type),
        Some(ValueType::Quantity)
    );
    assert_eq!(
        lookup(observation, "value-string").map(|def| def.value_type),
        Some(ValueType::String)
    );
}
