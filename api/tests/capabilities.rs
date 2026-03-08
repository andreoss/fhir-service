use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Capabilities, ConditionalDelete, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    body: String,
}

fn service(capabilities: Capabilities) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new()).with_capabilities(capabilities)
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

fn patient(id: &str, family: &str) -> Vec<u8> {
    json!({"resourceType": "Patient", "id": id, "name": [{"family": family}]})
        .to_string()
        .into_bytes()
}

async fn seed(app: &Service, ids: &[&str], family: &str) {
    for id in ids {
        let reply = ask(app, "PUT", &format!("/Patient/{id}"), &patient(id, family)).await;
        assert!(reply.status.is_success(), "{}", reply.body);
    }
}

async fn held(app: &Service) -> u64 {
    let reply = ask(app, "GET", "/Patient?_summary=count&_total=accurate", b"").await;
    let bundle: Value = serde_json::from_str(&reply.body).unwrap();
    bundle["total"].as_u64().unwrap_or_default()
}

#[tokio::test]
async fn a_condition_matching_many_is_refused_by_default() {
    let app = service(Capabilities::default());
    seed(&app, &["cd-1", "cd-2"], "Many").await;
    let reply = ask(&app, "DELETE", "/Patient?name=Many", b"").await;
    assert_eq!(
        reply.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        reply.body
    );
    assert_eq!(held(&app).await, 2, "nothing was taken");
}

#[tokio::test]
async fn a_condition_matching_one_is_taken_whatever_the_setting() {
    for setting in [ConditionalDelete::Single, ConditionalDelete::Multiple(10)] {
        let app = service(Capabilities {
            conditional_delete: setting,
            ..Capabilities::default()
        });
        seed(&app, &["cd-3"], "One").await;
        let reply = ask(&app, "DELETE", "/Patient?name=One", b"").await;
        assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
        assert_eq!(held(&app).await, 0);
    }
}

#[tokio::test]
async fn multiple_takes_every_match() {
    let app = service(Capabilities {
        conditional_delete: ConditionalDelete::Multiple(10),
        ..Capabilities::default()
    });
    seed(&app, &["cd-4", "cd-5", "cd-6"], "Many").await;
    let reply = ask(&app, "DELETE", "/Patient?name=Many", b"").await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    assert_eq!(held(&app).await, 0);
}

#[tokio::test]
async fn multiple_refuses_past_its_bound_and_takes_nothing() {
    let app = service(Capabilities {
        conditional_delete: ConditionalDelete::Multiple(2),
        ..Capabilities::default()
    });
    seed(&app, &["cd-7", "cd-8", "cd-9"], "Many").await;
    let reply = ask(&app, "DELETE", "/Patient?name=Many", b"").await;
    assert_eq!(
        reply.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        reply.body
    );
    assert_eq!(held(&app).await, 3, "a refused delete takes nothing");
}

#[tokio::test]
async fn a_write_to_an_absent_id_creates_by_default() {
    let app = service(Capabilities::default());
    let reply = ask(&app, "PUT", "/Patient/co-1", &patient("co-1", "Stone")).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

#[tokio::test]
async fn a_write_to_an_absent_id_is_refused_when_the_operator_said_so() {
    let app = service(Capabilities {
        create_on_update: false,
        ..Capabilities::default()
    });
    let reply = ask(&app, "PUT", "/Patient/co-2", &patient("co-2", "Stone")).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    let created = ask(&app, "POST", "/Patient", &patient("co-2", "Stone")).await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "a create is still a create: {}",
        created.body
    );
    let again = ask(&app, "PUT", "/Patient/co-2", &patient("co-2", "Rivers")).await;
    assert_eq!(
        again.status,
        StatusCode::OK,
        "an update of what is there still works: {}",
        again.body
    );
}

#[tokio::test]
async fn a_conditional_update_matching_nothing_is_refused_when_creation_is_off() {
    let app = service(Capabilities {
        create_on_update: false,
        ..Capabilities::default()
    });
    let reply = ask(
        &app,
        "PUT",
        "/Patient?name=Nobody",
        &patient("co-3", "Nobody"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
}

#[tokio::test]
async fn the_statement_declares_what_the_operator_chose() {
    let permissive = service(Capabilities::default());
    let reply = ask(&permissive, "GET", "/metadata", b"").await;
    let statement: Value = serde_json::from_str(&reply.body).unwrap();
    let patient = |statement: &Value| -> Value {
        statement["rest"][0]["resource"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["type"] == "Patient")
            .cloned()
            .unwrap()
    };
    let held = patient(&statement);
    assert_eq!(held["updateCreate"], true);
    assert_eq!(held["conditionalDelete"], "single");

    let strict = service(Capabilities {
        conditional_delete: ConditionalDelete::Multiple(5),
        create_on_update: false,
        ..Capabilities::default()
    });
    let reply = ask(&strict, "GET", "/metadata", b"").await;
    let statement: Value = serde_json::from_str(&reply.body).unwrap();
    let held = patient(&statement);
    assert_eq!(held["updateCreate"], false);
    assert_eq!(held["conditionalDelete"], "multiple");
}
