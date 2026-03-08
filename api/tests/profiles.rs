use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{OnWrite, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    body: String,
}

const URL: &str = "http://example.test/StructureDefinition/watched-patient";

fn service(on_write: OnWrite) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new()).with_profile_validation(on_write)
}

async fn ask(app: &Service, method: &str, uri: &str, body: &[u8]) -> Reply {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json")
        .body(Body::from(body.to_vec()))
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn definition() -> Vec<u8> {
    json!({
        "resourceType": "StructureDefinition",
        "id": "watched",
        "url": URL,
        "name": "Watched",
        "status": "active",
        "kind": "resource",
        "abstract": false,
        "type": "Patient",
        "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Patient",
        "derivation": "constraint",
        "differential": {"element": [
            {"path": "Patient.identifier", "min": 1, "max": "*"},
            {"path": "Patient.gender", "min": 1, "max": "1", "fixedCode": "female"}
        ]}
    })
    .to_string()
    .into_bytes()
}

async fn with_definition(on_write: OnWrite) -> Service {
    let app = service(on_write);
    let reply = ask(&app, "PUT", "/StructureDefinition/watched", &definition()).await;
    assert!(reply.status.is_success(), "{}", reply.body);
    app
}

fn patient(good: bool) -> Vec<u8> {
    let mut body = json!({
        "resourceType": "Patient",
        "id": "pp-1",
        "meta": {"profile": [URL]}
    });
    if good {
        body["identifier"] = json!([{"system": "urn:s", "value": "a"}]);
        body["gender"] = json!("female");
    }
    body.to_string().into_bytes()
}

fn issues(reply: &Reply) -> String {
    let value: Value = serde_json::from_str(&reply.body).expect("a json body");
    value["issue"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|issue| issue["diagnostics"].as_str())
                .collect::<Vec<&str>>()
                .join(" | ")
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn validate_applies_the_rules_a_stored_profile_states() {
    let app = with_definition(OnWrite::default()).await;
    let reply = ask(&app, "POST", "/Patient/$validate", &patient(false)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let told = issues(&reply);
    assert!(told.contains("Patient.identifier"), "{told}");
    assert!(told.contains("Patient.gender"), "{told}");
}

#[tokio::test]
async fn validate_reports_a_value_the_profile_fixes() {
    let app = with_definition(OnWrite::default()).await;
    let body = json!({
        "resourceType": "Patient",
        "id": "pp-1",
        "meta": {"profile": [URL]},
        "identifier": [{"system": "urn:s", "value": "a"}],
        "gender": "male"
    })
    .to_string()
    .into_bytes();
    let reply = ask(&app, "POST", "/Patient/$validate", &body).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let told = issues(&reply);
    assert!(told.contains("fixes"), "{told}");
    assert!(told.contains("Patient.gender"), "{told}");
}

#[tokio::test]
async fn validate_passes_a_resource_the_profile_allows() {
    let app = with_definition(OnWrite::default()).await;
    let reply = ask(&app, "POST", "/Patient/$validate", &patient(true)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let told = issues(&reply);
    assert!(!told.contains("Patient.identifier"), "{told}");
    assert!(!told.contains("fixes"), "{told}");
}

#[tokio::test]
async fn validate_refuses_a_profile_no_definition_resolves() {
    let app = service(OnWrite::default());
    let reply = ask(&app, "POST", "/Patient/$validate", &patient(true)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let told = issues(&reply);
    assert!(
        told.contains("could not be resolved"),
        "an unresolved profile is refused, not reported as checked: {told}"
    );
}

#[tokio::test]
async fn a_write_is_not_judged_against_a_profile_unless_the_operator_asked() {
    let app = with_definition(OnWrite::default()).await;
    let reply = ask(&app, "PUT", "/Patient/pp-1", &patient(false)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

#[tokio::test]
async fn a_create_is_judged_when_the_operator_asked_for_it() {
    let app = with_definition(OnWrite {
        create: true,
        update: false,
    })
    .await;
    let refused = ask(&app, "POST", "/Patient", &patient(false)).await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.body
    );
    assert!(
        refused.body.contains("Patient.identifier"),
        "{}",
        refused.body
    );
    let allowed = ask(&app, "POST", "/Patient", &patient(true)).await;
    assert_eq!(allowed.status, StatusCode::CREATED, "{}", allowed.body);
}

#[tokio::test]
async fn an_update_is_judged_when_the_operator_asked_for_it() {
    let app = with_definition(OnWrite {
        create: true,
        update: true,
    })
    .await;
    ask(&app, "PUT", "/Patient/pp-1", &patient(true)).await;
    let refused = ask(&app, "PUT", "/Patient/pp-1", &patient(false)).await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.body
    );
}

#[tokio::test]
async fn a_write_claiming_a_profile_the_instance_cannot_resolve_is_refused() {
    let app = service(OnWrite {
        create: true,
        update: true,
    });
    let reply = ask(&app, "POST", "/Patient", &patient(true)).await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.body
    );
    assert!(
        reply.body.contains("could not be resolved"),
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn a_resource_claiming_nothing_is_written_whatever_the_setting() {
    let app = service(OnWrite {
        create: true,
        update: true,
    });
    let body = json!({"resourceType": "Patient", "id": "pp-2", "active": true})
        .to_string()
        .into_bytes();
    let reply = ask(&app, "PUT", "/Patient/pp-2", &body).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

#[tokio::test]
async fn a_resource_claiming_a_base_profile_is_judged_by_the_structural_pass() {
    let app = service(OnWrite {
        create: true,
        update: true,
    });
    let body = json!({
        "resourceType": "Patient",
        "id": "pp-3",
        "meta": {"profile": ["http://hl7.org/fhir/StructureDefinition/Patient"]},
        "active": true
    })
    .to_string()
    .into_bytes();
    let reply = ask(&app, "PUT", "/Patient/pp-3", &body).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

const ALLOWED: &str = "http://example.test/StructureDefinition/allowed";

fn accepting(named: &[&str]) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new()).accepting_profiles(
        fhir_api::AllowedProfiles::new(named.iter().map(|held| (*held).to_owned()).collect()),
    )
}

fn claiming(id: &str, profile: Option<&str>) -> Vec<u8> {
    let mut body = json!({"resourceType": "Patient", "id": id, "active": true});
    if let Some(profile) = profile {
        body["meta"] = json!({"profile": [profile]});
    }
    body.to_string().into_bytes()
}

#[tokio::test]
async fn nothing_listed_accepts_anything() {
    let app = accepting(&[]);
    let reply = ask(&app, "PUT", "/Patient/al-1", &claiming("al-1", None)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

#[tokio::test]
async fn a_resource_claiming_an_accepted_profile_is_written() {
    let app = accepting(&[ALLOWED]);
    let reply = ask(
        &app,
        "PUT",
        "/Patient/al-2",
        &claiming("al-2", Some(ALLOWED)),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

#[tokio::test]
async fn a_resource_claiming_none_of_them_is_refused() {
    let app = accepting(&[ALLOWED]);
    let reply = ask(&app, "PUT", "/Patient/al-3", &claiming("al-3", None)).await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.body
    );
    assert!(reply.body.contains(ALLOWED), "the refusal names the list");
}

#[tokio::test]
async fn a_resource_claiming_another_profile_is_refused() {
    let app = accepting(&[ALLOWED]);
    let reply = ask(
        &app,
        "PUT",
        "/Patient/al-4",
        &claiming(
            "al-4",
            Some("http://example.test/StructureDefinition/other"),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_versioned_claim_matches_the_profile_it_names() {
    let app = accepting(&[ALLOWED]);
    let reply = ask(
        &app,
        "PUT",
        "/Patient/al-5",
        &claiming("al-5", Some(&format!("{ALLOWED}|1.0.0"))),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}
