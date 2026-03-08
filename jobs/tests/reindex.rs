use fhir_adapter_memory::{MemoryBulkStore, MemoryJobStore, MemoryStore};
use fhir_jobs::{Orchestrator, ReindexJob, Worker};
use fhir_store::{
    BulkStore, JobId, JobKind, JobRecord, JobRequest, JobState, JobStore, ResourceStore, StepTicker,
};
use fhir_store_contract::fixture::envelope;
use serde_json::Value;
use std::sync::Arc;

fn job(raw: &str) -> JobId {
    JobId::parse(raw).expect("test job id is valid")
}

fn sink() -> Arc<MemoryBulkStore> {
    Arc::new(MemoryBulkStore::new())
}

fn definition(id: &str, url: &str, code: &str, value_type: &str) -> fhir_core::ResourceEnvelope {
    envelope(
        "SearchParameter",
        id,
        &format!(
            r#""url":"{url}","status":"active","code":"{code}","base":["Patient"],"type":"{value_type}","expression":"Patient.extension.valueCode""#
        ),
    )
}

fn banded(id: &str, code: &str) -> fhir_core::ResourceEnvelope {
    envelope(
        "Patient",
        id,
        &format!(r#""extension":[{{"url":"urn:e:band","valueCode":"{code}"}}]"#),
    )
}

async fn seeded() -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    store
        .create(definition("sp1", "urn:p:when", "when", "date"))
        .await
        .unwrap();
    store.create(banded("t1", "2024-01-01")).await.unwrap();
    store.create(banded("t2", "high")).await.unwrap();
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
    let orchestrator = Orchestrator::new().with(Arc::new(ReindexJob::new(store, sink)));
    jobs.submit(JobRequest::new(id.clone(), JobKind::Reindex, payload))
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

#[tokio::test]
async fn a_reindex_naming_nothing_backfills_every_parameter() {
    let store = seeded().await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("r1"),
        "{}",
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let outcome = outcome_of(&record);
    assert_eq!(outcome["units"], 1);
    assert_eq!(outcome["handled"], 1);
    let failures = outcome["failures"].as_array().expect("failures are listed");
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].as_str().unwrap().contains("t2"), "{failures:?}");
    assert_eq!(
        store
            .index_report("urn:p:when")
            .await
            .unwrap()
            .unwrap()
            .indexed,
        1
    );
}

#[tokio::test]
async fn a_named_resource_is_the_only_one_read() {
    let store = seeded().await;
    ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("r2"),
        "{}",
    )
    .await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("r3"),
        r#"{"resources":["Patient/t1"]}"#,
    )
    .await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["units"], 1);
    assert_eq!(outcome["handled"], 1);
    assert!(
        outcome["failures"].as_array().expect("failures").is_empty(),
        "a neighbour was read: {outcome}"
    );
    assert_eq!(outcome["Patient/t1"]["parameters"], 1);
}

#[tokio::test]
async fn a_named_parameter_is_the_only_one_rebuilt() {
    let store = seeded().await;
    store
        .create(definition("sp2", "urn:p:band", "band", "token"))
        .await
        .unwrap();

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("r4"),
        r#"{"urls":["urn:p:band"]}"#,
    )
    .await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["units"], 1);
    assert_eq!(outcome["handled"], 2);
    assert_eq!(outcome["urn:p:band"]["indexed"], 2);
    assert!(store.index_report("urn:p:when").await.unwrap().is_none());
}

#[tokio::test]
async fn a_resource_a_parameter_cannot_read_is_itemised_in_a_file() {
    let store = seeded().await;
    let files = sink();
    ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        Arc::clone(&files) as Arc<dyn BulkStore>,
        job("r5"),
        "{}",
    )
    .await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        Arc::clone(&files) as Arc<dyn BulkStore>,
        job("r6"),
        r#"{"resources":["Patient/t2"]}"#,
    )
    .await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["handled"], 0);
    let failures = outcome["failures"].as_array().expect("failures are listed");
    assert_eq!(failures.len(), 1);
    let written = files.list(&job("r6")).await.unwrap();
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(written[0].count, 1);
    let body = files.read(&job("r6"), &written[0].name).await.unwrap();
    let rendered = String::from_utf8(body).expect("a failure file is text");
    assert!(rendered.contains("OperationOutcome"), "{rendered}");
}

#[tokio::test]
async fn a_reindex_naming_an_unknown_resource_reports_it_and_carries_on() {
    let store = seeded().await;
    ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("r8"),
        "{}",
    )
    .await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("r7"),
        r#"{"resources":["Patient/nobody","Patient/t1"]}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let outcome = outcome_of(&record);
    assert_eq!(outcome["units"], 2);
    assert_eq!(outcome["handled"], 1);
    let failures = outcome["failures"].as_array().expect("failures are listed");
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].as_str().unwrap().contains("nobody"),
        "{failures:?}"
    );
}
