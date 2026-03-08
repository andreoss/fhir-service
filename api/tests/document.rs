use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
}

async fn ask(app: &Service, method: &str, uri: &str, body: &[u8]) -> (StatusCode, String) {
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
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn put(app: &Service, uri: &str, body: Value) {
    let (status, told) = ask(app, "PUT", uri, body.to_string().as_bytes()).await;
    assert!(status.is_success(), "{uri}: {told}");
}

async fn seeded(dangling: bool) -> Service {
    let app = service();
    put(
        &app,
        "/Patient/dc-p",
        json!({"resourceType": "Patient", "id": "dc-p"}),
    )
    .await;
    put(
        &app,
        "/Practitioner/dc-a",
        json!({"resourceType": "Practitioner", "id": "dc-a"}),
    )
    .await;
    put(
        &app,
        "/Observation/dc-o",
        json!({
            "resourceType": "Observation",
            "id": "dc-o",
            "status": "final",
            "code": {"text": "probe"},
            "subject": {"reference": "Patient/dc-p"}
        }),
    )
    .await;
    let mut entries = vec![json!({"reference": "Observation/dc-o"})];
    if dangling {
        entries.push(json!({"reference": "Observation/nobody"}));
    }
    put(
        &app,
        "/Composition/dc-1",
        json!({
            "resourceType": "Composition",
            "id": "dc-1",
            "status": "final",
            "type": {"text": "summary"},
            "date": "2026-01-01",
            "title": "A summary",
            "subject": {"reference": "Patient/dc-p"},
            "author": [{"reference": "Practitioner/dc-a"}],
            "section": [{
                "title": "Observations",
                "entry": entries,
                "section": [{
                    "title": "Nested",
                    "entry": [{"reference": "Practitioner/dc-a"}]
                }]
            }]
        }),
    )
    .await;
    app
}

fn types(body: &str) -> Vec<String> {
    let bundle: Value = serde_json::from_str(body).expect("a bundle");
    let mut held: Vec<String> = bundle["entry"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            entry["resource"]["resourceType"]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    held.sort();
    held
}

#[tokio::test]
async fn a_composition_becomes_the_document_it_describes() {
    let app = seeded(false).await;
    let (status, body) = ask(&app, "GET", "/Composition/dc-1/$document", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bundle: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(bundle["type"], "document");
    assert_eq!(
        bundle["entry"][0]["resource"]["resourceType"], "Composition",
        "the composition comes first"
    );
    assert_eq!(
        types(&body),
        vec![
            "Composition".to_owned(),
            "Observation".to_owned(),
            "Patient".to_owned(),
            "Practitioner".to_owned()
        ],
        "{body}"
    );
}

#[tokio::test]
async fn a_resource_referenced_twice_appears_once() {
    let app = seeded(false).await;
    let (_, body) = ask(&app, "GET", "/Composition/dc-1/$document", b"").await;
    let held = types(&body);
    assert_eq!(
        held.iter().filter(|name| *name == "Practitioner").count(),
        1,
        "{body}"
    );
}

#[tokio::test]
async fn a_reference_that_resolves_to_nothing_is_named() {
    let app = seeded(true).await;
    let (status, body) = ask(&app, "GET", "/Composition/dc-1/$document", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("OperationOutcome"), "{body}");
    assert!(body.contains("Observation/nobody"), "{body}");
}

#[tokio::test]
async fn the_type_level_form_names_the_composition_in_its_body() {
    let app = seeded(false).await;
    let asked = json!({
        "resourceType": "Parameters",
        "parameter": [{"name": "id", "valueString": "dc-1"}]
    })
    .to_string();
    let (status, body) = ask(&app, "POST", "/Composition/$document", asked.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(types(&body).len(), 4, "{body}");
}

#[tokio::test]
async fn the_document_is_stored_when_the_client_asks() {
    let app = seeded(false).await;
    let (status, body) = ask(&app, "GET", "/Composition/dc-1/$document?persist=true", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, held) = ask(&app, "GET", "/Bundle?_summary=count&_total=accurate", b"").await;
    assert_eq!(status, StatusCode::OK, "{held}");
    let counted: Value = serde_json::from_str(&held).unwrap();
    assert_eq!(counted["total"], 1, "{held}");
}

#[tokio::test]
async fn nothing_is_stored_unless_the_client_asks() {
    let app = seeded(false).await;
    ask(&app, "GET", "/Composition/dc-1/$document", b"").await;
    let (_, held) = ask(&app, "GET", "/Bundle?_summary=count&_total=accurate", b"").await;
    let counted: Value = serde_json::from_str(&held).unwrap();
    assert_eq!(counted["total"], 0, "{held}");
}

#[tokio::test]
async fn a_composition_that_is_not_there_is_not_found() {
    let app = seeded(false).await;
    let (status, _) = ask(&app, "GET", "/Composition/nobody/$document", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_absolute_reference_is_not_followed() {
    let app = service();
    put(
        &app,
        "/Composition/dc-2",
        json!({
            "resourceType": "Composition",
            "id": "dc-2",
            "status": "final",
            "type": {"text": "summary"},
            "date": "2026-01-01",
            "title": "Outside",
            "author": [{"display": "somebody"}],
            "section": [{
                "title": "Elsewhere",
                "entry": [{"reference": "https://elsewhere.test/fhir/Observation/1"}]
            }]
        }),
    )
    .await;
    let (status, body) = ask(&app, "GET", "/Composition/dc-2/$document", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("outside this instance"), "{body}");
}

#[tokio::test]
async fn a_posted_document_becomes_a_reference_and_a_binary() {
    let app = service();
    let bundle = json!({
        "resourceType": "Bundle",
        "type": "document",
        "identifier": {"system": "urn:ietf:rfc:3986", "value": "urn:uuid:doc-0001"},
        "timestamp": "2026-01-01T00:00:00Z",
        "entry": [{
            "fullUrl": "urn:uuid:comp-1",
            "resource": {
                "resourceType": "Composition",
                "id": "comp-1",
                "status": "final",
                "type": {"text": "summary"},
                "date": "2026-01-01",
                "title": "A summary",
                "author": [{"display": "somebody"}],
                "text": {"status": "generated", "div": "<div>the whole story</div>"}
            }
        }]
    })
    .to_string();
    let (status, body) = ask(&app, "POST", "/", bundle.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(held["resourceType"], "DocumentReference");
    let id = held["id"].as_str().expect("an id").to_owned();
    assert_eq!(id.len(), 64, "the id is derived from the identifier");
    assert_eq!(
        held["content"][0]["attachment"]["url"],
        format!("Binary/{id}")
    );

    let (status, artifact) = ask(&app, "GET", &format!("/Binary/{id}"), b"").await;
    assert_eq!(status, StatusCode::OK, "{artifact}");
    let binary: Value = serde_json::from_str(&artifact).unwrap();
    assert_eq!(binary["contentType"], "text/html");
}

#[tokio::test]
async fn the_same_document_sent_twice_is_one_reference() {
    let app = service();
    let bundle = json!({
        "resourceType": "Bundle",
        "type": "document",
        "identifier": {"value": "urn:uuid:doc-0002"},
        "entry": [{"resource": {
            "resourceType": "Composition",
            "id": "comp-2",
            "status": "final",
            "type": {"text": "summary"},
            "date": "2026-01-01",
            "title": "Again",
            "author": [{"display": "somebody"}],
            "text": {"status": "generated", "div": "<div>one</div>"}
        }}]
    })
    .to_string();
    let (_, first) = ask(&app, "POST", "/", bundle.as_bytes()).await;
    let (_, second) = ask(&app, "POST", "/", bundle.as_bytes()).await;
    let one: Value = serde_json::from_str(&first).unwrap();
    let other: Value = serde_json::from_str(&second).unwrap();
    assert_eq!(one["id"], other["id"], "the same id");
    assert_eq!(other["meta"]["versionId"], "2", "a second version");

    let (_, counted) = ask(
        &app,
        "GET",
        "/DocumentReference?_summary=count&_total=accurate",
        b"",
    )
    .await;
    let held: Value = serde_json::from_str(&counted).unwrap();
    assert_eq!(held["total"], 1, "and one reference: {counted}");
}

#[tokio::test]
async fn a_document_carrying_no_identifier_is_refused() {
    let app = service();
    let bundle = json!({
        "resourceType": "Bundle",
        "type": "document",
        "entry": [{"resource": {
            "resourceType": "Composition",
            "id": "comp-3",
            "status": "final",
            "type": {"text": "summary"},
            "date": "2026-01-01",
            "title": "Nameless",
            "author": [{"display": "somebody"}],
            "text": {"status": "generated", "div": "<div>one</div>"}
        }}]
    })
    .to_string();
    let (status, body) = ask(&app, "POST", "/", bundle.as_bytes()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("identifier"), "{body}");
}

#[tokio::test]
async fn a_document_carrying_no_narrative_is_refused() {
    let app = service();
    let bundle = json!({
        "resourceType": "Bundle",
        "type": "document",
        "identifier": {"value": "urn:uuid:doc-0003"},
        "entry": [{"resource": {
            "resourceType": "Composition",
            "id": "comp-4",
            "status": "final",
            "type": {"text": "summary"},
            "date": "2026-01-01",
            "title": "Silent",
            "author": [{"display": "somebody"}]
        }}]
    })
    .to_string();
    let (status, body) = ask(&app, "POST", "/", bundle.as_bytes()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("narrative"), "{body}");
}
