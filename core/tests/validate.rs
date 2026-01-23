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
fn a_create_carrying_an_id_is_reported() {
    let body = json!({"resourceType": "Patient", "id": "pt-1"});
    let mut request = asked(&body);
    request.mode = Mode::Create;
    let report = validate(&request);
    assert!(!report.has_errors());
    assert!(codes(&report).iter().any(|code| code.starts_with("warning")));
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
