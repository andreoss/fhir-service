use fhir_core::validate::{validate, Mode, Request};
use serde_json::json;

fn codes(report: &fhir_core::validate::Report) -> Vec<String> {
    report
        .issues()
        .iter()
        .map(|issue| format!("{}:{}", issue.severity, issue.code))
        .collect()
}

fn asked<'a>(body: &'a serde_json::Value) -> Request<'a> {
    Request {
        version: fhir_core::FhirVersion::R4,
        resource_type: None,
        id: None,
        profile: None,
        mode: Mode::Update,
        body,
    }
}

#[test]
fn a_well_formed_resource_reports_no_error() {
    let body = json!({
        "resourceType": "Patient",
        "id": "pt-1",
        "text": {"status": "generated", "div": "<div xmlns=\"http://www.w3.org/1999/xhtml\">Ann</div>"},
        "active": true
    });
    let report = validate(&asked(&body));
    assert!(!report.has_errors());
    assert_eq!(codes(&report), vec!["information:informational".to_owned()]);
}

#[test]
fn a_body_without_a_type_is_an_error() {
    let body = json!({"id": "pt-1"});
    let report = validate(&asked(&body));
    assert!(report.has_errors());
    assert!(report.to_fhir_json_text().contains("resourceType"));
}

#[test]
fn a_type_or_id_that_contradicts_the_request_is_an_error() {
    let body = json!({"resourceType": "Patient", "id": "pt-1"});
    let mut request = asked(&body);
    request.resource_type = Some("Observation".parse().unwrap());
    assert!(validate(&request).has_errors());
    let mut other = asked(&body);
    other.resource_type = Some("Patient".parse().unwrap());
    other.id = Some("pt-2".parse().unwrap());
    assert!(validate(&other).has_errors());
    other.id = Some("pt-1".parse().unwrap());
    assert!(!validate(&other).has_errors());
}

#[test]
fn a_profile_the_resource_does_not_claim_is_an_error() {
    let body = json!({"resourceType": "Patient", "id": "pt-1"});
    let mut request = asked(&body);
    request.profile = Some("http://x/StructureDefinition/one");
    assert!(validate(&request).has_errors());
    let claimed = json!({
        "resourceType": "Patient",
        "id": "pt-1",
        "meta": {"profile": ["http://x/StructureDefinition/one"]}
    });
    let mut held = asked(&claimed);
    held.profile = Some("http://x/StructureDefinition/one");
    assert!(!validate(&held).has_errors());
}

#[test]
fn a_narrative_is_checked_when_the_resource_carries_one() {
    let bad_status = json!({
        "resourceType": "Patient",
        "text": {"status": "invented", "div": "<div>a</div>"}
    });
    assert!(validate(&asked(&bad_status)).has_errors());
    let bad_div = json!({"resourceType": "Patient", "text": {"status": "generated", "div": "plain"}});
    assert!(validate(&asked(&bad_div)).has_errors());
    let missing = json!({"resourceType": "Patient", "text": {"status": "generated"}});
    assert!(validate(&asked(&missing)).has_errors());
}

#[test]
fn an_element_carrying_no_value_is_an_error() {
    let body = json!({"resourceType": "Patient", "name": [], "active": null});
    let report = validate(&asked(&body));
    assert!(report.has_errors());
    assert!(report.issues().len() >= 2);
}

#[test]
fn a_create_carrying_an_id_is_reported_against_the_element_that_carries_it() {
    let body = json!({"resourceType": "Patient", "id": "pt-1"});
    let mut request = asked(&body);
    request.mode = Mode::Create;
    let report = validate(&request);
    let held = report
        .issues()
        .iter()
        .find(|issue| issue.expression.as_deref() == Some("id"))
        .expect("the id the body carries is named");
    assert_eq!(held.code, fhir_core::IssueCode::Invalid);
    assert!(held.diagnostics.contains("pt-1"), "{}", held.diagnostics);

    let mut update = asked(&body);
    update.mode = Mode::Update;
    assert!(validate(&update)
        .issues()
        .iter()
        .all(|issue| issue.expression.as_deref() != Some("id")));
}

#[test]
fn a_report_is_an_operation_outcome_the_definitions_accept() {
    let bodies = [
        json!({"resourceType": "Patient", "id": "pt-1", "active": true}),
        json!({"resourceType": "Patient", "favourite": "tea"}),
        json!({"resourceType": "Patient", "gender": "lady"}),
        json!({"resourceType": "Observation", "status": "final"}),
        json!({"resourceType": "Patient", "text": {"status": "invented", "div": "x"}}),
        json!({"resourceType": "Patient", "name": [], "active": null}),
        json!("text"),
    ];
    for version in [
        fhir_core::FhirVersion::R4,
        fhir_core::FhirVersion::R4b,
        fhir_core::FhirVersion::R5,
    ] {
        for body in &bodies {
            let mut request = asked(body);
            request.version = version;
            let rendered: serde_json::Value =
                serde_json::from_slice(&validate(&request).to_fhir_json()).unwrap();
            assert_eq!(
                fhir_core::Model::of(version).check(&rendered),
                Vec::new(),
                "{version} {body}"
            );
        }
    }
}

