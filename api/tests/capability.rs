use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::app::{served, Verb};
use fhir_api::{Dependency, Service};
use fhir_core::{FhirInstant, FhirVersion};
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::Arc;
use tower::ServiceExt;

fn service(version: FhirVersion) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Ok(())),
    }];
    Service::new(Arc::new(store), version, dependencies)
}

async fn reply(app: &Service, method: &str, uri: &str, body: &[u8]) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json")
        .body(Body::from(body.to_vec()))
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn statement(app: &Service) -> Value {
    let (status, body) = reply(app, "GET", "/metadata", b"").await;
    assert_eq!(status, StatusCode::OK);
    serde_json::from_str(&body).expect("the statement must be json")
}

fn concrete(path: &str) -> String {
    path.replace("{type}", "Patient")
        .replace("{id}", "pt-1")
        .replace("{vid}", "1")
        .replace("{target}", "Observation")
        .replace("{*name}", "output.ndjson")
        .replace("{code}", "validate")
}

fn codes(entries: Option<&Value>) -> BTreeSet<String> {
    entries
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("code").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn names(entries: Option<&Value>) -> BTreeSet<String> {
    entries
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn resource<'a>(statement: &'a Value, kind: &str) -> &'a Value {
    statement["rest"][0]["resource"]
        .as_array()
        .expect("the statement must list resources")
        .iter()
        .find(|entry| entry["type"] == kind)
        .unwrap_or_else(|| panic!("{kind} must be listed"))
}

#[tokio::test]
async fn the_declared_methods_are_the_methods_the_router_answers() {
    let app = service(FhirVersion::R4);
    for route in served() {
        let uri = concrete(route.path);
        for verb in Verb::ALL {
            let (status, _) = reply(&app, verb.as_str(), &uri, b"").await;
            let declared = route.methods.contains(&verb);
            assert_eq!(
                declared,
                status != StatusCode::METHOD_NOT_ALLOWED,
                "{} {uri} declared {declared}, answered {status}",
                verb.as_str()
            );
        }
    }
}

#[tokio::test]
async fn the_statement_advertises_the_operations_the_routes_serve() {
    let app = service(FhirVersion::R4);
    let statement = statement(&app).await;
    let routed: BTreeSet<String> = served()
        .iter()
        .filter_map(|route| route.path.rsplit('/').next())
        .filter(|last| last.starts_with('$'))
        .map(|last| last.trim_start_matches('$').to_owned())
        .collect();
    let mut advertised = names(statement["rest"][0].get("operation"));
    for entry in statement["rest"][0]["resource"].as_array().unwrap() {
        advertised.extend(names(entry.get("operation")));
    }
    assert_eq!(routed, advertised);
}

#[tokio::test]
async fn the_statement_advertises_the_interactions_the_routes_serve() {
    let app = service(FhirVersion::R4);
    let statement = statement(&app).await;
    assert_eq!(
        codes(statement["rest"][0].get("interaction")),
        BTreeSet::from([
            "batch".to_owned(),
            "history-system".to_owned(),
            "search-system".to_owned(),
            "transaction".to_owned(),
        ])
    );
    let patient = resource(&statement, "Patient");
    assert_eq!(
        codes(patient.get("interaction")),
        BTreeSet::from([
            "create".to_owned(),
            "delete".to_owned(),
            "history-instance".to_owned(),
            "history-type".to_owned(),
            "patch".to_owned(),
            "read".to_owned(),
            "search-type".to_owned(),
            "update".to_owned(),
            "vread".to_owned(),
        ])
    );
    assert_eq!(patient["conditionalCreate"], Value::Bool(true));
    assert_eq!(patient["conditionalUpdate"], Value::Bool(true));
    assert_eq!(patient["conditionalDelete"], Value::String("single".into()));
}

#[tokio::test]
async fn every_advertised_interaction_reaches_a_route() {
    let app = service(FhirVersion::R4);
    let statement = statement(&app).await;
    let patient = resource(&statement, "Patient");
    let probes = [
        ("read", "GET", "/Patient/pt-1"),
        ("vread", "GET", "/Patient/pt-1/_history/1"),
        ("update", "PUT", "/Patient/pt-1"),
        ("patch", "PATCH", "/Patient/pt-1"),
        ("delete", "DELETE", "/Patient/pt-1"),
        ("create", "POST", "/Patient"),
        ("search-type", "GET", "/Patient"),
        ("history-type", "GET", "/Patient/_history"),
        ("history-instance", "GET", "/Patient/pt-1/_history"),
    ];
    for (code, method, uri) in probes {
        assert!(codes(patient.get("interaction")).contains(code), "{code}");
        let (status, _) = reply(&app, method, uri, b"{}").await;
        assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
    }
}

#[tokio::test]
async fn the_statement_lists_the_search_parameters_the_registry_holds() {
    let app = service(FhirVersion::R4);
    let statement = statement(&app).await;
    let patient = resource(&statement, "Patient");
    let listed = names(patient.get("searchParam"));
    let held: BTreeSet<String> = app
        .registry()
        .for_type("Patient".parse().unwrap())
        .iter()
        .map(|def| def.name.clone())
        .collect();
    assert_eq!(listed, held);
    assert!(listed.contains("birthdate"));
    assert!(listed.contains("_lastUpdated"));
}

