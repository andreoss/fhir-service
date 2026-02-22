mod live;

use live::{request, spawn_with, stop};

const VERSIONS: [(&str, &str); 4] = [
    ("STU3", "3.0.2"),
    ("R4", "4.0.1"),
    ("R4B", "4.3.0"),
    ("R5", "5.0.0"),
];

fn patient(id: &str) -> Vec<u8> {
    format!(r#"{{"resourceType":"Patient","id":"{id}","active":true}}"#).into_bytes()
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).expect("the answer is a document")
}

#[test]
fn live_every_named_version_serves_a_whole_interaction_cycle() {
    for (version, numbered) in VERSIONS {
        let (child, port) = spawn_with(&[("FHIR_VERSION", version)]);
        let headers = [("Content-Type", "application/fhir+json")];
        let created = request(port, "POST", "/Patient", &headers, &patient("e2e-one"));
        let read = request(port, "GET", "/Patient/e2e-one", &[], &[]);
        let updated = request(
            port,
            "PUT",
            "/Patient/e2e-one",
            &[
                ("Content-Type", "application/fhir+json"),
                ("if-match", "W/\"1\""),
            ],
            r#"{"resourceType":"Patient","id":"e2e-one","active":false}"#.as_bytes(),
        );
        let searched = request(port, "GET", "/Patient?_id=e2e-one", &[], &[]);
        let history = request(port, "GET", "/Patient/e2e-one/_history", &[], &[]);
        let capability = request(port, "GET", "/metadata", &[], &[]);
        let versions = request(port, "GET", "/$versions", &[], &[]);
        let deleted = request(port, "DELETE", "/Patient/e2e-one", &[], &[]);
        let gone = request(port, "GET", "/Patient/e2e-one", &[], &[]);
        stop(child);

        assert_eq!(created.status, 201, "{version}: {}", created.body);
        assert_eq!(read.status, 200, "{version}: {}", read.body);
        assert_eq!(json(&read.body)["id"], "e2e-one", "{version}");
        assert_eq!(updated.status, 200, "{version}: {}", updated.body);
        assert_eq!(searched.status, 200, "{version}: {}", searched.body);
        assert_eq!(json(&searched.body)["total"], 1, "{version}");
        assert_eq!(history.status, 200, "{version}: {}", history.body);
        assert_eq!(capability.status, 200, "{version}: {}", capability.body);
        assert_eq!(json(&capability.body)["fhirVersion"], numbered, "{version}");
        assert_eq!(versions.status, 200, "{version}: {}", versions.body);
        assert!(
            versions.body.contains(numbered),
            "{version}: {}",
            versions.body
        );
        assert_eq!(deleted.status, 204, "{version}: {}", deleted.body);
        assert_eq!(gone.status, 410, "{version}: {}", gone.body);
    }
}

#[test]
fn live_every_named_version_answers_a_full_text_search_over_the_narrative() {
    for (version, _) in VERSIONS {
        let (child, port) = spawn_with(&[("FHIR_VERSION", version)]);
        let headers = [("Content-Type", "application/fhir+json")];
        let observation = r#"{"resourceType":"Observation","id":"e2e-text","status":"final","text":{"status":"generated","div":"<div><p>Fever and chills with bone pain</p></div>"},"code":{"text":"note"}}"#;
        let created = request(port, "POST", "/Observation", &headers, observation.as_bytes());
        let found = request(port, "GET", "/Observation?_text=fever", &[], &[]);
        let none = request(port, "GET", "/Observation?_text=rash", &[], &[]);
        let boolean = request(
            port,
            "GET",
            "/Observation?_text=(bone%20OR%20liver)%20AND%20pain",
            &[],
            &[],
        );
        stop(child);

        assert_eq!(created.status, 201, "{version}: {}", created.body);
        assert_eq!(found.status, 200, "{version}: {}", found.body);
        assert_eq!(json(&found.body)["total"], 1, "{version}");
        assert_eq!(
            json(&found.body)["entry"][0]["resource"]["id"],
            "e2e-text",
            "{version}"
        );
        assert_eq!(none.status, 200, "{version}: {}", none.body);
        assert_eq!(json(&none.body)["total"], 0, "{version}");
        assert_eq!(boolean.status, 200, "{version}: {}", boolean.body);
        assert_eq!(json(&boolean.body)["total"], 1, "{version}");
    }
}

