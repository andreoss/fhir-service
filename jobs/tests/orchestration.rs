use fhir_adapter_memory::{MemoryBulkStore, MemoryJobStore, MemoryStore};
use fhir_core::FhirVersion;
use fhir_jobs::{BulkDeleteJob, BulkUpdateJob, ExportJob, ImportJob, Orchestrator, Worker};
use fhir_store::{
    JobId, JobKind, JobRequest, JobState, JobStore, ResourceStore, SearchQuery, StepTicker,
};
use fhir_store_contract::fixture::patient;
use std::sync::Arc;

fn job(raw: &str) -> JobId {
    JobId::parse(raw).expect("test job id is valid")
}

async fn seeded() -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
}

fn sink() -> Arc<dyn fhir_store::BulkStore> {
    Arc::new(MemoryBulkStore::new())
}

fn queue() -> (Arc<MemoryJobStore>, StepTicker) {
    let ticker = StepTicker::starting_at(1_000);
    (Arc::new(MemoryJobStore::new(ticker.ticker())), ticker)
}

async fn ran(
    jobs: Arc<MemoryJobStore>,
    orchestrator: Orchestrator,
    request: JobRequest,
) -> fhir_store::JobRecord {
    let id = request.id.clone();
    jobs.submit(request).await.unwrap();
    let worker = Worker::new(Arc::clone(&jobs) as Arc<dyn JobStore>, Arc::new(orchestrator), "one", 1_000);
    assert_eq!(worker.poll().await.unwrap(), 1);
    jobs.fetch(&id).await.unwrap()
}

