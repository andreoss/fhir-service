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
        check: Arc::new(|| Box::pin(async { Ok(()) })),
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
async fn every_advertised_interaction_answers_the_status_the_specification_names() {
    let app = service(FhirVersion::R4);
    let statement = statement(&app).await;
    let patient = resource(&statement, "Patient");
    let body = br#"{"resourceType":"Patient","id":"pt-1","active":true}"#;
    let patch = br#"[{"op":"replace","path":"/active","value":false}]"#;
    let probes: [(&str, &str, &str, &[u8], StatusCode); 9] = [
        ("create", "POST", "/Patient", body, StatusCode::CREATED),
        ("read", "GET", "/Patient/pt-1", b"", StatusCode::OK),
        (
            "vread",
            "GET",
            "/Patient/pt-1/_history/1",
            b"",
            StatusCode::OK,
        ),
        ("update", "PUT", "/Patient/pt-1", body, StatusCode::OK),
        ("patch", "PATCH", "/Patient/pt-1", patch, StatusCode::OK),
        ("search-type", "GET", "/Patient", b"", StatusCode::OK),
        (
            "history-type",
            "GET",
            "/Patient/_history",
            b"",
            StatusCode::OK,
        ),
        (
            "history-instance",
            "GET",
            "/Patient/pt-1/_history",
            b"",
            StatusCode::OK,
        ),
        (
            "delete",
            "DELETE",
            "/Patient/pt-1",
            b"",
            StatusCode::NO_CONTENT,
        ),
    ];
    for (code, method, uri, sent, expected) in probes {
        assert!(
            codes(patient.get("interaction")).contains(code),
            "{code} is not advertised"
        );
        let (status, answered) = reply(&app, method, uri, sent).await;
        assert_eq!(
            status, expected,
            "{code}: {method} {uri} answered {answered}"
        );
    }
}

#[tokio::test]
async fn what_the_statement_says_about_update_create_is_what_an_unknown_id_gets() {
    for version in [
        FhirVersion::Stu3,
        FhirVersion::R4,
        FhirVersion::R4b,
        FhirVersion::R5,
    ] {
        let app = service(version);
        let statement = statement(&app).await;
        let advertised = resource(&statement, "Patient")["updateCreate"]
            .as_bool()
            .expect("the statement says whether an update may create");
        let (status, body) = reply(
            &app,
            "PUT",
            "/Patient/pt-unknown",
            br#"{"resourceType":"Patient","id":"pt-unknown","active":true}"#,
        )
        .await;
        match advertised {
            true => assert_eq!(status, StatusCode::CREATED, "{version}: {body}"),
            false => assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{version}: {body}"),
        }
    }
}

