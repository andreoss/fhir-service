use fhir_adapter_memory::{MemoryBulkStore, MemoryJobStore, MemoryStore};
use fhir_core::Error;
use fhir_jobs::{BulkDeleteJob, Orchestrator, Worker};
use fhir_store::{
    BulkStore, HistoryQuery, HistoryScope, JobId, JobKind, JobRecord, JobRequest, JobState,
    JobStore, ResourceStore, SearchQuery, StepTicker,
};
use fhir_store_contract::fixture::{id, observation, patient, Refusing};
use serde_json::Value;
use std::sync::Arc;

fn job(raw: &str) -> JobId {
    JobId::parse(raw).expect("test job id is valid")
}

fn sink() -> Arc<MemoryBulkStore> {
    Arc::new(MemoryBulkStore::new())
}

async fn seeded() -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .create(observation("o1", "code-1", 3.0, "Patient/p1"))
        .await
        .unwrap();
    store
}

async fn ran(
    store: Arc<dyn ResourceStore>,
    sink: Arc<dyn BulkStore>,
    id: JobId,
    payload: &str,
) -> JobRecord {
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let orchestrator = Orchestrator::new().with(Arc::new(BulkDeleteJob::new(store, sink)));
    jobs.submit(JobRequest::new(id.clone(), JobKind::BulkDelete, payload))
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

fn outcome_of(record: &JobRecord) -> Value {
    serde_json::from_str(record.outcome.as_deref().expect("a report is recorded"))
        .expect("the report is an object")
}

async fn versions(store: &dyn ResourceStore, resource_type: &str, held: &str) -> usize {
    let scope = HistoryScope::Instance(resource_type.parse().expect("a known type"), id(held));
    store
        .history(&scope, &HistoryQuery::default())
        .await
        .map(|page| page.total)
        .unwrap_or_default()
}

async fn live(store: &dyn ResourceStore, resource_type: &str) -> usize {
    let query = SearchQuery::of_type(resource_type.parse().expect("a known type"));
    store.search(&query).await.unwrap().entries.len()
}

#[tokio::test]
async fn a_bulk_delete_marks_the_named_type_and_leaves_the_rest() {
    let store = seeded().await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("b1"),
        r#"{"_type":["Patient"]}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let outcome = outcome_of(&record);
    assert_eq!(outcome["handled"], 2);
    assert_eq!(outcome["Patient"]["deleted"], 2);
    assert!(store.read(&id("p1")).await.unwrap().is_deleted());
    assert!(store.read(&id("p2")).await.unwrap().is_deleted());
    assert!(!store.read(&id("o1")).await.unwrap().is_deleted());
}

#[tokio::test]
async fn a_hard_delete_leaves_no_version_behind() {
    let store = seeded().await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("b2"),
        r#"{"_type":["Patient"],"hardDelete":true}"#,
    )
    .await;

    assert_eq!(outcome_of(&record)["handled"], 2);
    assert!(matches!(store.read(&id("p1")).await, Err(Error::NotFound)));
    assert_eq!(versions(store.as_ref(), "Patient", "p1").await, 0);
    assert_eq!(versions(store.as_ref(), "Observation", "o1").await, 1);
}

#[tokio::test]
async fn a_purge_alongside_a_delete_leaves_only_the_marker() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store
        .update(patient("p1", "Rivers", true), None)
        .await
        .unwrap();

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("b3"),
        r#"{"_type":["Patient"],"purgeHistory":true}"#,
    )
    .await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["handled"], 1);
    assert_eq!(outcome["Patient"]["purged"], 2);
    assert_eq!(versions(store.as_ref(), "Patient", "p1").await, 1);
    assert!(store.read(&id("p1")).await.unwrap().is_deleted());
}

#[tokio::test]
async fn a_maximum_count_caps_what_one_job_removes() {
    let store = seeded().await;
    store.create(patient("p3", "Fields", true)).await.unwrap();

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("b4"),
        r#"{"_type":["Patient"],"_maxCount":2}"#,
    )
    .await;

    assert_eq!(outcome_of(&record)["handled"], 2);
    assert_eq!(live(store.as_ref(), "Patient").await, 1);
}

#[tokio::test]
async fn an_excluded_type_is_not_touched() {
    let store = seeded().await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("b5"),
        r#"{"_exclude":["Observation"]}"#,
    )
    .await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["units"], 1);
    assert_eq!(outcome["handled"], 2);
    assert_eq!(live(store.as_ref(), "Patient").await, 0);
    assert_eq!(live(store.as_ref(), "Observation").await, 1);
}

#[tokio::test]
async fn only_the_already_deleted_go_when_the_soft_deleted_are_asked_for() {
    let store = seeded().await;
    store.delete(&id("p1")).await.unwrap();

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("b6"),
        r#"{"softDeleted":true,"_type":["Patient"]}"#,
    )
    .await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["handled"], 1);
    assert!(matches!(store.read(&id("p1")).await, Err(Error::NotFound)));
    assert_eq!(versions(store.as_ref(), "Patient", "p1").await, 0);
    assert!(!store.read(&id("p2")).await.unwrap().is_deleted());
}

#[tokio::test]
async fn a_soft_deleted_purge_keeps_the_marker_it_found() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store
        .update(patient("p1", "Rivers", true), None)
        .await
        .unwrap();
    store.delete(&id("p1")).await.unwrap();

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("b7"),
        r#"{"softDeleted":true,"purgeHistory":true}"#,
    )
    .await;

    assert_eq!(outcome_of(&record)["handled"], 1);
    assert_eq!(versions(store.as_ref(), "Patient", "p1").await, 1);
    assert!(store.read(&id("p1")).await.unwrap().is_deleted());
}

#[tokio::test]
async fn what_a_delete_could_not_remove_is_itemised_in_a_file() {
    let store = seeded().await;
    let refusing = Arc::new(Refusing::new(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        "p2",
    ));
    let files = sink();

    let record = ran(
        refusing as Arc<dyn ResourceStore>,
        Arc::clone(&files) as Arc<dyn BulkStore>,
        job("b8"),
        r#"{"_type":["Patient"]}"#,
    )
    .await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["handled"], 1);
    let failures = outcome["failures"].as_array().expect("failures are listed");
    assert_eq!(failures.len(), 1);
    assert!(failures[0].as_str().unwrap().contains("p2"), "{failures:?}");

    let written = files.list(&job("b8")).await.unwrap();
    let failure = written
        .iter()
        .find(|file| file.name == "Patient-failures.ndjson")
        .expect("a failure file is written");
    assert_eq!(failure.count, 1);
    assert_eq!(failure.kind, "OperationOutcome");
    let body = files.read(&job("b8"), &failure.name).await.unwrap();
    let rendered = String::from_utf8(body).expect("a failure file is text");
    assert!(rendered.contains("OperationOutcome"), "{rendered}");
    assert!(rendered.contains("p2"), "{rendered}");
}

#[tokio::test]
async fn a_delete_of_a_type_nothing_is_held_of_removes_nothing() {
    let store = Arc::new(MemoryStore::default());

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("b9"),
        r#"{"_type":["Patient"]}"#,
    )
    .await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["units"], 1);
    assert_eq!(outcome["handled"], 0);
}
