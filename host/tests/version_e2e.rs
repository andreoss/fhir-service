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
            &[("Content-Type", "application/fhir+json"), ("if-match", "W/\"1\"")],
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
        assert!(versions.body.contains(numbered), "{version}: {}", versions.body);
        assert_eq!(deleted.status, 204, "{version}: {}", deleted.body);
        assert_eq!(gone.status, 410, "{version}: {}", gone.body);
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
