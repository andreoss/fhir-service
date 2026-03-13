use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::app::served;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::Arc;
use tower::ServiceExt;

fn router() -> axum::Router {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new()).router()
}

async fn description() -> Value {
    let response = router()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/openapi.json")
                .header("host", "localhost")
                .body(Body::empty())
                .expect("a request"),
        )
        .await
        .expect("an answer");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|held| held.to_str().ok()),
        Some("application/json"),
        "a description is read by a tool, not by a FHIR client"
    );
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("the description is json")
}

#[tokio::test]
async fn every_route_the_table_serves_is_described() {
    let held = description().await;
    let paths = held["paths"].as_object().expect("paths");
    for route in served() {
        let written = route.path.replace("{*", "{");
        let found = paths
            .get(&written)
            .unwrap_or_else(|| panic!("{} is served and is not in the description", route.path));
        for verb in route.methods {
            let method = verb.as_str().to_ascii_lowercase();
            assert!(
                found.get(&method).is_some(),
                "{} {} is served and is not in the description",
                verb.as_str(),
                route.path
            );
        }
    }
    assert_eq!(
        paths.len(),
        served().len(),
        "and the description holds nothing the table does not serve"
    );
}

#[tokio::test]
async fn every_route_carries_a_line_saying_what_it_is_for() {
    let held = description().await;
    let paths = held["paths"].as_object().expect("paths");
    for (path, operations) in paths {
        for (method, operation) in operations.as_object().expect("operations") {
            if method == "parameters" {
                continue;
            }
            let summary = operation["summary"].as_str().unwrap_or_default();
            assert!(
                !summary.is_empty(),
                "{method} {path} is described as nothing; add a line to \
                 openapi::DESCRIBED"
            );
        }
    }
}

#[test]
fn a_description_left_behind_by_a_removed_route_is_refused() {
    let serving: BTreeSet<&str> = served().into_iter().map(|route| route.path).collect();
    let extra: Vec<&str> = fhir_api::openapi::described()
        .into_iter()
        .filter(|path| !serving.contains(path))
        .collect();
    assert!(
        extra.is_empty(),
        "these are described and no longer served: {extra:?}"
    );
}

#[tokio::test]
async fn a_path_parameter_is_declared_as_one() {
    let held = description().await;
    let read = &held["paths"]["/{type}/{id}"];
    let parameters = read["parameters"].as_array().expect("parameters");
    let names: Vec<&str> = parameters
        .iter()
        .filter_map(|held| held["name"].as_str())
        .collect();
    assert_eq!(names, vec!["type", "id"]);
    assert!(parameters.iter().all(|held| held["required"] == true));
    assert!(parameters.iter().all(|held| held["in"] == "path"));
}

#[tokio::test]
async fn a_wildcard_is_written_as_a_plain_parameter() {
    let held = description().await;
    let paths = held["paths"].as_object().expect("paths");
    assert!(
        paths.contains_key("/_jobs/{id}/{name}"),
        "OpenAPI has no wildcard; axum's {{*name}} is named plainly"
    );
    assert!(!paths.keys().any(|path| path.contains('*')));
}

#[tokio::test]
async fn a_write_declares_the_body_it_takes() {
    let held = description().await;
    let create = &held["paths"]["/{type}"]["post"];
    assert_eq!(create["requestBody"]["required"], true);
    assert!(
        create["requestBody"]["content"]["application/fhir+json"].is_object(),
        "the media type is the FHIR one: {create}"
    );
    let read = &held["paths"]["/{type}"]["get"];
    assert!(
        read.get("requestBody").is_none(),
        "and a read takes no body"
    );
}

#[tokio::test]
async fn the_refusals_are_described_beside_the_answers() {
    let held = description().await;
    let read = &held["paths"]["/{type}/{id}"]["get"];
    let responses = read["responses"].as_object().expect("responses");
    for status in ["200", "401", "403", "404", "500"] {
        assert!(responses.contains_key(status), "{status} is described");
    }
    let deleted = &held["paths"]["/{type}/{id}"]["delete"]["responses"];
    assert!(deleted["204"].is_object(), "a delete answers no content");
}

#[tokio::test]
async fn the_document_says_what_it_is_and_what_release_it_is_for() {
    let held = description().await;
    assert_eq!(held["openapi"], "3.1.0");
    assert_eq!(held["info"]["title"], "FHIR R4 API");
    assert!(held["info"]["version"]
        .as_str()
        .is_some_and(|held| !held.is_empty()));
}

#[tokio::test]
async fn the_description_describes_itself() {
    let held = description().await;
    assert!(
        held["paths"]["/openapi.json"]["get"].is_object(),
        "a tool reading it learns where it came from"
    );
}
