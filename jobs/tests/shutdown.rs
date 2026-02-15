use fhir_adapter_memory::MemoryJobStore;
use fhir_jobs::{Orchestrator, Worker};
use fhir_store::{JobKind, JobRequest, JobState, JobStore, Lease, StepTicker};
use std::sync::Arc;

fn queue() -> Arc<MemoryJobStore> {
    let ticker = StepTicker::starting_at(1_000);
    Arc::new(MemoryJobStore::new(ticker.ticker()))
}

fn submitted(id: &str) -> JobRequest {
    JobRequest::new(id.parse().expect("a job id"), JobKind::Export, "{}")
}

#[tokio::test]
async fn every_instance_claims_under_a_name_of_its_own() {
    let jobs = queue() as Arc<dyn JobStore>;
    let orchestrator = Arc::new(Orchestrator::new());
    let one = Worker::per_instance(Arc::clone(&jobs), Arc::clone(&orchestrator), 1_000);
    let two = Worker::per_instance(Arc::clone(&jobs), orchestrator, 1_000);
    assert_ne!(
        one.name(),
        two.name(),
        "two instances must not claim one name"
    );
    assert!(!one.name().is_empty());
}

#[tokio::test]
async fn a_stopping_worker_hands_back_what_it_held() {
    let store = queue();
    let jobs = Arc::clone(&store) as Arc<dyn JobStore>;
    jobs.submit(submitted("d1")).await.expect("submitted");
    let worker = Worker::per_instance(Arc::clone(&jobs), Arc::new(Orchestrator::new()), 600_000);
    let held = jobs
        .claim(&Lease::new(worker.name().to_owned(), 600_000))
        .await
        .expect("claimed");
    assert_eq!(held.len(), 1);

    let handed = worker.stopping().await.expect("handed back");
    assert_eq!(handed, 1);
    let record = jobs.fetch(&"d1".parse().unwrap()).await.expect("fetched");
    assert_eq!(record.state, JobState::Queued);
    assert!(record.worker.is_none());
}
