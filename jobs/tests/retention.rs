use fhir_adapter_memory::{MemoryJobStore, MemoryStore};
use fhir_core::{FhirInstant, FhirVersion, ResourceEnvelope};
use fhir_jobs::{
    BulkDeleteJob, BulkDeleteRequest, Orchestrator, Retention, RetentionWorker, Worker,
};
use fhir_store::{JobStore, ResourceStore, SearchQuery, StepTicker};
use std::sync::Arc;

fn kind(name: &str) -> fhir_core::ResourceType {
    name.parse().expect("a served type")
}

fn envelope(id: &str, stamped: &str) -> ResourceEnvelope {
    let body = serde_json::json!({
        "resourceType": "Observation",
        "id": id,
        "status": "final",
        "code": {"text": "probe"},
        "meta": {"versionId": "1", "lastUpdated": stamped}
    })
    .to_string();
    ResourceEnvelope::parse(FhirVersion::R4, body.as_bytes()).expect("a valid envelope")
}

#[tokio::test]
async fn nothing_configured_submits_nothing() {
    let ticker = StepTicker::starting_at(1_780_000_000_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let worker = RetentionWorker::new(Arc::clone(&jobs) as Arc<dyn JobStore>, Retention::default())
        .with_ticker(ticker.ticker());
    assert!(worker.sweep().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_rule_submits_a_bulk_delete_naming_its_cutoff() {
    let ticker = StepTicker::starting_at(1_780_000_000_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let held = Retention::parse("Observation=30:purge", FhirVersion::R4).unwrap();
    let worker = RetentionWorker::new(Arc::clone(&jobs) as Arc<dyn JobStore>, held)
        .with_ticker(ticker.ticker());

    let swept = worker.sweep().await.unwrap();
    assert_eq!(swept.submitted.len(), 1);
    assert_eq!(swept.submitted[0].0, kind("Observation"));

    let queued = jobs.list(&fhir_store::JobFilter::default()).await.unwrap();
    assert_eq!(queued.len(), 1, "{queued:?}");
    let payload: serde_json::Value =
        serde_json::from_str(queued[0].payload.as_deref().expect("a payload")).unwrap();
    assert_eq!(payload["_type"][0], "Observation");
    assert_eq!(payload["purgeHistory"], true);
    assert!(payload["_before"].as_str().unwrap().starts_with("20"));
    assert!(payload["_rule"].as_str().unwrap().contains("30 days"));
}

#[tokio::test]
async fn a_sweep_waits_for_its_own_period() {
    let ticker = StepTicker::starting_at(1_780_000_000_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let held = Retention::parse("Observation=30", FhirVersion::R4)
        .unwrap()
        .every(10_000);
    let worker = RetentionWorker::new(Arc::clone(&jobs) as Arc<dyn JobStore>, held)
        .with_ticker(ticker.ticker());

    assert_eq!(worker.sweep().await.unwrap().submitted.len(), 1);
    assert!(worker.sweep().await.unwrap().is_empty());
    ticker.advance(11_000);
    assert_eq!(worker.sweep().await.unwrap().submitted.len(), 1);
}

#[tokio::test]
async fn the_window_removes_what_sat_past_it_and_leaves_the_rest() {
    
    
    let written = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stamps = ["2020-01-01T00:00:00.000Z", "2026-09-01T00:00:00.000Z"];
    let counted = Arc::clone(&written);
    let store = Arc::new(MemoryStore::with_clock(Arc::new(move || {
        let at = counted
            .load(std::sync::atomic::Ordering::SeqCst)
            .min(stamps.len() - 1);
        FhirInstant::parse(stamps[at]).unwrap()
    })));
    let held = Arc::clone(&store) as Arc<dyn ResourceStore>;
    held.create(envelope("old", "2020-01-01T00:00:00.000Z"))
        .await
        .unwrap();
    written.store(1, std::sync::atomic::Ordering::SeqCst);
    held.create(envelope("new", "2026-09-01T00:00:00.000Z"))
        .await
        .unwrap();

    let payload = serde_json::json!({
        "_type": ["Observation"],
        "_before": "2024-01-01T00:00:00Z",
        "hardDelete": false,
        "purgeHistory": false,
        "softDeleted": false
    })
    .to_string();
    let request = BulkDeleteRequest::parse(&payload).unwrap();
    assert_eq!(request.before.as_deref(), Some("2024-01-01T00:00:00Z"));

    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let orchestrator = Orchestrator::new().with(Arc::new(BulkDeleteJob::new(
        Arc::clone(&held),
        Arc::new(fhir_adapter_memory::MemoryBulkStore::new()),
    )));
    let id = fhir_store::JobId::parse("retention-run").unwrap();
    jobs.submit(fhir_store::JobRequest::new(
        id.clone(),
        fhir_store::JobKind::BulkDelete,
        payload,
    ))
    .await
    .unwrap();
    let worker = Worker::new(
        Arc::clone(&jobs) as Arc<dyn JobStore>,
        Arc::new(orchestrator),
        "one",
        1_000,
    );
    assert_eq!(worker.poll().await.unwrap(), 1);

    let live = held
        .search(&SearchQuery::of_type(kind("Observation")))
        .await
        .unwrap();
    let ids: Vec<String> = live
        .entries
        .iter()
        .map(|entry| entry.id().as_str().to_owned())
        .collect();
    assert_eq!(ids, vec!["new".to_owned()], "{ids:?}");
}
