use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service};
use fhir_core::{FhirInstant, FhirVersion};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    body: String,
}

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Ok(())),
    }];
    Service::new(Arc::new(store), FhirVersion::R4, dependencies)
}

async fn request(app: &Service, method: &str, uri: &str, body: &[u8]) -> Reply {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .body(Body::from(body.to_vec()))
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    Reply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn json(reply: &Reply) -> Value {
    serde_json::from_str(&reply.body).expect("body must be json")
}

fn conversion(collection: &str, root: &str, data: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "inputData", "valueString": data},
            {"name": "inputDataType", "valueString": "hl7v2"},
            {"name": "templateCollectionReference", "valueString": collection},
            {"name": "rootTemplate", "valueString": root}
        ]
    }))
    .unwrap()
}

#[tokio::test]
async fn convert_data_renders_the_named_template() {
    let app = service();
    let body = conversion(
        fhir_core::convert::DEFAULT_COLLECTION,
        "Patient",
        "PID|1||pt-1||Ann^Bea||19800401|female",
    );
    let reply = request(&app, "POST", "/$convert-data", &body).await;
    assert_eq!(reply.status, StatusCode::OK);
    let value = json(&reply);
    assert_eq!(value["resourceType"], "Patient");
    assert_eq!(value["id"], "pt-1");
    assert_eq!(value["name"][0]["family"], "Ann");
}

#[tokio::test]
async fn convert_data_persists_nothing() {
    let app = service();
    let body = conversion(
        fhir_core::convert::DEFAULT_COLLECTION,
        "Patient",
        "PID|1||pt-2||Cyd",
    );
    assert_eq!(request(&app, "POST", "/$convert-data", &body).await.status, StatusCode::OK);
    let read = request(&app, "GET", "/Patient/pt-2", &[]).await;
    assert_eq!(read.status, StatusCode::NOT_FOUND);
    let searched = request(&app, "GET", "/Patient", &[]).await;
    assert_eq!(json(&searched)["total"], 0);
}

#[tokio::test]
async fn a_collection_outside_the_registry_is_refused() {
    let app = service();
    let body = conversion("urn:collection:other", "Patient", "PID|1||pt-3");
    let reply = request(&app, "POST", "/$convert-data", &body).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(json(&reply)["issue"][0]["code"], "forbidden");
}

#[tokio::test]
async fn a_conversion_missing_its_input_is_refused() {
    let app = service();
    let empty = serde_json::to_vec(&serde_json::json!({"resourceType": "Parameters"})).unwrap();
    let reply = request(&app, "POST", "/$convert-data", &empty).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&reply)["issue"][0]["code"], "invalid");
    let malformed = request(&app, "POST", "/$convert-data", b"not json").await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_unknown_template_and_input_form_are_refused() {
    let app = service();
    let unknown = conversion(fhir_core::convert::DEFAULT_COLLECTION, "Nonesuch", "PID|1");
    assert_eq!(
        request(&app, "POST", "/$convert-data", &unknown).await.status,
        StatusCode::BAD_REQUEST
    );
    let form = serde_json::to_vec(&serde_json::json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "inputData", "valueString": "x"},
            {"name": "inputDataType", "valueString": "ccda"},
            {"name": "templateCollectionReference", "valueString": fhir_core::convert::DEFAULT_COLLECTION},
            {"name": "rootTemplate", "valueString": "Patient"}
        ]
    }))
    .unwrap();
    let reply = request(&app, "POST", "/$convert-data", &form).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&reply)["issue"][0]["code"], "not-supported");
}

