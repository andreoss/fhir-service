use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const SYSTEM: &str = "http://example.test/CodeSystem/shapes";
const SET: &str = "http://example.test/ValueSet/shapes";
const MAP: &str = "http://example.test/ConceptMap/shapes-to-forms";

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

fn named(body: &str, name: &str) -> Value {
    let held: Value = serde_json::from_str(body).expect("a parameters resource");
    held["parameter"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .find(|entry| entry["name"] == name)
        .and_then(|entry| {
            entry
                .as_object()?
                .iter()
                .find(|(key, _)| key.starts_with("value"))
                .map(|(_, value)| value.clone())
        })
        .unwrap_or(Value::Null)
}

async fn seeded(app: &Service) {
    let system = json!({
        "resourceType": "CodeSystem",
        "id": "shapes",
        "url": SYSTEM,
        "name": "Shapes",
        "status": "active",
        "content": "complete",
        "concept": [
            {"code": "shape", "display": "Shape", "concept": [
                {"code": "round", "display": "Round", "concept": [
                    {"code": "circle", "display": "Circle"}
                ]},
                {"code": "square", "display": "Square"}
            ]}
        ]
    });
    let (status, body) = ask(
        app,
        "PUT",
        "/CodeSystem/shapes",
        system.to_string().as_bytes(),
    )
    .await;
    assert!(status.is_success(), "{body}");

    let set = json!({
        "resourceType": "ValueSet",
        "id": "shapes",
        "url": SET,
        "status": "active",
        "compose": {"include": [{"system": SYSTEM}]}
    });
    let (status, body) = ask(app, "PUT", "/ValueSet/shapes", set.to_string().as_bytes()).await;
    assert!(status.is_success(), "{body}");

    let map = json!({
        "resourceType": "ConceptMap",
        "id": "shapes-to-forms",
        "url": MAP,
        "status": "active",
        "group": [{
            "source": SYSTEM,
            "target": "http://example.test/CodeSystem/forms",
            "element": [{
                "code": "circle",
                "target": [{"code": "disc", "display": "Disc", "equivalence": "equivalent"}]
            }]
        }]
    });
    let (status, body) = ask(
        app,
        "PUT",
        "/ConceptMap/shapes-to-forms",
        map.to_string().as_bytes(),
    )
    .await;
    assert!(status.is_success(), "{body}");
}

#[tokio::test]
async fn a_code_in_the_set_is_validated() {
    let app = service();
    seeded(&app).await;
    let (status, body) = ask(
        &app,
        "GET",
        &format!("/ValueSet/$validate-code?url={SET}&system={SYSTEM}&code=circle"),
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(named(&body, "result"), json!(true), "{body}");
    assert_eq!(named(&body, "display"), json!("Circle"), "{body}");
}

#[tokio::test]
async fn a_code_outside_the_set_is_reported_rather_than_refused() {
    let app = service();
    seeded(&app).await;
    let (status, body) = ask(
        &app,
        "GET",
        &format!("/ValueSet/$validate-code?url={SET}&code=triangle"),
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(named(&body, "result"), json!(false), "{body}");
    assert!(
        named(&body, "message")
            .as_str()
            .unwrap()
            .contains("triangle"),
        "{body}"
    );
}

#[tokio::test]
async fn a_wrong_display_is_reported() {
    let app = service();
    seeded(&app).await;
    let (_, body) = ask(
        &app,
        "GET",
        &format!("/ValueSet/$validate-code?url={SET}&code=circle&display=Sphere"),
        b"",
    )
    .await;
    assert_eq!(named(&body, "result"), json!(false), "{body}");
}

#[tokio::test]
async fn the_set_may_be_named_by_its_own_address() {
    let app = service();
    seeded(&app).await;
    let (status, body) = ask(
        &app,
        "GET",
        "/ValueSet/shapes/$validate-code?code=square",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(named(&body, "result"), json!(true), "{body}");
}

#[tokio::test]
async fn a_code_in_the_system_is_validated() {
    let app = service();
    seeded(&app).await;
    let (status, body) = ask(
        &app,
        "GET",
        &format!("/CodeSystem/$validate-code?url={SYSTEM}&code=round"),
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(named(&body, "result"), json!(true), "{body}");
    assert_eq!(named(&body, "display"), json!("Round"));
}

#[tokio::test]
async fn a_lookup_answers_the_display_the_system_gives() {
    let app = service();
    seeded(&app).await;
    let (status, body) = ask(
        &app,
        "GET",
        &format!("/CodeSystem/$lookup?system={SYSTEM}&code=circle"),
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(named(&body, "display"), json!("Circle"), "{body}");
    assert_eq!(named(&body, "name"), json!("Shapes"), "{body}");
}

#[tokio::test]
async fn a_code_the_system_does_not_hold_is_not_found() {
    let app = service();
    seeded(&app).await;
    let (status, _) = ask(
        &app,
        "GET",
        &format!("/CodeSystem/$lookup?system={SYSTEM}&code=triangle"),
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn subsumption_answers_the_four_outcomes() {
    let app = service();
    seeded(&app).await;
    for (a, b, expected) in [
        ("round", "round", "equivalent"),
        ("round", "circle", "subsumes"),
        ("circle", "round", "subsumed-by"),
        ("circle", "square", "not-subsumed"),
    ] {
        let (status, body) = ask(
            &app,
            "GET",
            &format!("/CodeSystem/$subsumes?system={SYSTEM}&codeA={a}&codeB={b}"),
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            named(&body, "outcome").as_str().unwrap(),
            expected,
            "{a} against {b}: {body}"
        );
    }
}

#[tokio::test]
async fn find_matches_answers_what_it_matched() {
    let app = service();
    seeded(&app).await;
    for path in ["$find-matches", "$compose"] {
        let (status, body) = ask(
            &app,
            "GET",
            &format!("/CodeSystem/{path}?system={SYSTEM}&property.value=circ"),
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert!(body.contains("circle"), "{path}: {body}");
        assert!(!body.contains("\"square\""), "{path}: {body}");
    }
}

#[tokio::test]
async fn a_translation_answers_what_the_map_says() {
    let app = service();
    seeded(&app).await;
    let (status, body) = ask(
        &app,
        "GET",
        &format!("/ConceptMap/$translate?url={MAP}&system={SYSTEM}&code=circle"),
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(named(&body, "result"), json!(true), "{body}");
    assert!(body.contains("disc"), "{body}");
}

#[tokio::test]
async fn a_code_no_map_translates_is_reported() {
    let app = service();
    seeded(&app).await;
    let (status, body) = ask(
        &app,
        "GET",
        &format!("/ConceptMap/$translate?url={MAP}&system={SYSTEM}&code=square"),
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(named(&body, "result"), json!(false), "{body}");
}

#[tokio::test]
async fn a_map_may_be_named_by_its_own_address() {
    let app = service();
    seeded(&app).await;
    let (status, body) = ask(
        &app,
        "GET",
        &format!("/ConceptMap/shapes-to-forms/$translate?system={SYSTEM}&code=circle"),
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(named(&body, "result"), json!(true), "{body}");
}

#[tokio::test]
async fn the_closure_this_instance_does_not_keep_is_refused_rather_than_empty() {
    let app = service();
    let (status, body) = ask(&app, "POST", "/$closure", b"{}").await;
    assert!(status.is_client_error(), "{status}: {body}");
    assert!(body.contains("closure table"), "{body}");
}

#[tokio::test]
async fn a_posted_parameters_body_asks_the_same_question() {
    let app = service();
    seeded(&app).await;
    let asked = json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "url", "valueUri": SET},
            {"name": "coding", "valueCoding": {"system": SYSTEM, "code": "circle"}}
        ]
    })
    .to_string();
    let (status, body) = ask(&app, "POST", "/ValueSet/$validate-code", asked.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(named(&body, "result"), json!(true), "{body}");
}
