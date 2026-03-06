use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    body: String,
}

fn service(version: FhirVersion) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), version, Vec::new())
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

fn ids(body: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(body).expect("the answer is json");
    value["entry"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item["resource"]["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn seeded(version: FhirVersion, with_source: bool) -> Service {
    let app = service(version);
    for (id, language, source) in [
        ("cm-1", "en", "http://example.org/records/one"),
        ("cm-2", "fr", "http://example.org/records/two"),
    ] {
        let meta = if with_source {
            format!(r#","meta":{{"source":"{source}"}}"#)
        } else {
            String::new()
        };
        let body = format!(
            r#"{{"resourceType":"Patient","id":"{id}","language":"{language}"{meta},"name":[{{"family":"Stone"}}]}}"#
        );
        let reply = ask(&app, "PUT", &format!("/Patient/{id}"), body.as_bytes()).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    }
    app
}

#[tokio::test]
async fn a_search_names_the_source_the_release_gives_every_type() {
    let app = seeded(FhirVersion::R4, true).await;
    let held = ask(
        &app,
        "GET",
        "/Patient?_source=http%3A%2F%2Fexample.org%2Frecords%2Fone",
        &[],
    )
    .await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.body);
    assert_eq!(ids(&held.body), vec!["cm-1".to_owned()]);

    let either = ask(
        &app,
        "GET",
        "/Patient?_source=http%3A%2F%2Fexample.org%2Frecords%2Ftwo,http%3A%2F%2Fexample.org%2Frecords%2Fone",
        &[],
    )
    .await;
    assert_eq!(either.status, StatusCode::OK, "{}", either.body);
    assert_eq!(
        ids(&either.body),
        vec!["cm-1".to_owned(), "cm-2".to_owned()]
    );

    let none = ask(
        &app,
        "GET",
        "/Patient?_source=http%3A%2F%2Fexample.org%2Frecords%2Fthree",
        &[],
    )
    .await;
    assert_eq!(none.status, StatusCode::OK, "{}", none.body);
    assert!(ids(&none.body).is_empty(), "{}", none.body);
}

#[tokio::test]
async fn a_search_names_the_language_the_latest_release_gives_every_type() {
    let app = seeded(FhirVersion::R5, true).await;
    let held = ask(&app, "GET", "/Patient?_language=en", &[]).await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.body);
    assert_eq!(ids(&held.body), vec!["cm-1".to_owned()]);

    let either = ask(&app, "GET", "/Patient?_language=fr,en", &[]).await;
    assert_eq!(either.status, StatusCode::OK, "{}", either.body);
    assert_eq!(
        ids(&either.body),
        vec!["cm-1".to_owned(), "cm-2".to_owned()]
    );

    let none = ask(&app, "GET", "/Patient?_language=de", &[]).await;
    assert_eq!(none.status, StatusCode::OK, "{}", none.body);
    assert!(ids(&none.body).is_empty(), "{}", none.body);
}

#[tokio::test]
async fn a_release_that_names_no_source_refuses_one() {
    let app = seeded(FhirVersion::Stu3, false).await;
    let reply = ask(
        &app,
        "GET",
        "/Patient?_source=http%3A%2F%2Fexample.org%2Frecords%2Fone",
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(
        reply.body.contains("_source"),
        "the refusal names the parameter: {}",
        reply.body
    );
}

#[tokio::test]
async fn an_earlier_release_refuses_the_language_it_does_not_name() {
    let app = seeded(FhirVersion::R4, true).await;
    let reply = ask(&app, "GET", "/Patient?_language=en", &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(
        reply.body.contains("_language"),
        "the refusal names the parameter: {}",
        reply.body
    );
}

#[tokio::test]
async fn the_parameters_the_search_was_answered_by_come_back_in_its_self_link() {
    let app = seeded(FhirVersion::R5, true).await;
    let reply = ask(&app, "GET", "/Patient?_language=en&_source=x", &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let value: serde_json::Value = serde_json::from_str(&reply.body).expect("the answer is json");
    let links = value["link"].as_array().expect("the bundle carries links");
    let itself = links
        .iter()
        .find(|link| link["relation"] == "self")
        .and_then(|link| link["url"].as_str())
        .expect("the bundle names itself");
    assert!(
        itself.contains("_language=en"),
        "the search is named back: {itself}"
    );
    assert!(
        itself.contains("_source=x"),
        "the search is named back: {itself}"
    );
}

async fn collections(version: FhirVersion) -> Service {
    let app = service(version);
    for (path, body) in [
        ("/Patient/101", r#"{"resourceType":"Patient","id":"101"}"#),
        ("/Patient/102", r#"{"resourceType":"Patient","id":"102"}"#),
        ("/Patient/103", r#"{"resourceType":"Patient","id":"103"}"#),
        (
            "/Group/grp-1",
            &format!(
                r#"{{"resourceType":"Group","id":"grp-1","type":"person"{},"member":[
                {{"entity":{{"reference":"Patient/101"}}}},
                {{"entity":{{"reference":"Patient/102"}},"inactive":true}}]}}"#,
                match version {
                    FhirVersion::R5 => r#","membership":"enumerated""#,
                    _ => r#","actual":true"#,
                }
            ),
        ),
        (
            "/List/lst-1",
            r#"{"resourceType":"List","id":"lst-1","status":"current","mode":"working","entry":[
                {"item":{"reference":"Patient/103"}},
                {"item":{"reference":"Patient/102"},"deleted":true}]}"#,
        ),
        (
            "/CareTeam/ct-1",
            r#"{"resourceType":"CareTeam","id":"ct-1","status":"active","participant":[
                {"member":{"reference":"Patient/103"}}]}"#,
        ),
        (
            "/Observation/ob-1",
            r#"{"resourceType":"Observation","id":"ob-1","status":"final","code":{"text":"weight"},"subject":{"reference":"Patient/101"}}"#,
        ),
        (
            "/Observation/ob-2",
            r#"{"resourceType":"Observation","id":"ob-2","status":"final","code":{"text":"weight"},"subject":{"reference":"Patient/102"}}"#,
        ),
    ] {
        let reply = ask(&app, "PUT", path, body.as_bytes()).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{path} {}", reply.body);
    }
    app
}