#[tokio::test]
async fn an_export_counts_every_resource_of_the_named_types() {
    let store = seeded().await;
    let (jobs, _ticker) = queue();
    let orchestrator = Orchestrator::new().with(Arc::new(ExportJob::new(store, sink())));
    let record = ran(
        jobs,
        orchestrator,
        JobRequest::new(job("e1"), JobKind::Export, r#"{"types":["Patient"]}"#),
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    assert_eq!(record.progress.percent(), Some(100));
    let outcome: serde_json::Value =
        serde_json::from_str(&record.outcome.expect("a completed job reports")).unwrap();
    assert_eq!(outcome["handled"], 2);
    assert_eq!(outcome["units"], 1);
}

#[tokio::test]
async fn an_export_without_named_types_plans_every_stored_type() {
    let store = seeded().await;
    let (jobs, _ticker) = queue();
    let orchestrator = Orchestrator::new().with(Arc::new(ExportJob::new(store, sink())));
    let record = ran(
        jobs,
        orchestrator,
        JobRequest::new(job("e2"), JobKind::Export, "{}"),
    )
    .await;

    let outcome: serde_json::Value = serde_json::from_str(&record.outcome.unwrap()).unwrap();
    assert_eq!(outcome["units"], 1);
    assert_eq!(outcome["handled"], 2);
}

#[tokio::test]
async fn an_import_writes_rows_and_reports_a_bad_row_by_position() {
    let store = Arc::new(MemoryStore::default());
    let row = String::from_utf8(patient("i1", "Stone", true).to_json()).unwrap();
    let payload = format!(r#"{{"resources":[{row},{{"resourceType":"Nothing"}}]}}"#);
    let (jobs, _ticker) = queue();
    let orchestrator = Orchestrator::new().with(Arc::new(ImportJob::new(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        FhirVersion::R4,
    )));
    let record = ran(
        jobs,
        orchestrator,
        JobRequest::new(job("m1"), JobKind::Import, payload),
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let outcome: serde_json::Value = serde_json::from_str(&record.outcome.unwrap()).unwrap();
    assert_eq!(outcome["handled"], 1);
    assert_eq!(outcome["failures"].as_array().unwrap().len(), 1);
    assert!(outcome["failures"][0].as_str().unwrap().starts_with("row 1"));
    assert!(store.read(&fhir_store_contract::fixture::id("i1")).await.is_ok());
}

#[tokio::test]
async fn a_bulk_delete_removes_every_matching_resource() {
    let store = seeded().await;
    let (jobs, _ticker) = queue();
    let orchestrator =
        Orchestrator::new().with(Arc::new(BulkDeleteJob::new(Arc::clone(&store) as Arc<dyn ResourceStore>)));
    let record = ran(
        jobs,
        orchestrator,
        JobRequest::new(job("d1"), JobKind::BulkDelete, r#"{"types":["Patient"]}"#),
    )
    .await;

    let outcome: serde_json::Value = serde_json::from_str(&record.outcome.unwrap()).unwrap();
    assert_eq!(outcome["handled"], 2);
    let left = store.search(&SearchQuery::default()).await.unwrap();
    assert!(left.entries.is_empty(), "{left:?}");
}

#[tokio::test]
async fn a_bulk_update_patches_every_matching_resource() {
    let store = seeded().await;
    let (jobs, _ticker) = queue();
    let orchestrator =
        Orchestrator::new().with(Arc::new(BulkUpdateJob::new(Arc::clone(&store) as Arc<dyn ResourceStore>)));
    let payload = r#"{"types":["Patient"],"patch":[{"op":"replace","path":"/active","value":false}]}"#;
    let record = ran(
        jobs,
        orchestrator,
        JobRequest::new(job("u1"), JobKind::BulkUpdate, payload),
    )
    .await;

    let outcome: serde_json::Value = serde_json::from_str(&record.outcome.unwrap()).unwrap();
    assert_eq!(outcome["handled"], 2);
    let read = store.read(&fhir_store_contract::fixture::id("p1")).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(read.raw()).unwrap();
    assert_eq!(body["active"], false);
}

#[tokio::test]
async fn a_job_of_an_unregistered_kind_fails_with_its_reason() {
    let (jobs, _ticker) = queue();
    let record = ran(
        jobs,
        Orchestrator::new(),
        JobRequest::new(job("x1"), JobKind::Reindex, "{}"),
    )
    .await;

    assert_eq!(record.state, JobState::Failed);
    assert!(record.outcome.unwrap().contains("reindex"));
}

#[tokio::test]
async fn a_malformed_payload_fails_the_job_and_not_the_worker() {
    let store = seeded().await;
    let (jobs, _ticker) = queue();
    let orchestrator = Orchestrator::new().with(Arc::new(ExportJob::new(store, sink())));
    let record = ran(
        jobs,
        orchestrator,
        JobRequest::new(job("b1"), JobKind::Export, "not json"),
    )
    .await;

    assert_eq!(record.state, JobState::Failed);
    assert!(record.outcome.is_some());
}

#[tokio::test]
async fn an_orchestrator_reports_the_kinds_it_runs() {
    let store = seeded().await;
    let orchestrator = Orchestrator::default().with(Arc::new(ExportJob::new(store, sink())));
    assert_eq!(orchestrator.kinds(), vec![JobKind::Export]);
    assert!(orchestrator.handler(JobKind::Import).is_err());
}

#[tokio::test]
async fn a_reindex_backfills_the_stored_parameters() {
    let store = seeded().await;
    let definition = br#"{"resourceType":"SearchParameter","id":"sp-1","meta":{"versionId":"1","lastUpdated":"2026-09-06T04:00:00.000Z"},"url":"urn:p:band","status":"active","code":"band","base":["Patient"],"type":"string","expression":"Patient.name.family"}"#;
    let envelope = fhir_core::ResourceEnvelope::parse(FhirVersion::R4, definition).unwrap();
    store.create(envelope).await.unwrap();
    let (jobs, _ticker) = queue();
    let orchestrator = Orchestrator::new().with(Arc::new(fhir_jobs::ReindexJob::new(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
    )));
    let record = ran(
        jobs,
        orchestrator,
        JobRequest::new(job("r1"), JobKind::Reindex, r#"{"urls":["urn:p:band"]}"#),
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let outcome: serde_json::Value = serde_json::from_str(&record.outcome.unwrap()).unwrap();
    assert_eq!(outcome["units"], 1);
    assert_eq!(outcome["handled"], 2);
}