#[tokio::test]
async fn a_registered_parameter_reaches_the_statement() {
    let app = service(FhirVersion::R4);
    let body = serde_json::to_vec(&serde_json::json!({
        "resourceType": "SearchParameter",
        "id": "patient-nickname",
        "url": "http://example.org/SearchParameter/patient-nickname",
        "name": "nickname",
        "status": "active",
        "code": "nickname",
        "base": ["Patient"],
        "type": "string",
        "expression": "Patient.name.text"
    }))
    .unwrap();
    let (status, _) = reply(&app, "POST", "/SearchParameter", &body).await;
    assert_eq!(status, StatusCode::CREATED);
    let statement = statement(&app).await;
    let patient = resource(&statement, "Patient");
    assert!(names(patient.get("searchParam")).contains("nickname"));
}

#[tokio::test]
async fn the_statement_reports_the_running_version_and_build() {
    for version in [FhirVersion::Stu3, FhirVersion::R4, FhirVersion::R4b, FhirVersion::R5] {
        let app = service(version);
        let statement = statement(&app).await;
        assert_eq!(statement["resourceType"], "CapabilityStatement");
        assert_eq!(statement["status"], "active");
        assert_eq!(statement["fhirVersion"], version.release());
        assert_eq!(statement["software"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(statement["date"].as_str().is_some());
    }
}

fn advertised(statement: &Value) -> BTreeSet<String> {
    let mut found = names(statement["rest"][0].get("operation"));
    for entry in statement["rest"][0]["resource"].as_array().unwrap() {
        found.extend(names(entry.get("operation")));
    }
    found
}

#[tokio::test]
async fn every_advertised_operation_has_a_definition() {
    let app = service(FhirVersion::R4);
    let statement = statement(&app).await;
    for code in advertised(&statement) {
        let (status, body) = reply(&app, "GET", &format!("/OperationDefinition/{code}"), b"").await;
        assert_eq!(status, StatusCode::OK, "{code}");
        let value: Value = serde_json::from_str(&body).expect("the definition must be json");
        assert_eq!(value["resourceType"], "OperationDefinition");
        assert_eq!(value["code"], code.as_str());
        assert_eq!(value["status"], "active");
    }
}

#[tokio::test]
async fn the_definitions_listed_are_the_operations_routed() {
    let app = service(FhirVersion::R4);
    let (status, body) = reply(&app, "GET", "/OperationDefinition", b"").await;
    assert_eq!(status, StatusCode::OK);
    let bundle: Value = serde_json::from_str(&body).expect("the bundle must be json");
    let listed: BTreeSet<String> = bundle["entry"]
        .as_array()
        .expect("the bundle must carry entries")
        .iter()
        .map(|entry| entry["resource"]["code"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(listed, advertised(&statement(&app).await));
}

#[tokio::test]
async fn an_unknown_operation_has_no_definition() {
    let app = service(FhirVersion::R4);
    let (status, body) = reply(&app, "GET", "/OperationDefinition/nonesuch", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("OperationOutcome"));
}

#[tokio::test]
async fn a_definition_declares_the_levels_and_types_its_routes_serve() {
    let app = service(FhirVersion::R4);
    async fn read(app: &Service, code: &str) -> Value {
        let (_, body) = reply(app, "GET", &format!("/OperationDefinition/{code}"), b"").await;
        serde_json::from_str::<Value>(&body).expect("the definition must be json")
    }
    let validate = read(&app, "validate").await;
    assert_eq!(validate["system"], Value::Bool(false));
    assert_eq!(validate["type"], Value::Bool(true));
    assert_eq!(validate["instance"], Value::Bool(true));
    assert!(validate["resource"].as_array().unwrap().is_empty());
    let export = read(&app, "export").await;
    assert_eq!(export["system"], Value::Bool(true));
    let scoped: BTreeSet<String> = export["resource"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(scoped, BTreeSet::from(["Group".to_owned(), "Patient".to_owned()]));
    let everything = read(&app, "everything").await;
    assert_eq!(everything["instance"], Value::Bool(true));
    assert_eq!(everything["resource"][0], "Patient");
}

#[tokio::test]
async fn a_definition_declares_the_parameters_the_handler_accepts() {
    let app = service(FhirVersion::R4);
    let (_, body) = reply(&app, "GET", "/OperationDefinition/export", b"").await;
    let export: Value = serde_json::from_str(&body).unwrap();
    let declared = names(export.get("parameter"));
    for name in ["_type", "_since", "_till", "_outputFormat", "_typeFilter"] {
        assert!(declared.contains(name), "{name}");
    }
    let (_, body) = reply(&app, "GET", "/OperationDefinition/expand", b"").await;
    let expand: Value = serde_json::from_str(&body).unwrap();
    assert!(names(expand.get("parameter")).contains("filter"));
}

#[tokio::test]
async fn a_required_parameter_is_one_the_handler_insists_on() {
    let app = service(FhirVersion::R4);
    let (_, body) = reply(&app, "GET", "/OperationDefinition/convert-data", b"").await;
    let definition: Value = serde_json::from_str(&body).unwrap();
    let required: Vec<String> = definition["parameter"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["use"] == "in" && item["min"] == 1)
        .map(|item| item["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(!required.is_empty());
    let complete = serde_json::json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "inputData", "valueString": "MSH|^~\\&|"},
            {"name": "inputDataType", "valueString": "hl7v2"},
            {"name": "templateCollectionReference", "valueString": "builtin"},
            {"name": "rootTemplate", "valueString": "ADT_A01"}
        ]
    });
    for name in required {
        let mut body = complete.clone();
        let kept: Vec<Value> = body["parameter"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["name"] != name.as_str())
            .cloned()
            .collect();
        body["parameter"] = Value::Array(kept);
        let (status, _) = reply(
            &app,
            "POST",
            "/$convert-data",
            &serde_json::to_vec(&body).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name} must be required");
    }
}
