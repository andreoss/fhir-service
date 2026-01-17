use fhir_adapter_memory::{MemoryBulkStore, MemoryJobStore, MemoryStore};
use fhir_core::FhirInstant;
use fhir_jobs::{ExportJob, Orchestrator, Worker};
use fhir_store::{
    BulkStore, JobId, JobKind, JobRecord, JobRequest, JobState, JobStore, ResourceStore, StepTicker,
};
use fhir_store_contract::fixture::{observation, patient};
use serde_json::Value;
use std::sync::{Arc, Mutex};

fn job(raw: &str) -> JobId {
    JobId::parse(raw).expect("test job id is valid")
}

struct Hand {
    now: Mutex<String>,
}

fn clock(hand: &Arc<Hand>) -> fhir_store::Clock {
    let hand = Arc::clone(hand);
    Arc::new(move || {
        FhirInstant::parse(&hand.now.lock().unwrap().clone()).expect("a written instant")
    })
}

fn at(hand: &Arc<Hand>, instant: &str) {
    *hand.now.lock().unwrap() = instant.to_owned();
}

async fn ran(
    store: Arc<MemoryStore>,
    sink: Arc<MemoryBulkStore>,
    id: JobId,
    payload: &str,
) -> JobRecord {
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    jobs.submit(JobRequest::new(id.clone(), JobKind::Export, payload))
        .await
        .unwrap();
    let orchestrator = Arc::new(Orchestrator::new().with(Arc::new(ExportJob::new(
        store as Arc<dyn ResourceStore>,
        sink as Arc<dyn BulkStore>,
    ))));
    let worker = Worker::new(
        Arc::clone(&jobs) as Arc<dyn JobStore>,
        orchestrator,
        "one",
        5_000,
    );
    worker.poll().await.unwrap();
    jobs.fetch(&id).await.unwrap()
}

fn rows(body: &[u8]) -> Vec<Value> {
    String::from_utf8(body.to_vec())
        .expect("ndjson is text")
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("every row is one resource"))
        .collect()
}

#[tokio::test]
async fn a_system_export_writes_every_resource_to_one_file_of_its_type() {
    let hand = Arc::new(Hand {
        now: Mutex::new("2026-09-06T04:00:00.000Z".to_owned()),
    });
    let store = Arc::new(MemoryStore::with_clock(clock(&hand)));
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .create(observation("o1", "code-1", 3.0, "Patient/p1"))
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());
    at(&hand, "2026-09-06T05:00:00.000Z");

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("x1"),
        r#"{"scope":"system","_till":"2026-09-06T05:00:00.000Z"}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let outcome: Value = serde_json::from_str(&record.outcome.unwrap()).unwrap();
    assert_eq!(outcome["handled"], 3);
    let files = sink.list(&job("x1")).await.unwrap();
    let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, vec!["Observation.ndjson", "Patient.ndjson"]);
    let patients = rows(&sink.read(&job("x1"), "Patient.ndjson").await.unwrap());
    let ids: Vec<&str> = patients
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["p1", "p2"]);
    assert_eq!(files[1].kind, "Patient");
    assert_eq!(files[1].count, 2);
}

#[tokio::test]
async fn an_export_reads_a_fixed_point_and_carries_every_resource_once() {
    let hand = Arc::new(Hand {
        now: Mutex::new("2026-09-06T04:00:00.000Z".to_owned()),
    });
    let store = Arc::new(MemoryStore::with_clock(clock(&hand)));
    store.create(patient("p1", "Stone", true)).await.unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    at(&hand, "2026-09-06T06:00:00.000Z");
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store.update(patient("p1", "Fields", true), None).await.unwrap();

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("x2"),
        r#"{"scope":"system","_type":["Patient"],"_till":"2026-09-06T05:00:00.000Z"}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let patients = rows(&sink.read(&job("x2"), "Patient.ndjson").await.unwrap());
    assert_eq!(patients.len(), 1);
    assert_eq!(patients[0]["id"], "p1");
    assert_eq!(patients[0]["name"][0]["family"], "Stone");
    assert_eq!(patients[0]["meta"]["versionId"], "1");
}

#[tokio::test]
async fn a_deleted_resource_leaves_the_export() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .delete(&fhir_store_contract::fixture::id("p2"))
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("x3"),
        r#"{"scope":"system","_type":["Patient"]}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let patients = rows(&sink.read(&job("x3"), "Patient.ndjson").await.unwrap());
    assert_eq!(patients.len(), 1);
    assert_eq!(patients[0]["id"], "p1");
}

#[tokio::test]
async fn a_patient_export_carries_the_compartment_and_a_group_export_its_members() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .create(observation("o1", "code-1", 3.0, "Patient/p1"))
        .await
        .unwrap();
    store
        .create(observation("o2", "code-2", 4.0, "Patient/p2"))
        .await
        .unwrap();
    let group = fhir_store_contract::fixture::envelope(
        "Group",
        "g1",
        r#""member":[{"entity":{"reference":"Patient/p1"}}]"#,
    );
    store.create(group).await.unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let all = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("x4"),
        r#"{"scope":"patient"}"#,
    )
    .await;
    assert_eq!(all.state, JobState::Completed);
    let listed: Vec<String> = sink
        .list(&job("x4"))
        .await
        .unwrap()
        .into_iter()
        .map(|file| file.name)
        .collect();
    assert_eq!(listed, vec!["Observation.ndjson", "Patient.ndjson"]);
    assert_eq!(
        rows(&sink.read(&job("x4"), "Patient.ndjson").await.unwrap()).len(),
        2
    );

    let one = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("x5"),
        r#"{"scope":"group","id":"g1"}"#,
    )
    .await;
    assert_eq!(one.state, JobState::Completed);
    let patients = rows(&sink.read(&job("x5"), "Patient.ndjson").await.unwrap());
    assert_eq!(patients.len(), 1);
    assert_eq!(patients[0]["id"], "p1");
    let observations = rows(&sink.read(&job("x5"), "Observation.ndjson").await.unwrap());
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0]["id"], "o1");
}
