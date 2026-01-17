use async_trait::async_trait;
use fhir_adapter_memory::MemoryJobStore;
use fhir_core::Error;
use fhir_jobs::{JobContext, JobHandler, Orchestrator, Unit, UnitOutcome, Worker};
use fhir_store::{JobId, JobKind, JobRequest, JobState, JobStore, StepTicker};
use std::sync::{Arc, Mutex};

struct Recording {
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl JobHandler for Recording {
    fn kind(&self) -> JobKind {
        JobKind::Export
    }

    async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("plan {} {}", job.id.as_str(), job.submitted));
        Ok(vec![Unit::new("only", String::new())])
    }

    async fn process(&self, job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("process {} {}", job.id.as_str(), unit.label));
        Ok(UnitOutcome::handled(1))
    }
}

#[tokio::test]
async fn a_handler_reads_the_job_id_and_the_submission_instant() {
    let ticker = StepTicker::starting_at(7_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let id = JobId::parse("ctx-1").unwrap();
    jobs.submit(JobRequest::new(id.clone(), JobKind::Export, "{}"))
        .await
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let orchestrator = Arc::new(Orchestrator::new().with(Arc::new(Recording {
        seen: Arc::clone(&seen),
    })));
    ticker.advance(5_000);
    let worker = Worker::new(
        Arc::clone(&jobs) as Arc<dyn JobStore>,
        orchestrator,
        "one",
        5_000,
    );
    worker.poll().await.unwrap();

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen, vec!["plan ctx-1 7000", "process ctx-1 only"]);
    assert_eq!(jobs.fetch(&id).await.unwrap().state, JobState::Completed);
}