async fn create_patient(app: &Service, id: &str) {
    let body = format!(r#"{{"resourceType":"Patient","id":"{id}","active":true}}"#);
    let reply = request(app, "POST", "/Patient", body.as_bytes()).await;
    assert!(reply.status.is_success(), "{}", reply.body);
}

#[tokio::test]
async fn validate_reports_a_submitted_resource_without_storing_it() {
    let app = service();
    let body = br#"{"resourceType":"Patient","id":"pt-v1","active":true}"#;
    let reply = request(&app, "POST", "/Patient/$validate", body).await;
    assert_eq!(reply.status, StatusCode::OK);
    let value = json(&reply);
    assert_eq!(value["resourceType"], "OperationOutcome");
    assert_eq!(value["issue"][0]["severity"], "information");
    assert_eq!(request(&app, "GET", "/Patient/pt-v1", &[]).await.status, StatusCode::NOT_FOUND);
    assert_eq!(json(&request(&app, "GET", "/Patient", &[]).await)["total"], 0);
}

#[tokio::test]
async fn validate_reports_a_resource_that_contradicts_its_path() {
    let app = service();
    let body = br#"{"resourceType":"Observation","id":"ob-1"}"#;
    let reply = request(&app, "POST", "/Patient/$validate", body).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(json(&reply)["issue"][0]["severity"], "error");
}

#[tokio::test]
async fn validate_reads_a_stored_resource_and_leaves_its_version() {
    let app = service();
    create_patient(&app, "pt-v2").await;
    let reply = request(&app, "GET", "/Patient/pt-v2/$validate", &[]).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(json(&reply)["issue"][0]["severity"], "information");
    let history = request(&app, "GET", "/Patient/pt-v2/_history", &[]).await;
    assert_eq!(json(&history)["total"], 1);
}

#[tokio::test]
async fn validate_checks_a_profile_and_a_narrative() {
    let app = service();
    let body = br#"{"resourceType":"Patient","id":"pt-v3","text":{"status":"invented","div":"plain"}}"#;
    let reply = request(&app, "POST", "/Patient/$validate?profile=http://x/one", body).await;
    assert_eq!(reply.status, StatusCode::OK);
    let issues = json(&reply)["issue"].as_array().cloned().unwrap_or_default();
    assert!(issues.len() >= 3, "{}", reply.body);
    assert!(issues.iter().all(|issue| issue["severity"] == "error"));
}

#[tokio::test]
async fn validate_takes_the_resource_from_an_input_parameters_body() {
    let app = service();
    let body = serde_json::to_vec(&serde_json::json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "resource", "resource": {"resourceType": "Patient", "id": "pt-v4"}},
            {"name": "mode", "valueCode": "create"}
        ]
    }))
    .unwrap();
    let reply = request(&app, "POST", "/Patient/$validate", &body).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(json(&reply)["issue"][0]["severity"], "warning");
}