fn sorted(body: &str) -> Vec<String> {
    let mut held = ids(body);
    held.sort();
    held
}

#[tokio::test]
async fn a_search_names_the_members_a_collection_counts_as_active() {
    let app = collections(FhirVersion::R5).await;
    for (uri, expected) in [
        ("/Patient?_in=Group/grp-1", vec!["101"]),
        ("/Patient?_in=grp-1", vec!["101"]),
        ("/Patient?_in=List/lst-1", vec!["103"]),
        ("/Patient?_in=CareTeam/ct-1", vec!["103"]),
        ("/Patient?_in=Group/grp-1,List/lst-1", vec!["101", "103"]),
    ] {
        let reply = ask(&app, "GET", uri, &[]).await;
        assert_eq!(reply.status, StatusCode::OK, "{uri} {}", reply.body);
        assert_eq!(sorted(&reply.body), expected, "{uri}");
    }
}

#[tokio::test]
async fn a_search_names_what_a_collection_does_not_count() {
    let app = collections(FhirVersion::R5).await;
    let reply = ask(&app, "GET", "/Patient?_in:not=Group/grp-1", &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(sorted(&reply.body), vec!["102", "103"]);
    for uri in [
        "/Patient?_in=Group/lst-1",
        "/Patient?_in=Patient/101",
        "/Patient?_in=Group/grp-1&_in=Patient/101",
    ] {
        let reply = ask(&app, "GET", uri, &[]).await;
        assert_eq!(reply.status, StatusCode::OK, "{uri} {}", reply.body);
        assert!(sorted(&reply.body).is_empty(), "{uri} {}", reply.body);
    }
}

#[tokio::test]
async fn a_chained_search_follows_the_members_of_a_collection() {
    let app = collections(FhirVersion::R5).await;
    let reply = ask(&app, "GET", "/Observation?subject._in=Group/grp-1", &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(sorted(&reply.body), vec!["ob-1"]);
}

#[tokio::test]
async fn a_release_that_names_no_membership_parameter_refuses_one() {
    let app = collections(FhirVersion::R4).await;
    let reply = ask(&app, "GET", "/Patient?_in=Group/grp-1", &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body.contains("_in"), "{}", reply.body);
    let chained = ask(&app, "GET", "/Observation?subject._in=Group/grp-1", &[]).await;
    assert_eq!(chained.status, StatusCode::BAD_REQUEST, "{}", chained.body);
}
