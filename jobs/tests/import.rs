use fhir_adapter_memory::{MemoryJobStore, MemoryStore};
use fhir_core::FhirVersion;
use fhir_jobs::{ImportJob, Orchestrator, Worker};
use fhir_store::{JobId, JobKind, JobRequest, JobState, JobStore, ResourceStore, StepTicker};
use fhir_store_contract::fixture::{id, observation, patient};
use serde_json::Value;
use std::sync::Arc;

fn job(raw: &str) -> JobId {
    JobId::parse(raw).expect("test job id is valid")
}

fn queue() -> (Arc<MemoryJobStore>, StepTicker) {
    let ticker = StepTicker::starting_at(1_000);
    (Arc::new(MemoryJobStore::new(ticker.ticker())), ticker)
}

fn row(envelope: fhir_core::ResourceEnvelope) -> String {
    String::from_utf8(envelope.raw().to_vec()).expect("a fixture renders as text")
}

async fn ran(
    store: Arc<MemoryStore>,
    id: JobId,
    payload: impl Into<String>,
) -> fhir_store::JobRecord {
    let (jobs, _ticker) = queue();
    let orchestrator = Orchestrator::new().with(Arc::new(ImportJob::new(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        FhirVersion::R4,
    )));
    jobs.submit(JobRequest::new(id.clone(), JobKind::Import, payload))
        .await
        .unwrap();
    let worker = Worker::new(
        Arc::clone(&jobs) as Arc<dyn JobStore>,
        Arc::new(orchestrator),
        "one",
        1_000,
    );
    assert_eq!(worker.poll().await.unwrap(), 1);
    jobs.fetch(&id).await.unwrap()
}

fn outcome_of(record: &fhir_store::JobRecord) -> Value {
    serde_json::from_str(record.outcome.as_deref().expect("a report is recorded"))
        .expect("the report is an object")
}

#[tokio::test]
async fn newline_delimited_rows_are_loaded_and_a_bad_row_names_its_line() {
    let store = Arc::new(MemoryStore::default());
    let supplied = format!(
        "{}\n{{ this is not a resource\n\n{}\n{{\"resourceType\":\"Nothing\"}}\n",
        row(patient("i1", "Stone", true)),
        row(observation("o1", "code-1", 3.0, "Patient/i1")),
    );

    let record = ran(Arc::clone(&store), job("m1"), supplied).await;

    assert_eq!(record.state, JobState::Completed);
    let outcome = outcome_of(&record);
    assert_eq!(outcome["handled"], 2);
    let failures = outcome["failures"].as_array().expect("failures are listed");
    assert_eq!(failures.len(), 2);
    assert!(failures[0].as_str().unwrap().starts_with("row 1"), "{failures:?}");
    assert!(failures[1].as_str().unwrap().starts_with("row 4"), "{failures:?}");
    assert!(store.read(&id("i1")).await.is_ok());
    assert!(store.read(&id("o1")).await.is_ok());
}

#[tokio::test]
async fn a_blank_supply_loads_nothing_and_reports_nothing() {
    let store = Arc::new(MemoryStore::default());

    let record = ran(Arc::clone(&store), job("m2"), "\n  \n").await;

    assert_eq!(record.state, JobState::Completed);
    let outcome = outcome_of(&record);
    assert_eq!(outcome["units"], 0);
    assert_eq!(outcome["handled"], 0);
    assert_eq!(outcome["failures"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn a_described_supply_still_carries_its_rows_in_an_array() {
    let store = Arc::new(MemoryStore::default());
    let payload = format!(
        r#"{{"resources":[{},{{"resourceType":"Nothing"}}]}}"#,
        row(patient("i2", "Rivers", true))
    );

    let record = ran(Arc::clone(&store), job("m3"), payload).await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["handled"], 1);
    assert_eq!(outcome["failures"].as_array().unwrap().len(), 1);
    assert!(outcome["failures"][0].as_str().unwrap().starts_with("row 1"));
    assert!(store.read(&id("i2")).await.is_ok());
}

#[tokio::test]
async fn a_row_matching_what_is_held_creates_no_version() {
    let store = Arc::new(MemoryStore::default());
    let supplied = format!(
        "{}\n{}\n",
        row(patient("i3", "Stone", true)),
        row(observation("o3", "code-1", 3.0, "Patient/i3")),
    );

    let first = ran(Arc::clone(&store), job("m4"), supplied.clone()).await;
    let loaded = outcome_of(&first);
    assert_eq!(loaded["handled"], 2);
    assert_eq!(loaded["unchanged"], 0);
    assert_eq!(store.read(&id("i3")).await.unwrap().version_id().as_str(), "1");

    let again = ran(Arc::clone(&store), job("m5"), supplied).await;
    let repeated = outcome_of(&again);
    assert_eq!(repeated["units"], 2);
    assert_eq!(repeated["handled"], 0);
    assert_eq!(repeated["unchanged"], 2);
    assert_eq!(repeated["failures"].as_array().unwrap().len(), 0);
    assert_eq!(store.read(&id("i3")).await.unwrap().version_id().as_str(), "1");
    assert_eq!(store.read(&id("o3")).await.unwrap().version_id().as_str(), "1");
}

#[tokio::test]
async fn a_row_that_moved_on_creates_the_next_version() {
    let store = Arc::new(MemoryStore::default());
    let first = ran(
        Arc::clone(&store),
        job("m6"),
        format!("{}\n", row(patient("i4", "Stone", true))),
    )
    .await;
    assert_eq!(outcome_of(&first)["handled"], 1);

    let moved = ran(
        Arc::clone(&store),
        job("m7"),
        format!("{}\n", row(patient("i4", "Rivers", true))),
    )
    .await;

    let outcome = outcome_of(&moved);
    assert_eq!(outcome["handled"], 1);
    assert_eq!(outcome["unchanged"], 0);
    assert_eq!(store.read(&id("i4")).await.unwrap().version_id().as_str(), "2");
}