#[tokio::test]
async fn validate_refuses_a_malformed_body_and_an_unknown_mode() {
    let app = service();
    let malformed = request(&app, "POST", "/Patient/$validate", b"not json").await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    let empty = request(&app, "POST", "/Patient/$validate", &[]).await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    let body = br#"{"resourceType":"Patient","id":"pt-v5"}"#;
    let mode = request(&app, "POST", "/Patient/$validate?mode=nonesuch", body).await;
    assert_eq!(mode.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn validate_reports_an_unknown_stored_resource() {
    let app = service();
    let reply = request(&app, "GET", "/Patient/nonesuch/$validate", &[]).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

fn stepping_service() -> Service {
    let step = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let clock = Arc::new(move || {
        let minute = step.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        FhirInstant::parse(&format!("2026-09-06T04:{minute:02}:00.000Z")).unwrap()
    });
    let store = MemoryStore::with_clock(clock);
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
}

async fn create(app: &Service, resource: serde_json::Value) {
    let kind = resource["resourceType"].as_str().unwrap().to_owned();
    let body = serde_json::to_vec(&resource).unwrap();
    let reply = request(app, "POST", &format!("/{kind}"), &body).await;
    assert!(reply.status.is_success(), "{}", reply.body);
}

fn observation(id: &str, patient: &str) -> serde_json::Value {
    serde_json::json!({
        "resourceType": "Observation",
        "id": id,
        "status": "final",
        "code": {"text": "probe"},
        "subject": {"reference": format!("Patient/{patient}")}
    })
}

fn ids(value: &Value) -> Vec<String> {
    value["entry"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["resource"]["id"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn everything_gathers_the_patient_and_its_compartment() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-e1"})).await;
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-e2"})).await;
    create(&app, observation("ob-e1", "pt-e1")).await;
    create(&app, observation("ob-e2", "pt-e2")).await;
    let reply = request(&app, "GET", "/Patient/pt-e1/$everything", &[]).await;
    assert_eq!(reply.status, StatusCode::OK);
    let value = json(&reply);
    assert_eq!(value["type"], "searchset");
    let gathered = ids(&value);
    assert!(gathered.contains(&"pt-e1".to_owned()), "{gathered:?}");
    assert!(gathered.contains(&"ob-e1".to_owned()), "{gathered:?}");
    assert!(!gathered.contains(&"ob-e2".to_owned()), "{gathered:?}");
    assert!(!gathered.contains(&"pt-e2".to_owned()), "{gathered:?}");
}

#[tokio::test]
async fn everything_answers_the_same_over_post() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-e3"})).await;
    let got = request(&app, "GET", "/Patient/pt-e3/$everything", &[]).await;
    let posted = request(&app, "POST", "/Patient/pt-e3/$everything", &[]).await;
    assert_eq!(posted.status, StatusCode::OK);
    assert_eq!(ids(&json(&got)), ids(&json(&posted)));
}

#[tokio::test]
async fn everything_narrows_by_type_and_time() {
    let app = stepping_service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-e4"})).await;
    create(&app, observation("ob-e4", "pt-e4")).await;
    let typed = request(&app, "GET", "/Patient/pt-e4/$everything?_type=Observation", &[]).await;
    assert_eq!(ids(&json(&typed)), vec!["ob-e4".to_owned()]);
    let since = request(
        &app,
        "GET",
        "/Patient/pt-e4/$everything?_since=2026-09-06T04:01:00Z",
        &[],
    )
    .await;
    assert_eq!(ids(&json(&since)), vec!["ob-e4".to_owned()]);
    let till = request(
        &app,
        "GET",
        "/Patient/pt-e4/$everything?_till=2026-09-06T04:00:30Z",
        &[],
    )
    .await;
    assert_eq!(ids(&json(&till)), vec!["pt-e4".to_owned()]);
}

#[tokio::test]
async fn everything_pages_through_search_continuation_tokens() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-e5"})).await;
    create(&app, observation("ob-e5", "pt-e5")).await;
    let first = request(&app, "GET", "/Patient/pt-e5/$everything?_count=1", &[]).await;
    let value = json(&first);
    assert_eq!(value["total"], 2);
    assert_eq!(ids(&value).len(), 1);
    let next = value["link"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["relation"] == "next")
        .map(|link| link["url"].as_str().unwrap().to_owned())
        .expect("a next link");
    assert!(next.contains("ct="), "{next}");
    let path = next.split_once("localhost").unwrap().1.to_owned();
    let second = request(&app, "GET", &path, &[]).await;
    assert_eq!(ids(&json(&second)).len(), 1);
    assert_ne!(ids(&json(&second)), ids(&value));
}

#[tokio::test]
async fn everything_refuses_an_unknown_patient_and_parameter() {
    let app = service();
    let missing = request(&app, "GET", "/Patient/nonesuch/$everything", &[]).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-e6"})).await;
    let unknown = request(&app, "GET", "/Patient/pt-e6/$everything?nonesuch=1", &[]).await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    let typed = request(&app, "GET", "/Patient/pt-e6/$everything?_type=Medication", &[]).await;
    assert_eq!(typed.status, StatusCode::BAD_REQUEST);
}

fn member(id: &str, system: &str, value: &str, birth: &str) -> serde_json::Value {
    serde_json::json!({
        "resourceType": "Patient",
        "id": id,
        "identifier": [{"system": system, "value": value}],
        "birthDate": birth
    })
}

fn match_body(patient: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "MemberPatient", "resource": patient},
            {"name": "CoverageToMatch", "resource": {"resourceType": "Coverage", "id": "cv-1"}}
        ]
    }))
    .unwrap()
}

