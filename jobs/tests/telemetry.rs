use fhir_adapter_memory::{MemoryBulkStore, MemoryJobStore, MemoryStore};
use fhir_core::Error;
use fhir_jobs::{measured, ExportJob, ImportJob, Orchestrator, Worker};
use fhir_store::{JobId, JobKind, JobRequest, JobStore, ResourceStore, StepTicker};
use fhir_store_contract::fixture::patient;
use fhir_telemetry::{Dimensions, Held, Operation, Outcome, Telemetry};
use std::sync::Arc;

fn job(raw: &str) -> JobId {
    JobId::parse(raw).expect("test job id is valid")
}

async fn seeded() -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store
}

async fn ran(orchestrator: Orchestrator, request: JobRequest) {
    let ticker = StepTicker::starting_at(1_000);
    let jobs: Arc<dyn JobStore> = Arc::new(MemoryJobStore::new(ticker.ticker()));
    jobs.submit(request).await.unwrap();
    let worker = Worker::new(jobs, Arc::new(orchestrator), "one", 1_000);
    worker.poll().await.unwrap();
}

fn telemetry() -> (Held, Arc<Telemetry>) {
    let sink = Held::default();
    let ticker = StepTicker::starting_at(0).ticker();
    let telemetry = Arc::new(Telemetry::new(sink.sink(), ticker));
    (sink, telemetry)
}

#[tokio::test]
async fn a_job_kind_names_the_operation_it_is_measured_under() {
    assert_eq!(measured(JobKind::Export), Operation::Export);
    assert_eq!(measured(JobKind::Import), Operation::Import);
    assert_eq!(measured(JobKind::Reindex), Operation::Reindex);
    assert_eq!(measured(JobKind::BulkDelete), Operation::BulkDelete);
    assert_eq!(measured(JobKind::BulkUpdate), Operation::BulkUpdate);
}

#[tokio::test]
async fn a_finished_job_is_measured_under_its_kind() {
    let (_sink, telemetry) = telemetry();
    let store = seeded().await;
    let orchestrator = Orchestrator::new()
        .with(Arc::new(ExportJob::new(
            store,
            Arc::new(MemoryBulkStore::new()),
        )))
        .reporting(Arc::clone(&telemetry));
    ran(
        orchestrator,
        JobRequest::new(job("e1"), JobKind::Export, r#"{"types":["Patient"]}"#),
    )
    .await;
    assert_eq!(
        telemetry.count(Dimensions::of(Operation::Export, Outcome::Success)),
        1
    );
    assert!(telemetry
        .exposition()
        .contains("fhir_operation_duration_ms_count{operation=\"export\",outcome=\"success\"} 1"));
}

struct Breaking;

#[async_trait::async_trait]
impl fhir_jobs::JobHandler for Breaking {
    fn kind(&self) -> JobKind {
        JobKind::BulkUpdate
    }

    async fn plan(&self, _job: &fhir_jobs::JobContext) -> Result<Vec<fhir_jobs::Unit>, Error> {
        Ok(vec![fhir_jobs::Unit::new("one", "{}")])
    }

    async fn process(
        &self,
        _job: &fhir_jobs::JobContext,
        _unit: &fhir_jobs::Unit,
    ) -> Result<fhir_jobs::UnitOutcome, Error> {
        Err(Error::Internal("the unit could not be run".to_owned()))
    }
}

#[tokio::test]
async fn work_that_could_not_be_run_is_measured_as_a_fault() {
    let (_sink, telemetry) = telemetry();
    let orchestrator = Orchestrator::new()
        .with(Arc::new(Breaking))
        .reporting(Arc::clone(&telemetry));
    ran(
        orchestrator,
        JobRequest::new(job("b1"), JobKind::BulkUpdate, "{}").with_attempts(1),
    )
    .await;
    assert_eq!(
        telemetry.count(Dimensions::of(Operation::BulkUpdate, Outcome::ServerFault)),
        1
    );
    assert!(telemetry
        .exposition()
        .contains("fhir_operation_failure_total{operation=\"bulk_update\"} 1"));
}

#[tokio::test]
async fn a_job_of_an_unregistered_kind_is_measured_too() {
    let (_sink, telemetry) = telemetry();
    let orchestrator = Orchestrator::new().reporting(Arc::clone(&telemetry));
    ran(
        orchestrator,
        JobRequest::new(job("r1"), JobKind::Reindex, "{}"),
    )
    .await;
    assert_eq!(
        telemetry.count(Dimensions::of(Operation::Reindex, Outcome::ClientFault)),
        1
    );
}

#[tokio::test]
async fn nothing_of_the_payload_reaches_the_measurements() {
    let (sink, telemetry) = telemetry();
    let store = seeded().await;
    let orchestrator = Orchestrator::new()
        .with(Arc::new(ImportJob::new(store, fhir_core::FhirVersion::R4)))
        .reporting(Arc::clone(&telemetry));
    let payload = r#"{"resourceType":"Patient","id":"pt-confidential-77","name":[{"family":"Rossignol"}]}"#;
    ran(
        orchestrator,
        JobRequest::new(job("i2"), JobKind::Import, payload),
    )
    .await;
    let mut written = sink.lines();
    written.push(telemetry.exposition());
    let held = written.join("\n");
    for secret in ["pt-confidential-77", "Rossignol", "i2"] {
        assert!(!held.contains(secret), "{secret} reached telemetry");
    }
    assert!(held.contains("operation=import"));
}
