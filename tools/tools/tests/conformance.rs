use fhir_adapter_memory::MemoryStore;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use fhir_tools::conformance::{capture, EXCHANGES};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn held(into: &Path, name: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(into.join(format!("{name}.json")))
        .unwrap_or_else(|_| panic!("{name} was not captured"));
    serde_json::from_str(&text).expect("the capture is json")
}

fn code_of(resource: &serde_json::Value, system: &str) -> Option<String> {
    resource["code"]["coding"]
        .as_array()
        .and_then(|codings| codings.iter().find(|coding| coding["system"] == system))
        .and_then(|coding| coding["code"].as_str())
        .map(str::to_owned)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_capture_keeps_every_answer_a_running_instance_gave() {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-07T00:00:00.000Z").unwrap()
    }));
    let service = Service::new(Arc::new(store), FhirVersion::R4, Vec::new());
    let bound = service
        .bind("127.0.0.1:0".parse().unwrap())
        .await
        .expect("the instance binds a port it was given");
    let address = bound
        .local_addr()
        .expect("the address is announced")
        .to_string();
    tokio::spawn(async move { bound.serve().await });
    let into = PathBuf::from("../../scratch/conformance-capture");
    let held = tokio::task::spawn_blocking(move || capture(&address, "fhir.test", &into))
        .await
        .expect("the capture runs")
        .expect("the capture succeeds");
    assert_eq!(held.len(), EXCHANGES.len());
    let statement: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string("../../scratch/conformance-capture/capability.json")
            .expect("the statement is kept"),
    )
    .expect("the statement is json");
    assert_eq!(statement["resourceType"], "CapabilityStatement");
    assert!(statement["implementation"]["url"]
        .as_str()
        .is_some_and(|url| url.contains("fhir.test")));
    let outcome: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string("../../scratch/conformance-capture/outcome.json")
            .expect("the outcome is kept"),
    )
    .expect("the outcome is json");
    assert_eq!(outcome["resourceType"], "OperationOutcome");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_capture_holds_codes_a_terminology_server_judges() {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-08T10:00:00.000Z").unwrap()
    }));
    let service = Service::new(Arc::new(store), FhirVersion::R4, Vec::new());
    let bound = service
        .bind("127.0.0.1:0".parse().unwrap())
        .await
        .expect("the instance binds a port it was given");
    let address = bound
        .local_addr()
        .expect("the address is announced")
        .to_string();
    tokio::spawn(async move { bound.serve().await });
    let into = PathBuf::from("../../scratch/conformance-capture-codes");
    let folder = into.clone();
    tokio::task::spawn_blocking(move || capture(&address, "fhir.test", &folder))
        .await
        .expect("the capture runs")
        .expect("the capture succeeds");
    let condition = held(&into, "created-condition");
    assert_eq!(condition["resourceType"], "Condition");
    assert_eq!(
        code_of(&condition, "http://snomed.info/sct").as_deref(),
        Some("38341003")
    );
    let rate = held(&into, "created-heart-rate");
    assert_eq!(rate["resourceType"], "Observation");
    assert_eq!(
        code_of(&rate, "http://loinc.org").as_deref(),
        Some("8867-4")
    );
    assert!(
        rate.get("effectiveDateTime").is_some(),
        "a heart rate needs an effective time: {rate}"
    );
}
