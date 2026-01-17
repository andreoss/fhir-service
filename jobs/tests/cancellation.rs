use async_trait::async_trait;
use fhir_adapter_memory::MemoryJobStore;
use fhir_core::Error;
use fhir_jobs::{JobContext, JobHandler, Orchestrator, Unit, UnitOutcome, Worker};
use fhir_store::{
    JobId, JobKind, JobRequest, JobState, JobStore, StepTicker,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Counting {
    jobs: Arc<MemoryJobStore>,
    id: JobId,
    units: usize,
    run: Arc<AtomicUsize>,
    stop_after: usize,
}

#[async_trait]
impl JobHandler for Counting {
    fn kind(&self) -> JobKind {
        JobKind::Export
    }

    async fn plan(&self, _job: &JobContext) -> Result<Vec<Unit>, Error> {
        Ok((0..self.units)
            .map(|position| Unit::new(format!("unit {position}"), String::new()))
            .collect())
    }

    async fn process(&self, _job: &JobContext, _unit: &Unit) -> Result<UnitOutcome, Error> {
        let ran = self.run.fetch_add(1, Ordering::SeqCst) + 1;
        if ran == self.stop_after {
            self.jobs.cancel(&self.id).await?;
        }
        Ok(UnitOutcome::handled(1))
    }
}

#[tokio::test]
async fn a_stop_asked_for_mid_flight_ends_the_job_at_the_next_heartbeat() {
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let id = JobId::parse("c-1").unwrap();
    let run = Arc::new(AtomicUsize::new(0));
    jobs.submit(JobRequest::new(id.clone(), JobKind::Export, "{}"))
        .await
        .unwrap();
    let handler = Arc::new(Counting {
        jobs: Arc::clone(&jobs),
        id: id.clone(),
        units: 10,
        run: Arc::clone(&run),
        stop_after: 3,
    });
    let orchestrator = Arc::new(Orchestrator::new().with(handler));
    let worker = Worker::new(Arc::clone(&jobs) as Arc<dyn JobStore>, orchestrator, "one", 5_000);

    worker.poll().await.unwrap();

    let record = jobs.fetch(&id).await.unwrap();
    assert_eq!(record.state, JobState::Cancelled);
    assert_eq!(
        run.load(Ordering::SeqCst),
        3,
        "the job ran past the heartbeat that carried the stop"
    );
    assert!(record.worker.is_none());
    assert!(record.lease.is_none());
}

#[tokio::test]
async fn a_job_not_stopped_runs_every_unit() {
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let id = JobId::parse("c-2").unwrap();
    let run = Arc::new(AtomicUsize::new(0));
    jobs.submit(JobRequest::new(id.clone(), JobKind::Export, "{}"))
        .await
        .unwrap();
    let handler = Arc::new(Counting {
        jobs: Arc::clone(&jobs),
        id: id.clone(),
        units: 4,
        run: Arc::clone(&run),
        stop_after: usize::MAX,
    });
    let orchestrator = Arc::new(Orchestrator::new().with(handler));
    let worker = Worker::new(Arc::clone(&jobs) as Arc<dyn JobStore>, orchestrator, "one", 5_000);

    worker.poll().await.unwrap();

    assert_eq!(jobs.fetch(&id).await.unwrap().state, JobState::Completed);
    assert_eq!(run.load(Ordering::SeqCst), 4);
}