#[tokio::test]
async fn member_match_returns_the_identifier_of_the_matched_member() {
    let app = service();
    create(&app, member("pt-m1", "urn:mrn", "42", "1980-04-01")).await;
    let asked = member("submitted", "urn:mrn", "42", "1980-04-01");
    let reply = request(&app, "POST", "/Patient/$member-match", &match_body(asked)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let value = json(&reply);
    assert_eq!(value["resourceType"], "Parameters");
    let identifier = value["parameter"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "MemberIdentifier")
        .cloned()
        .expect("a member identifier");
    assert_eq!(identifier["valueIdentifier"]["value"], "42");
    assert_eq!(identifier["valueIdentifier"]["system"], "urn:mrn");
    let matched = value["parameter"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "MemberPatient")
        .cloned()
        .expect("the matched patient");
    assert_eq!(matched["resource"]["id"], "pt-m1");
}

#[tokio::test]
async fn member_match_reports_no_match_explicitly() {
    let app = service();
    create(&app, member("pt-m2", "urn:mrn", "43", "1980-04-01")).await;
    let asked = member("submitted", "urn:mrn", "nonesuch", "1980-04-01");
    let reply = request(&app, "POST", "/Patient/$member-match", &match_body(asked)).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    let value = json(&reply);
    assert_eq!(value["resourceType"], "OperationOutcome");
    assert_eq!(value["issue"][0]["code"], "business-rule");
}

#[tokio::test]
async fn member_match_refuses_a_member_that_is_not_unique() {
    let app = service();
    create(&app, member("pt-m3", "urn:mrn", "44", "1980-04-01")).await;
    create(&app, member("pt-m4", "urn:mrn", "44", "1990-01-01")).await;
    let asked = serde_json::json!({
        "resourceType": "Patient",
        "identifier": [{"system": "urn:mrn", "value": "44"}]
    });
    let reply = request(&app, "POST", "/Patient/$member-match", &match_body(asked)).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn member_match_separates_members_by_birth_date() {
    let app = service();
    create(&app, member("pt-m5", "urn:mrn", "45", "1980-04-01")).await;
    create(&app, member("pt-m6", "urn:mrn", "45", "1990-01-01")).await;
    let asked = member("submitted", "urn:mrn", "45", "1990-01-01");
    let reply = request(&app, "POST", "/Patient/$member-match", &match_body(asked)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let value = json(&reply);
    let matched = value["parameter"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "MemberPatient")
        .cloned()
        .unwrap();
    assert_eq!(matched["resource"]["id"], "pt-m6");
}

#[tokio::test]
async fn member_match_refuses_a_request_without_a_member() {
    let app = service();
    let empty = serde_json::to_vec(&serde_json::json!({"resourceType": "Parameters"})).unwrap();
    let reply = request(&app, "POST", "/Patient/$member-match", &empty).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let unidentified = match_body(serde_json::json!({"resourceType": "Patient", "id": "x"}));
    let without = request(&app, "POST", "/Patient/$member-match", &unidentified).await;
    assert_eq!(without.status, StatusCode::BAD_REQUEST);
    let wrong = request(&app, "POST", "/Patient/$member-match", b"{}").await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
}

async fn request_with(
    app: &Service,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Reply {
    let mut builder = Request::builder().method(method).uri(uri).header("host", "localhost");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .router()
        .oneshot(builder.body(Body::from(body.to_vec())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    Reply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn modes(value: &Value) -> Vec<String> {
    value["entry"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["search"]["mode"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn includes_answers_with_the_related_resources_only() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-i1"})).await;
    create(&app, observation("ob-i1", "pt-i1")).await;
    let reply = request(
        &app,
        "GET",
        "/Observation/$includes?_id=ob-i1&_include=Observation:subject",
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let value = json(&reply);
    assert_eq!(value["type"], "searchset");
    assert_eq!(ids(&value), vec!["pt-i1".to_owned()]);
    assert_eq!(modes(&value), vec!["include".to_owned()]);
    assert_eq!(value["total"], 1);
}

#[tokio::test]
async fn includes_pages_through_a_continuation_token() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-i2"})).await;
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-i3"})).await;
    create(&app, observation("ob-i2", "pt-i2")).await;
    create(&app, observation("ob-i3", "pt-i3")).await;
    let first = request(
        &app,
        "GET",
        "/Observation/$includes?_include=Observation:subject&_count=1",
        &[],
    )
    .await;
    let value = json(&first);
    assert_eq!(value["total"], 2);
    assert_eq!(ids(&value).len(), 1);
    let next = value["link"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["relation"] == "next")
        .map(|link| link["url"].as_str().unwrap().to_owned())
        .expect("a next link");
    let path = next.split_once("localhost").unwrap().1.to_owned();
    let second = json(&request(&app, "GET", &path, &[]).await);
    assert_eq!(ids(&second).len(), 1);
    assert_ne!(ids(&second), ids(&value));
    assert!(second["link"]
        .as_array()
        .unwrap()
        .iter()
        .all(|link| link["relation"] != "next"));
}

#[tokio::test]
async fn includes_stays_inside_the_grant() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-i4"})).await;
    create(&app, observation("ob-i4", "pt-i4")).await;
    let reply = request_with(
        &app,
        "GET",
        "/Observation/$includes?_include=Observation:subject",
        &[("x-scope", "types=Observation")],
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(ids(&json(&reply)).is_empty());
}

#[tokio::test]
async fn includes_needs_an_include_and_rejects_the_unknown() {
    let app = service();
    let without = request(&app, "GET", "/Observation/$includes", &[]).await;
    assert_eq!(without.status, StatusCode::BAD_REQUEST);
    let unknown = request(
        &app,
        "GET",
        "/Observation/$includes?nonesuch=1&_include=Observation:subject",
        &[],
    )
    .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
}

fn document(id: &str, patient: &str, date: &str, code: &str) -> serde_json::Value {
    serde_json::json!({
        "resourceType": "DocumentReference",
        "id": id,
        "status": "current",
        "type": {"coding": [{"system": "urn:doc", "code": code}]},
        "subject": {"reference": format!("Patient/{patient}")},
        "date": format!("{date}T00:00:00Z"),
        "content": [{"attachment": {"url": "urn:doc:body"}}]
    })
}

fn docref_body(pairs: &[(&str, &str)]) -> Vec<u8> {
    let parameter: Vec<serde_json::Value> = pairs
        .iter()
        .map(|(name, value)| serde_json::json!({"name": name, "valueString": value}))
        .collect();
    serde_json::to_vec(&serde_json::json!({
        "resourceType": "Parameters",
        "parameter": parameter
    }))
    .unwrap()
}

#[tokio::test]
async fn docref_answers_the_documents_of_one_patient() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-d1"})).await;
    create(&app, document("dr-d1", "pt-d1", "2026-03-01", "note")).await;
    create(&app, document("dr-d2", "pt-d2", "2026-03-01", "note")).await;
    let reply = request(&app, "GET", "/DocumentReference/$docref?patient=pt-d1", &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let value = json(&reply);
    assert_eq!(value["type"], "searchset");
    assert_eq!(ids(&value), vec!["dr-d1".to_owned()]);
}

#[tokio::test]
async fn docref_answers_the_same_over_get_and_post() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-d3"})).await;
    create(&app, document("dr-d3", "pt-d3", "2026-03-01", "note")).await;
    create(&app, document("dr-d4", "pt-d3", "2026-06-01", "summary")).await;
    let uri = "/DocumentReference/$docref?patient=pt-d3&start=2026-05-01&type=urn:doc|summary";
    let got = json(&request(&app, "GET", uri, &[]).await);
    let posted = request(
        &app,
        "POST",
        "/DocumentReference/$docref",
        &docref_body(&[
            ("patient", "pt-d3"),
            ("start", "2026-05-01"),
            ("type", "urn:doc|summary"),
        ]),
    )
    .await;
    assert_eq!(posted.status, StatusCode::OK, "{}", posted.body);
    let posted = json(&posted);
    assert_eq!(ids(&got), vec!["dr-d4".to_owned()]);
    assert_eq!(ids(&got), ids(&posted));
    assert_eq!(got["total"], posted["total"]);
    assert_eq!(got["link"], posted["link"]);
}

#[tokio::test]
async fn docref_narrows_by_the_end_of_the_window() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-d5"})).await;
    create(&app, document("dr-d5", "pt-d5", "2026-03-01", "note")).await;
    create(&app, document("dr-d6", "pt-d5", "2026-06-01", "note")).await;
    let uri = "/DocumentReference/$docref?patient=Patient/pt-d5&end=2026-04-01";
    assert_eq!(ids(&json(&request(&app, "GET", uri, &[]).await)), vec!["dr-d5".to_owned()]);
}

#[tokio::test]
async fn docref_pages_like_a_search() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-d7"})).await;
    create(&app, document("dr-d7", "pt-d7", "2026-03-01", "note")).await;
    create(&app, document("dr-d8", "pt-d7", "2026-06-01", "note")).await;
    let first = json(&request(&app, "GET", "/DocumentReference/$docref?patient=pt-d7&_count=1", &[]).await);
    assert_eq!(first["total"], 2);
    let next = first["link"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["relation"] == "next")
        .map(|link| link["url"].as_str().unwrap().to_owned())
        .expect("a next link");
    let path = next.split_once("localhost").unwrap().1.to_owned();
    let second = json(&request(&app, "GET", &path, &[]).await);
    assert_eq!(ids(&second).len(), 1);
    assert_ne!(ids(&second), ids(&first));
}

#[tokio::test]
async fn docref_refuses_a_request_without_a_patient_or_beyond_the_server() {
    let app = service();
    let without = request(&app, "GET", "/DocumentReference/$docref", &[]).await;
    assert_eq!(without.status, StatusCode::BAD_REQUEST);
    let demanded = request(
        &app,
        "GET",
        "/DocumentReference/$docref?patient=pt-d9&on-demand=true",
        &[],
    )
    .await;
    assert_eq!(demanded.status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&demanded)["issue"][0]["code"], "not-supported");
    let unknown = request(
        &app,
        "GET",
        "/DocumentReference/$docref?patient=pt-d9&nonesuch=1",
        &[],
    )
    .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
}

fn code_system() -> serde_json::Value {
    serde_json::json!({
        "resourceType": "CodeSystem",
        "id": "cs-1",
        "url": "urn:cs",
        "version": "1.0",
        "status": "active",
        "content": "complete",
        "concept": [{
            "code": "top",
            "display": "Top",
            "designation": [{"language": "nl", "value": "Boven"}],
            "concept": [
                {"code": "mid", "display": "Middle", "concept": [{"code": "leaf", "display": "Leaf"}]},
                {"code": "old", "display": "Old", "property": [{"code": "status", "valueCode": "retired"}]}
            ]
        }]
    })
}

fn value_set() -> serde_json::Value {
    serde_json::json!({
        "resourceType": "ValueSet",
        "id": "vs-1",
        "url": "urn:vs",
        "version": "2.0",
        "status": "active",
        "compose": {"include": [{"system": "urn:cs"}]}
    })
}

fn codes(value: &Value) -> Vec<String> {
    value["expansion"]["contains"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|held| held["code"].as_str().map(str::to_owned))
        .collect()
}

async fn terminology_service() -> Service {
    let app = service();
    create(&app, code_system()).await;
    create(&app, value_set()).await;
    app
}

#[tokio::test]
async fn expand_answers_the_codes_of_a_value_set() {
    let app = terminology_service().await;
    let reply = request(&app, "GET", "/ValueSet/$expand?url=urn:vs&excludeNested=true", &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let value = json(&reply);
    assert_eq!(value["resourceType"], "ValueSet");
    assert_eq!(value["expansion"]["total"], 4);
    assert_eq!(
        codes(&value),
        vec!["top".to_owned(), "mid".to_owned(), "leaf".to_owned(), "old".to_owned()]
    );
}

#[tokio::test]
async fn expand_narrows_by_filter_paging_and_activity() {
    let app = terminology_service().await;
    let filtered = json(
        &request(&app, "GET", "/ValueSet/$expand?url=urn:vs&filter=lea&excludeNested=true", &[]).await,
    );
    assert_eq!(codes(&filtered), vec!["leaf".to_owned()]);
    let paged = json(
        &request(
            &app,
            "GET",
            "/ValueSet/$expand?url=urn:vs&excludeNested=true&count=2&offset=1",
            &[],
        )
        .await,
    );
    assert_eq!(paged["expansion"]["offset"], 1);
    assert_eq!(codes(&paged).len(), 2);
    let active = json(
        &request(
            &app,
            "GET",
            "/ValueSet/$expand?url=urn:vs&excludeNested=true&activeOnly=true",
            &[],
        )
        .await,
    );
    assert!(!codes(&active).contains(&"old".to_owned()));
}

#[tokio::test]
async fn expand_honours_language_designations_and_nesting() {
    let app = terminology_service().await;
    let reply = request(
        &app,
        "GET",
        "/ValueSet/$expand?url=urn:vs&displayLanguage=nl&includeDesignations=true",
        &[],
    )
    .await;
    let value = json(&reply);
    assert_eq!(value["expansion"]["contains"][0]["display"], "Boven");
    assert_eq!(value["expansion"]["contains"][0]["designation"][0]["value"], "Boven");
    assert_eq!(value["expansion"]["contains"][0]["contains"][0]["code"], "mid");
}

#[tokio::test]
async fn expand_answers_the_same_over_a_parameters_body() {
    let app = terminology_service().await;
    let got = json(&request(&app, "GET", "/ValueSet/$expand?url=urn:vs&excludeNested=true", &[]).await);
    let body = serde_json::to_vec(&serde_json::json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "url", "valueUri": "urn:vs"},
            {"name": "excludeNested", "valueBoolean": true}
        ]
    }))
    .unwrap();
    let posted = request(&app, "POST", "/ValueSet/$expand", &body).await;
    assert_eq!(posted.status, StatusCode::OK, "{}", posted.body);
    assert_eq!(codes(&json(&posted)), codes(&got));
}

#[tokio::test]
async fn expand_reports_failures_as_outcomes() {
    let app = terminology_service().await;
    let unknown = request(&app, "GET", "/ValueSet/$expand?url=urn:nonesuch", &[]).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(json(&unknown)["resourceType"], "OperationOutcome");
    let without = request(&app, "GET", "/ValueSet/$expand", &[]).await;
    assert_eq!(without.status, StatusCode::BAD_REQUEST);
    let count = request(&app, "GET", "/ValueSet/$expand?url=urn:vs&count=many", &[]).await;
    assert_eq!(count.status, StatusCode::BAD_REQUEST);
    let version = request(&app, "GET", "/ValueSet/$expand?url=urn:vs&valueSetVersion=9.9", &[]).await;
    assert_eq!(version.status, StatusCode::NOT_FOUND);
    let pinned = request(
        &app,
        "GET",
        "/ValueSet/$expand?url=urn:vs&system-version=urn:cs|9.9",
        &[],
    )
    .await;
    assert_eq!(pinned.status, StatusCode::BAD_REQUEST);
    let date = request(&app, "GET", "/ValueSet/$expand?url=urn:vs&nonesuch=1", &[]).await;
    assert_eq!(date.status, StatusCode::BAD_REQUEST);
}

fn coded(id: &str, system: Option<&str>, code: &str) -> serde_json::Value {
    let coding = match system {
        Some(system) => serde_json::json!({"system": system, "code": code}),
        None => serde_json::json!({"code": code}),
    };
    serde_json::json!({
        "resourceType": "Observation",
        "id": id,
        "status": "final",
        "code": {"coding": [coding]}
    })
}

async fn found(app: &Service, uri: &str) -> Vec<String> {
    let reply = request(app, "GET", uri, &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let mut held = ids(&json(&reply));
    held.sort();
    held
}

#[tokio::test]
async fn subsumption_modifiers_resolve_through_the_terminology() {
    let app = service();
    create(&app, code_system()).await;
    create(&app, coded("ob-s1", Some("urn:cs"), "leaf")).await;
    create(&app, coded("ob-s2", Some("urn:cs"), "top")).await;
    assert_eq!(
        found(&app, "/Observation?code:below=urn:cs%7Cmid").await,
        vec!["ob-s1".to_owned()]
    );
    assert_eq!(
        found(&app, "/Observation?code:above=urn:cs%7Cmid").await,
        vec!["ob-s2".to_owned()]
    );
    assert!(found(&app, "/Observation?code:below=urn:cs%7Cleaf")
        .await
        .contains(&"ob-s1".to_owned()));
}

#[tokio::test]
async fn a_code_no_system_defines_is_compared_as_it_stands() {
    let app = service();
    create(&app, code_system()).await;
    create(&app, coded("ob-s3", None, "a.b.c")).await;
    assert_eq!(
        found(&app, "/Observation?code:below=a.b").await,
        vec!["ob-s3".to_owned()]
    );
    assert!(found(&app, "/Observation?code:below=a.d").await.is_empty());
    assert_eq!(
        found(&app, "/Observation?code:above=a.b.c.d").await,
        vec!["ob-s3".to_owned()]
    );
}

#[tokio::test]
async fn validate_names_the_rule_that_failed() {
    let app = service();
    let body = br#"{"resourceType":"Observation","id":"ob-r1","status":"draft","gender":"x","text":{"status":"invented","div":"plain"}}"#;
    let reply = request(&app, "POST", "/Observation/$validate?profile=http://x/one", body).await;
    assert_eq!(reply.status, StatusCode::OK);
    let named: Vec<String> = json(&reply)["issue"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|issue| {
            format!(
                "{} {}",
                issue["diagnostics"].as_str().unwrap_or_default(),
                issue["expression"][0].as_str().unwrap_or_default()
            )
        })
        .collect();
    let text = named.join(" | ");
    for rule in ["structure", "cardinality", "binding", "profile", "narrative"] {
        assert!(text.contains(rule), "{rule} was not named in {text}");
    }
    assert!(text.contains("Observation.code"), "{text}");
    assert!(text.contains("Observation.gender"), "{text}");
}
