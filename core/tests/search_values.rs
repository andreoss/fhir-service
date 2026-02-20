use fhir_core::search::{lookup, Filter, ValueType};
use fhir_core::{FhirInstant, ResourceId};
use serde_json::{json, Value};

fn filter(type_name: &str, name: &str, raw: &str) -> Filter {
    let resource_type = type_name.parse().unwrap();
    let def = lookup(Some(resource_type), name).expect("the parameter is published");
    Filter::new(
        name,
        def.target.clone(),
        raw.split(',')
            .map(|part| def.value(part).expect("the value parses"))
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
        "performer": [{"reference": "Practitioner/pr-1"}],
        "effectivePeriod": {"start": "2026-09-06T04:00:00Z", "end": "2026-09-06T05:00:00Z"},
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
fn a_string_matches_the_start_of_a_stored_value_whatever_its_case() {
    assert!(matches("Patient", "family", "de la", &patient()));
    assert!(matches("Patient", "family", "DE LA CRUZ", &patient()));
    assert!(matches("Patient", "family", "de la Cruz", &patient()));
    assert!(!matches("Patient", "family", "Cruz", &patient()));
    assert!(!matches("Patient", "family", "de la Cruz y", &patient()));
    assert!(matches("Patient", "given", "ana", &patient()));
    assert!(matches("Patient", "given", "maria", &patient()));
    assert!(matches("Patient", "name", "Maria", &patient()));
}

#[test]
fn a_date_selects_by_the_span_the_written_precision_denotes() {
    assert!(matches("Patient", "birthdate", "1980-04-01", &patient()));
    assert!(matches("Patient", "birthdate", "1980-04", &patient()));
    assert!(matches("Patient", "birthdate", "1980", &patient()));
    assert!(!matches("Patient", "birthdate", "1980-04-02", &patient()));
    assert!(matches("Patient", "birthdate", "lt1990", &patient()));
    assert!(!matches("Patient", "birthdate", "gt1990", &patient()));
    assert!(matches("Patient", "birthdate", "ge1980-04-01", &patient()));
    assert!(matches("Patient", "birthdate", "le1980-04-01", &patient()));
}

#[test]
fn a_date_compares_against_the_whole_of_a_stored_period() {
    let body = observation();
    assert!(matches("Observation", "date", "2026-09-06", &body));
    assert!(!matches("Observation", "date", "2026-09-06T04:00:00Z", &body));
    assert!(matches("Observation", "date", "ge2026-09-06T04:30:00Z", &body));
    assert!(matches("Observation", "date", "le2026-09-06T04:30:00Z", &body));
    assert!(matches("Observation", "date", "eb2026-09-07", &body));
    assert!(matches("Observation", "date", "sa2026-09-05", &body));
    assert!(!matches("Observation", "date", "sa2026-09-06", &body));
}

#[test]
fn a_reference_matches_by_type_and_id_and_by_the_id_alone() {
    assert!(matches("Observation", "subject", "Patient/pt-1", &observation()));
    assert!(matches("Observation", "patient", "pt-1", &observation()));
    assert!(!matches("Observation", "subject", "Patient/pt-2", &observation()));
    assert!(!matches("Observation", "subject", "Group/pt-1", &observation()));
    assert!(matches("Observation", "performer", "Practitioner/pr-1", &observation()));
    assert!(!matches("Observation", "performer", "Practitioner/pr-2", &observation()));
    assert!(matches("Patient", "organization", "Organization/org-1", &patient()));
}

#[test]
fn a_token_matches_a_coding_a_plain_code_and_the_system_it_was_given() {
    assert!(matches("Observation", "code", "http://loinc.org|8867-4", &observation()));
    assert!(matches("Observation", "code", "8867-4", &observation()));
    assert!(matches("Observation", "code", "http://loinc.org|", &observation()));
    assert!(!matches("Observation", "code", "|8867-4", &observation()));
    assert!(!matches("Observation", "code", "http://snomed.info/sct|8867-4", &observation()));
    assert!(matches("Observation", "status", "final", &observation()));
    assert!(!matches("Observation", "status", "amended", &observation()));
    assert!(matches("Observation", "status", "amended,final", &observation()));
}

#[test]
fn a_quantity_needs_its_value_its_system_and_its_unit_to_hold_together() {
    assert!(matches("Observation", "value-quantity", "72.5", &observation()));
    assert!(matches(
        "Observation",
        "value-quantity",
        "72.5|http://unitsofmeasure.org|/min",
        &observation()
    ));
    assert!(!matches(
        "Observation",
        "value-quantity",
        "72.5|http://other.org|/min",
        &observation()
    ));
    assert!(!matches("Observation", "value-quantity", "72.5||mg", &observation()));
    assert!(matches("Observation", "value-quantity", "gt70", &observation()));
    assert!(!matches("Observation", "value-quantity", "lt70", &observation()));
    assert!(
        !matches("Observation", "value-quantity", "120", &observation()),
        "the parameter reads the value of the observation, not that of a component"
    );
    assert!(matches("Observation", "component-value-quantity", "120", &observation()));
}

#[test]
fn a_number_selects_the_range_the_precision_it_was_written_with_denotes() {
    let assessment = |value: Value| {
        json!({
            "resourceType": "RiskAssessment",
            "id": "ra-1",
            "prediction": [{"probabilityDecimal": value}]
        })
    };
    let held = assessment(json!(0.42));
    assert!(matches("RiskAssessment", "probability", "0.42", &held));
    assert!(matches("RiskAssessment", "probability", "0.4", &held));
    assert!(!matches("RiskAssessment", "probability", "0.43", &held));
    assert!(matches("RiskAssessment", "probability", "lt1", &held));
    for boundary in [0.35, 0.45] {
        assert!(
            matches("RiskAssessment", "probability", "0.4", &assessment(json!(boundary))),
            "the bound its precision denotes is inside the range: {boundary}"
        );
    }
    assert!(!matches("RiskAssessment", "probability", "0.4", &assessment(json!(0.34))));
    assert!(!matches("RiskAssessment", "probability", "0.4", &assessment(json!(0.46))));
    assert!(matches("RiskAssessment", "probability", "0.5", &assessment(json!(0.45))));
}

#[test]
fn a_composite_needs_both_halves_to_hold_of_one_element() {
    let body = observation();
    assert!(matches(
        "Observation",
        "code-value-quantity",
        "http://loinc.org|8867-4$72.5",
        &body
    ));
    assert!(!matches(
        "Observation",
        "code-value-quantity",
        "http://loinc.org|8867-4$99",
        &body
    ));
    assert!(!matches(
        "Observation",
        "code-value-quantity",
        "http://loinc.org|8480-6$72.5",
        &body
    ));
    assert!(matches("Observation", "component-code-value-quantity", "8480-6$120", &body));
    assert!(!matches(
        "Observation",
        "component-code-value-quantity",
        "8867-4$120",
        &body
    ));
    assert!(
        !matches(
            "Observation",
            "component-code-value-quantity",
            "8867-4$72.5",
            &body
        ),
        "the halves must hold of one component, not of the observation and a component"
    );
}

#[test]
fn a_uri_is_matched_verbatim_and_never_as_a_prefix() {
    let value = json!({"resourceType": "ValueSet", "id": "vs-1", "url": "http://x/vs"});
    assert!(matches("ValueSet", "url", "http://x/vs", &value));
    assert!(!matches("ValueSet", "url", "http://x/other", &value));
    assert!(!matches("ValueSet", "url", "http://x", &value));
    assert!(!matches("ValueSet", "url", "http://x/vs/more", &value));
    assert!(!matches("ValueSet", "url", "HTTP://X/VS", &value));
}

#[test]
fn every_kind_of_value_the_specification_publishes_is_carried_by_a_parameter() {
    let published: &[(&str, &str, ValueType)] = &[
        ("RiskAssessment", "probability", ValueType::Number),
        ("Observation", "date", ValueType::Date),
        ("Observation", "value-string", ValueType::String),
        ("Observation", "code", ValueType::Token),
        ("Observation", "value-quantity", ValueType::Quantity),
        ("Observation", "subject", ValueType::Reference),
        ("Observation", "code-value-quantity", ValueType::Composite),
        ("ValueSet", "url", ValueType::Uri),
    ];
    for (type_name, name, value_type) in published {
        let def = lookup(Some(type_name.parse().unwrap()), name)
            .unwrap_or_else(|| panic!("{type_name}.{name}"));
        assert_eq!(def.value_type, *value_type, "{type_name}.{name}");
    }
    assert_eq!(published.len(), 8);
}