#[test]
fn a_delete_needs_no_body() {
    let body = json!(null);
    let mut request = asked(&body);
    request.mode = Mode::Delete;
    assert!(!validate(&request).has_errors());
}

#[test]
fn a_mode_is_read_from_its_spelling() {
    assert_eq!("create".parse::<Mode>().unwrap(), Mode::Create);
    assert_eq!("update".parse::<Mode>().unwrap(), Mode::Update);
    assert_eq!("delete".parse::<Mode>().unwrap(), Mode::Delete);
    assert!("nonesuch".parse::<Mode>().is_err());
}

#[test]
fn a_missing_required_element_names_the_cardinality_rule() {
    let body = json!({"resourceType": "Observation", "status": "final"});
    let report = validate(&asked(&body));
    assert!(report.has_errors());
    let text = report.to_fhir_json_text();
    assert!(text.contains("cardinality"), "{text}");
    assert!(text.contains("Observation.code"), "{text}");
}

#[test]
fn a_code_outside_a_bound_value_set_names_the_binding_rule() {
    let body = json!({"resourceType": "Patient", "gender": "lady"});
    let report = validate(&asked(&body));
    assert!(report.has_errors());
    let text = report.to_fhir_json_text();
    assert!(text.contains("binding"), "{text}");
}

#[test]
fn an_element_outside_the_definitions_names_the_structure_rule() {
    let body = json!({"resourceType": "Patient", "favourite": "tea"});
    let report = validate(&asked(&body));
    assert!(report.has_errors());
    assert!(report.to_fhir_json_text().contains("structure"), "not named");
}

#[test]
fn a_malformed_narrative_names_the_narrative_rule() {
    let body = json!({"resourceType": "Patient", "text": {"status": "invented", "div": "x"}});
    let report = validate(&asked(&body));
    assert!(report.has_errors());
    assert!(report.to_fhir_json_text().contains("narrative"), "not named");
}

#[test]
fn a_profile_of_another_type_names_the_profile_rule() {
    let body = json!({
        "resourceType": "Patient",
        "meta": {"profile": ["http://hl7.org/fhir/StructureDefinition/Observation"]}
    });
    let mut request = asked(&body);
    request.profile = Some("http://hl7.org/fhir/StructureDefinition/Observation");
    let report = validate(&request);
    assert!(report.has_errors());
    assert!(report.to_fhir_json_text().contains("profile"), "not named");
}

#[test]
fn a_profile_the_definitions_do_not_carry_is_reported_as_unchecked() {
    let body = json!({
        "resourceType": "Patient",
        "meta": {"profile": ["http://example.test/StructureDefinition/local"]}
    });
    let mut request = asked(&body);
    request.profile = Some("http://example.test/StructureDefinition/local");
    let report = validate(&request);
    let held = report
        .issues()
        .iter()
        .find(|issue| issue.expression.as_deref() == Some("meta.profile"))
        .expect("the profile is named");
    assert_eq!(held.severity, fhir_core::IssueSeverity::Information);
    assert_eq!(held.code, fhir_core::IssueCode::Informational);
    assert!(!report.has_errors(), "{}", report.to_fhir_json_text());
}

#[test]
fn a_type_a_version_does_not_publish_is_refused_by_that_version() {
    let body = json!({"resourceType": "EvidenceVariable", "status": "active"});
    let names_the_type = |version| {
        let mut request = asked(&body);
        request.version = version;
        validate(&request).to_fhir_json_text().contains(&format!(
            "EvidenceVariable is not a resource type of {version}"
        ))
    };
    assert!(names_the_type(fhir_core::FhirVersion::Stu3));
    for version in [
        fhir_core::FhirVersion::R4,
        fhir_core::FhirVersion::R4b,
        fhir_core::FhirVersion::R5,
    ] {
        assert!(!names_the_type(version), "{version}");
    }

    let animal = json!({"resourceType": "Patient", "animal": {"species": {"text": "dog"}}});
    let mut asked_animal = asked(&animal);
    asked_animal.version = fhir_core::FhirVersion::Stu3;
    assert!(
        !validate(&asked_animal).has_errors(),
        "STU3 publishes Patient.animal: {}",
        validate(&asked_animal).to_fhir_json_text()
    );
    for version in [
        fhir_core::FhirVersion::R4,
        fhir_core::FhirVersion::R4b,
        fhir_core::FhirVersion::R5,
    ] {
        asked_animal.version = version;
        assert!(
            validate(&asked_animal).has_errors(),
            "{version} publishes no Patient.animal"
        );
    }
}