#[tokio::test]
async fn the_statement_names_the_instance_it_was_read_from() {
    for version in [
        FhirVersion::Stu3,
        FhirVersion::R4,
        FhirVersion::R4b,
        FhirVersion::R5,
    ] {
        let statement = statement(&service(version)).await;
        assert_eq!(statement["resourceType"], "CapabilityStatement");
        assert_eq!(statement["kind"], "instance", "{version}");
        assert_eq!(statement["status"], "active", "{version}");
        assert_eq!(statement["fhirVersion"], version.release(), "{version}");
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
        "description": "a parameter",
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
    for version in [
        FhirVersion::Stu3,
        FhirVersion::R4,
        FhirVersion::R4b,
        FhirVersion::R5,
    ] {
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
    assert_eq!(
        scoped,
        BTreeSet::from(["Group".to_owned(), "Patient".to_owned()])
    );
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

fn authorized() -> Service {
    service(FhirVersion::R4).with_authorization(
        fhir_api::Authorization::new(
            "https://issuer.example.org",
            "https://issuer.example.org/authorize",
            "https://issuer.example.org/token",
        )
        .with_introspection("https://issuer.example.org/introspect")
        .with_scopes(vec![
            "system/*.read".to_owned(),
            "system/*.write".to_owned(),
        ])
        .with_capabilities(vec!["client-confidential-symmetric".to_owned()]),
    )
}

#[tokio::test]
async fn an_unsecured_instance_publishes_no_discovery_document() {
    let app = service(FhirVersion::R4);
    let (status, body) = reply(&app, "GET", "/.well-known/smart-configuration", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("OperationOutcome"));
    let statement = statement(&app).await;
    assert!(statement["rest"][0].get("security").is_none());
}

#[tokio::test]
async fn discovery_reports_the_authorization_the_instance_runs_under() {
    let app = authorized();
    let (status, body) = reply(&app, "GET", "/.well-known/smart-configuration", b"").await;
    assert_eq!(status, StatusCode::OK);
    let document: Value = serde_json::from_str(&body).expect("the document must be json");
    assert_eq!(document["issuer"], "https://issuer.example.org");
    assert_eq!(
        document["authorization_endpoint"],
        "https://issuer.example.org/authorize"
    );
    assert_eq!(
        document["token_endpoint"],
        "https://issuer.example.org/token"
    );
    assert_eq!(
        document["introspection_endpoint"],
        "https://issuer.example.org/introspect"
    );
    assert_eq!(document["scopes_supported"][0], "system/*.read");
    assert_eq!(document["capabilities"][0], "client-confidential-symmetric");
}

fn listed(document: &Value, name: &str) -> Vec<String> {
    document[name]
        .as_array()
        .unwrap_or_else(|| panic!("{name} must be published as a list"))
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn discovery_publishes_the_fields_a_launch_needs_to_choose_a_flow() {
    let app = authorized();
    let (status, body) = reply(&app, "GET", "/.well-known/smart-configuration", b"").await;
    assert_eq!(status, StatusCode::OK);
    let document: Value = serde_json::from_str(&body).expect("the document must be json");
    assert!(
        listed(&document, "grant_types_supported").contains(&"authorization_code".to_owned()),
        "{body}"
    );
    assert!(
        listed(&document, "response_types_supported").contains(&"code".to_owned()),
        "{body}"
    );
    assert!(
        listed(&document, "code_challenge_methods_supported").contains(&"S256".to_owned()),
        "a launch that cannot bind its code to a verifier is open to interception: {body}"
    );
    assert!(
        !listed(&document, "code_challenge_methods_supported").contains(&"plain".to_owned()),
        "{body}"
    );
    assert!(!listed(&document, "scopes_supported").is_empty(), "{body}");
    assert!(!listed(&document, "capabilities").is_empty(), "{body}");
}

#[tokio::test]
async fn the_statement_carries_the_addresses_discovery_publishes() {
    let app = authorized();
    let (_, body) = reply(&app, "GET", "/.well-known/smart-configuration", b"").await;
    let document: Value = serde_json::from_str(&body).unwrap();
    let statement = statement(&app).await;
    let security = &statement["rest"][0]["security"];
    assert_eq!(security["service"][0]["coding"][0]["code"], "SMART-on-FHIR");
    let uris = security["extension"][0]["extension"].as_array().unwrap();
    let held = |name: &str| {
        uris.iter()
            .find(|item| item["url"] == name)
            .map(|item| item["valueUri"].clone())
            .unwrap_or(Value::Null)
    };
    assert_eq!(held("authorize"), document["authorization_endpoint"]);
    assert_eq!(held("token"), document["token_endpoint"]);
    assert_eq!(held("introspect"), document["introspection_endpoint"]);
}

async fn observation_parameters(version: FhirVersion) -> BTreeSet<String> {
    let app = service(version);
    let statement = statement(&app).await;
    names(resource(&statement, "Observation").get("searchParam"))
}

#[tokio::test]
async fn the_statement_lists_the_parameters_of_the_running_version() {
    let old = observation_parameters(FhirVersion::Stu3).await;
    let current = observation_parameters(FhirVersion::R4).await;
    assert!(old.contains("context"), "{old:?}");
    assert!(old.contains("encounter"), "{old:?}");
    assert!(current.contains("encounter"));
    assert!(!current.contains("context"));
    for version in [FhirVersion::R4b, FhirVersion::R5] {
        assert_eq!(observation_parameters(version).await, current);
    }
}

#[tokio::test]
async fn a_parameter_a_version_does_not_publish_is_refused_by_it() {
    let old = service(FhirVersion::Stu3);
    for uri in [
        "/Observation?context=Encounter/enc-1",
        "/Observation?encounter=Encounter/enc-1",
        "/DocumentReference?authenticator=Practitioner/pr-1",
    ] {
        let (status, body) = reply(&old, "GET", uri, b"").await;
        assert_eq!(status, StatusCode::OK, "{uri} gave {body}");
    }
    let (status, _) = reply(
        &old,
        "GET",
        "/DocumentReference?attester=Practitioner/pr-1",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let current = service(FhirVersion::R4);
    let (status, _) = reply(
        &current,
        "GET",
        "/Observation?encounter=Encounter/enc-1",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = reply(&current, "GET", "/Observation?context=Encounter/enc-1", b"").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let newest = service(FhirVersion::R5);
    let (status, _) = reply(
        &newest,
        "GET",
        "/DocumentReference?attester=Practitioner/pr-1",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = reply(
        &newest,
        "GET",
        "/DocumentReference?authenticator=Practitioner/pr-1",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn compartment_definitions_carry_the_references_the_version_defines() {
    let params = |version| async move {
        let app = service(version);
        let (status, body) = reply(&app, "GET", "/CompartmentDefinition/Encounter", b"").await;
        assert_eq!(status, StatusCode::OK);
        let value: Value = serde_json::from_str(&body).expect("the definition must be json");
        value["resource"]
            .as_array()
            .expect("members must be listed")
            .iter()
            .find(|item| item["code"] == "Observation")
            .expect("Observation must belong")
            .clone()
    };
    let old = params(FhirVersion::Stu3).await;
    let current = params(FhirVersion::R4).await;
    assert_eq!(old["param"], serde_json::json!(["encounter"]));
    assert_eq!(current["param"], serde_json::json!(["encounter"]));
}

#[tokio::test]
async fn the_status_report_names_the_unsupported_parameters_of_the_version() {
    let unsupported = |version| async move {
        let app = service(version);
        let (status, body) = reply(&app, "GET", "/SearchParameter/$status", b"").await;
        assert_eq!(status, StatusCode::OK);
        let value: Value = serde_json::from_str(&body).expect("the report must be json");
        value["parameter"]
            .as_array()
            .expect("the report must carry parameters")
            .iter()
            .filter(|item| item["name"] == "unsupported")
            .map(|item| item["valueCode"].as_str().unwrap().to_owned())
            .collect::<BTreeSet<String>>()
    };
    let old = unsupported(FhirVersion::Stu3).await;
    let current = unsupported(FhirVersion::R4).await;
    assert!(old.contains("_filter"));
    assert!(current.contains("_filter"));
    assert!(old.contains("_content") && current.contains("_content"));
    assert!(!old.contains("_text") && !current.contains("_text"));
    for version in [FhirVersion::Stu3, FhirVersion::R4] {
        let app = service(version);
        let (status, _) = reply(&app, "GET", "/Patient?_filter=name%20eq%20a", b"").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{version:?}");
        let (status, body) = reply(&app, "GET", "/Patient?_text=fever", b"").await;
        assert_eq!(status, StatusCode::OK, "{version:?} {body}");
    }
}

#[tokio::test]
async fn versions_reports_the_running_build() {
    for version in [
        FhirVersion::Stu3,
        FhirVersion::R4,
        FhirVersion::R4b,
        FhirVersion::R5,
    ] {
        let app = service(version);
        for method in ["GET", "POST"] {
            let (status, body) = reply(&app, method, "/$versions", b"").await;
            assert_eq!(status, StatusCode::OK, "{method} {body}");
            let value: Value = serde_json::from_str(&body).expect("the report must be json");
            assert_eq!(value["resourceType"], "Parameters");
            let listed: BTreeSet<String> = value["parameter"]
                .as_array()
                .expect("versions must be listed")
                .iter()
                .filter(|item| item["name"] == "version")
                .map(|item| item["valueCode"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(
                listed,
                BTreeSet::from([
                    "3.0.2".to_owned(),
                    "4.0.1".to_owned(),
                    "4.3.0".to_owned(),
                    "5.0.0".to_owned(),
                ])
            );
            let named = |name: &str| {
                value["parameter"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["name"] == name)
                    .map(|item| item["valueCode"].clone())
                    .unwrap_or(Value::Null)
            };
            assert_eq!(
                named("default"),
                Value::String(version.release().to_owned())
            );
            assert_eq!(
                named("build"),
                Value::String(env!("CARGO_PKG_VERSION").to_owned())
            );
        }
    }
}

#[tokio::test]
async fn the_build_the_statement_reports_is_the_build_versions_reports() {
    let app = service(FhirVersion::R4);
    let (_, body) = reply(&app, "GET", "/$versions", b"").await;
    let reported: Value = serde_json::from_str(&body).unwrap();
    let build = reported["parameter"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "build")
        .unwrap()["valueCode"]
        .clone();
    assert_eq!(statement(&app).await["software"]["version"], build);
}

#[tokio::test]
async fn a_statement_lists_only_the_types_its_version_defines() {
    for version in FhirVersion::ALL {
        let app = service(version);
        let statement = statement(&app).await;
        let listed: Vec<String> = statement["rest"][0]["resource"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|entry| entry["type"].as_str().unwrap_or_default().to_owned())
            .collect();
        let model = fhir_core::Model::of(version);
        assert!(!listed.is_empty(), "{version}");
        assert!(
            listed.iter().all(|name| model.has_resource(name)),
            "{version} listed a type it does not define"
        );
        assert_eq!(
            listed.contains(&"Citation".to_owned()),
            matches!(version, FhirVersion::R4b | FhirVersion::R5),
            "{version}"
        );
    }
}

#[tokio::test]
async fn the_statement_names_the_software_it_describes() {
    for version in [
        FhirVersion::Stu3,
        FhirVersion::R4,
        FhirVersion::R4b,
        FhirVersion::R5,
    ] {
        let held = statement(&service(version)).await;
        assert!(
            held["software"]["name"]
                .as_str()
                .is_some_and(|name| !name.is_empty()),
            "{version} names no software"
        );
        assert!(held["software"]["version"].as_str().is_some());
    }
}

#[tokio::test]
async fn a_statement_carries_only_the_elements_its_version_defines() {
    for version in [FhirVersion::Stu3, FhirVersion::R4, FhirVersion::R4b] {
        let held = statement(&service(version)).await;
        let first = &held["rest"][0]["resource"][0];
        assert!(
            first["conditionalPatch"].is_null(),
            "{version} claims a later element"
        );
    }
    let held = statement(&service(FhirVersion::R5)).await;
    assert!(held["rest"][0]["resource"][0]["conditionalPatch"].is_boolean());
}

#[tokio::test]
async fn the_earliest_version_shapes_its_statement_as_that_version_defines_it() {
    let held = statement(&service(FhirVersion::Stu3)).await;
    assert_eq!(held["acceptUnknown"], "no");
    assert!(
        held["rest"][0]["resource"][0]["operation"].is_null(),
        "operations are declared at the system level"
    );
    let system = &held["rest"][0]["operation"][0];
    assert!(system["name"].as_str().is_some());
    assert!(
        system["definition"]["reference"].as_str().is_some(),
        "the definition is a reference in that version"
    );
    let later = statement(&service(FhirVersion::R4)).await;
    assert!(later["acceptUnknown"].is_null());
    assert!(later["rest"][0]["operation"][0]["definition"].is_string());
    assert!(later["rest"][0]["resource"][0]["operation"].is_array());
}

#[tokio::test]
async fn the_statement_advertises_only_the_formats_the_build_serves() {
    for version in FhirVersion::ALL {
        let held = statement(&service(version)).await;
        let declared: Vec<String> = held["format"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let wanted = [
            "json",
            "xml",
            "application/fhir+json",
            "application/fhir+xml",
        ];
        for format in wanted {
            assert!(
                declared.iter().any(|held| held == format),
                "{version} must advertise {format:?}, got {declared:?}"
            );
        }
        assert!(
            declared
                .iter()
                .all(|held| !held.contains("turtle") && held != "application/rdf+xml"),
            "{version} must not advertise a format it does not serve: {declared:?}"
        );
    }
}