#[test]
fn live_a_capability_statement_names_the_version_it_was_started_for() {
    for (version, numbered) in VERSIONS {
        let (child, port) = spawn_with(&[("FHIR_VERSION", version)]);
        let capability = request(port, "GET", "/metadata", &[], &[]);
        stop(child);

        assert_eq!(capability.status, 200, "{version}: {}", capability.body);
        assert_eq!(json(&capability.body)["fhirVersion"], numbered, "{version}");
    }
}

#[test]
fn live_a_version_the_service_does_not_serve_is_refused() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BACKEND", "memory")
        .env("FHIR_VERSION", "R6")
        .output()
        .expect("the binary runs");
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("FHIR_VERSION"), "{said}");
}

#[test]
fn live_each_version_serves_the_types_it_defines_and_refuses_the_rest() {
    let citation = br#"{"resourceType":"Citation","id":"ct-1","status":"active"}"#;
    for (version, _) in VERSIONS {
        let (child, port) = spawn_with(&[("FHIR_VERSION", version)]);
        let headers = [("Content-Type", "application/fhir+json")];
        let created = request(port, "POST", "/Citation", &headers, citation);
        let statement = request(port, "GET", "/metadata", &[], &[]);
        stop(child);

        let defines = version == "R4B" || version == "R5";
        match defines {
            true => assert_eq!(created.status, 201, "{version}: {}", created.body),
            false => assert_eq!(created.status, 400, "{version}: {}", created.body),
        }
        assert_eq!(
            statement.body.contains("\"Citation\""),
            defines,
            "{version} statement disagrees with the types it serves"
        );
    }
}

#[test]
fn live_a_body_that_does_not_match_its_type_is_refused_by_every_version() {
    for (version, _) in VERSIONS {
        let (child, port) = spawn_with(&[("FHIR_VERSION", version)]);
        let headers = [("Content-Type", "application/fhir+json")];
        let bad = request(
            port,
            "POST",
            "/Patient",
            &headers,
            br#"{"resourceType":"Patient","id":"pt-x1","favourite":"tea"}"#,
        );
        let read = request(port, "GET", "/Patient/pt-x1", &[], &[]);
        let validated = request(
            port,
            "POST",
            "/Observation/$validate",
            &headers,
            br#"{"resourceType":"Observation","id":"ob-x1","status":"draft"}"#,
        );
        stop(child);

        assert_eq!(bad.status, 400, "{version}: {}", bad.body);
        assert!(bad.body.contains("structure"), "{version}: {}", bad.body);
        assert_eq!(read.status, 404, "{version}: {}", read.body);
        assert_eq!(validated.status, 200, "{version}: {}", validated.body);
        assert!(
            validated.body.contains("cardinality"),
            "{version}: {}",
            validated.body
        );
    }
}

#[test]
fn live_each_version_emits_the_shapes_that_version_defines() {
    for (version, _) in VERSIONS {
        let (child, port) = spawn_with(&[("FHIR_VERSION", version)]);
        let created = request(port, "POST", "/Patient", &[], &patient("pv-1"));
        let statement = request(port, "GET", "/metadata", &[], &[]);
        let history = request(port, "GET", "/Patient/_history", &[], &[]);
        stop(child);
        assert_eq!(created.status, 201, "{version}: {}", created.body);
        let held = json(&statement.body);
        let profile = &held["rest"][0]["resource"][0]["profile"];
        let entry = &json(&history.body)["entry"][0];
        match version {
            "STU3" => {
                assert!(
                    profile["reference"].as_str().is_some(),
                    "{version}: {profile}"
                );
                assert!(entry["response"].is_null(), "{version}: {entry}");
            }
            _ => {
                assert!(profile.as_str().is_some(), "{version}: {profile}");
                assert!(entry["response"]["status"].as_str().is_some(), "{version}");
            }
        }
        assert!(entry["request"]["method"].as_str().is_some(), "{version}");
    }
}
