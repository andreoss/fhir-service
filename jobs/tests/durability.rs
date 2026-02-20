use async_trait::async_trait;
use fhir_adapter_relational::{Namespace, RelationalJobStore, RelationalStore, DEFAULT_URL, ENV_URL};
use fhir_core::Error;
use fhir_jobs::{JobContext, JobHandler, Orchestrator, Schedule, Unit, UnitOutcome, Watchdog, Worker};
use fhir_store::{
    JobId, JobKind, JobLimits, JobRequest, JobResult, JobState, JobStore, Lease, StepTicker,
    RETRY_BACKOFF,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const LEASE: i64 = 5_000;

fn url() -> String {
    std::env::var(ENV_URL).unwrap_or_else(|_| DEFAULT_URL.to_owned())
}

async fn pool() -> PgPool {
    match PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&url())
        .await
    {
        Ok(pool) => pool,
        Err(error) => panic!(
            "a durable queue is required and none answered: {error}; start the services named in compose.yaml"
        ),
    }
}

struct Queue {
    namespace: Namespace,
    ticker: StepTicker,
    held: PgPool,
}

impl Queue {
    async fn opened(name: &str) -> Queue {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default();
        let namespace =
            Namespace::parse(&format!("t_dur_{name}_{stamp}")).expect("a generated namespace");
        let held = pool().await;
        RelationalStore::new(held.clone(), namespace.clone())
            .migrate()
            .await
            .expect("the schema applies");
        Queue {
            namespace,
            ticker: StepTicker::starting_at(1_000_000),
            held,
        }
    }

    async fn handle(&self) -> Arc<dyn JobStore> {
        Arc::new(
            RelationalJobStore::new(pool().await, self.namespace.clone())
                .with_ticker(self.ticker.ticker()),
        )
    }

    async fn drop_namespace(self) {
        let statement = format!("drop schema if exists {} cascade", self.namespace.as_str());
        let _ = sqlx::raw_sql(&statement).execute(&self.held).await;
    }
}

fn job(raw: &str) -> JobId {
    JobId::parse(raw).expect("a job id")
}

fn queued(raw: &str, kind: JobKind) -> JobRequest {
    JobRequest::new(job(raw), kind, "{}")
}

struct Counting {
    ran: Arc<AtomicUsize>,
    units: usize,
    fail_until: usize,
    watching: Option<Arc<dyn JobStore>>,
    stop_after: usize,
}

impl Counting {
    fn of(units: usize) -> Counting {
        Counting {
            ran: Arc::new(AtomicUsize::new(0)),
            units,
            fail_until: 0,
            watching: None,
            stop_after: usize::MAX,
        }
    }

    fn failing_until(self, attempt: usize) -> Counting {
        Counting {
            fail_until: attempt,
            ..self
        }
    }

    fn stopped_through(self, watching: Arc<dyn JobStore>, after: usize) -> Counting {
        Counting {
            watching: Some(watching),
            stop_after: after,
            ..self
        }
    }

    fn runs(&self) -> usize {
        self.ran.load(Ordering::SeqCst)
    }
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

    async fn process(&self, job: &JobContext, _unit: &Unit) -> Result<UnitOutcome, Error> {
        let ran = self.ran.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(watching) = &self.watching {
            if ran == self.stop_after {
                watching.cancel(&job.id).await?;
            }
        }
        if ran <= self.fail_until {
            return Err(Error::Internal("the unit could not be run".to_owned()));
        }
        Ok(UnitOutcome::handled(1))
    }
}

fn working(jobs: Arc<dyn JobStore>, handler: Arc<Counting>, name: &str) -> Worker {
    Worker::new(
        jobs,
        Arc::new(Orchestrator::new().with(handler)),
        name.to_owned(),
        LEASE,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lease_a_worker_holds_is_read_back_through_another_handle() {
    let queue = Queue::opened("lease").await;
    let claiming = queue.handle().await;
    let reading = queue.handle().await;
    claiming.submit(queued("l1", JobKind::Export)).await.unwrap();

    let held = claiming
        .claim(&Lease::new("one", LEASE))
        .await
        .expect("a job is claimed");

    assert_eq!(held.len(), 1);
    let seen = reading.fetch(&job("l1")).await.expect("the record persists");
    assert_eq!(seen.state, JobState::Running);
    assert_eq!(seen.worker.as_deref(), Some("one"));
    assert_eq!(seen.lease, Some(queue.ticker.now() + LEASE));
    assert_eq!(seen.attempt, 1);
    assert_eq!(seen.started, Some(queue.ticker.now()));
    queue.drop_namespace().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_heartbeat_extends_the_lease_and_a_stranger_is_refused() {
    let queue = Queue::opened("beat").await;
    let holding = queue.handle().await;
    let reading = queue.handle().await;
    holding.submit(queued("h1", JobKind::Export)).await.unwrap();
    holding.claim(&Lease::new("one", LEASE)).await.unwrap();
    queue.ticker.advance(LEASE - 1);

    holding
        .heartbeat(&job("h1"), "one", LEASE, None)
        .await
        .expect("the holder may extend its lease");

    let seen = reading.fetch(&job("h1")).await.unwrap();
    assert_eq!(seen.lease, Some(queue.ticker.now() + LEASE));
    assert_eq!(seen.state, JobState::Running);
    assert_eq!(
        reading.heartbeat(&job("h1"), "two", LEASE, None).await,
        Err(Error::VersionConflict),
        "a worker that holds no lease must not extend one"
    );
    assert_eq!(
        reading
            .finish(&job("h1"), "two", JobResult::Succeeded("{}".to_owned()))
            .await
            .unwrap_err(),
        Error::VersionConflict,
        "a worker that holds no lease must not finish the job"
    );
    queue.drop_namespace().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_whose_lease_ran_out_is_reclaimed_and_finished_by_a_second_worker() {
    let queue = Queue::opened("reclaim").await;
    let stopped = queue.handle().await;
    let sweeping = queue.handle().await;
    let resuming = queue.handle().await;
    let reading = queue.handle().await;
    stopped.submit(queued("r1", JobKind::Export)).await.unwrap();
    stopped.claim(&Lease::new("one", LEASE)).await.unwrap();

    let watchdog = Watchdog::new(Arc::clone(&sweeping))
        .with_schedule(Schedule::default())
        .with_ticker(queue.ticker.ticker());
    assert!(
        watchdog.sweep().await.unwrap().is_empty(),
        "a live lease is not stalled"
    );
    queue.ticker.advance(LEASE + 1);
    assert_eq!(watchdog.sweep().await.unwrap().reclaimed, 1);
    assert_eq!(reading.fetch(&job("r1")).await.unwrap().state, JobState::Queued);
    assert_eq!(
        stopped
            .finish(&job("r1"), "one", JobResult::Succeeded("{}".to_owned()))
            .await
            .unwrap_err(),
        Error::VersionConflict,
        "the stopped worker no longer owns the job"
    );

    let handler = Arc::new(Counting::of(2));
    let worker = working(Arc::clone(&resuming), Arc::clone(&handler), "two");
    assert_eq!(worker.poll().await.unwrap(), 1);

    let seen = reading.fetch(&job("r1")).await.unwrap();
    assert_eq!(seen.state, JobState::Completed);
    assert_eq!(seen.attempt, 2, "the resumed run is the second attempt");
    assert_eq!(handler.runs(), 2, "every unit ran on the second worker");
    let outcome: serde_json::Value = serde_json::from_str(&seen.outcome.unwrap()).unwrap();
    assert_eq!(outcome["handled"], 2);
    queue.drop_namespace().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_attempt_waits_its_backoff_and_then_runs_again() {
    let queue = Queue::opened("retry").await;
    let running = queue.handle().await;
    let reading = queue.handle().await;
    running.submit(queued("t1", JobKind::Export)).await.unwrap();
    let handler = Arc::new(Counting::of(1).failing_until(1));
    let worker = working(Arc::clone(&running), Arc::clone(&handler), "one");

    assert_eq!(worker.poll().await.unwrap(), 1);
    let failed = reading.fetch(&job("t1")).await.unwrap();
    assert_eq!(failed.state, JobState::Queued, "an attempt left means a retry");
    assert_eq!(failed.attempt, 1);
    assert_eq!(
        failed.available,
        queue.ticker.now() + RETRY_BACKOFF,
        "the retry waits one backoff per attempt"
    );

    assert_eq!(
        worker.poll().await.unwrap(),
        0,
        "the retry ran before its backoff elapsed"
    );
    assert_eq!(handler.runs(), 1);

    queue.ticker.advance(RETRY_BACKOFF);
    assert_eq!(worker.poll().await.unwrap(), 1);
    let done = reading.fetch(&job("t1")).await.unwrap();
    assert_eq!(done.state, JobState::Completed);
    assert_eq!(done.attempt, 2);
    assert_eq!(handler.runs(), 2);
    queue.drop_namespace().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_with_no_attempt_left_is_failed_rather_than_queued() {
    let queue = Queue::opened("spent").await;
    let running = queue.handle().await;
    let reading = queue.handle().await;
    running
        .submit(queued("s1", JobKind::Export).with_attempts(2))
        .await
        .unwrap();
    let handler = Arc::new(Counting::of(1).failing_until(usize::MAX));
    let worker = working(Arc::clone(&running), Arc::clone(&handler), "one");

    worker.poll().await.unwrap();
    assert_eq!(reading.fetch(&job("s1")).await.unwrap().state, JobState::Queued);
    queue.ticker.advance(RETRY_BACKOFF);
    worker.poll().await.unwrap();

    let spent = reading.fetch(&job("s1")).await.unwrap();
    assert_eq!(spent.state, JobState::Failed);
    assert_eq!(spent.attempt, 2);
    assert!(
        spent.outcome.unwrap().contains("the unit could not be run"),
        "the last failure is kept"
    );
    queue.ticker.advance(RETRY_BACKOFF * 4);
    assert_eq!(
        worker.poll().await.unwrap(),
        0,
        "a spent job is never claimed again"
    );
    assert_eq!(handler.runs(), 2);
    queue.drop_namespace().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_recorded_through_another_handle_ends_the_running_job() {
    let queue = Queue::opened("stop").await;
    let running = queue.handle().await;
    let stopping = queue.handle().await;
    let reading = queue.handle().await;
    running.submit(queued("c1", JobKind::Export)).await.unwrap();
    let handler = Arc::new(Counting::of(10).stopped_through(Arc::clone(&stopping), 3));
    let worker = working(Arc::clone(&running), handler.clone(), "one");

    worker.poll().await.unwrap();

    let seen = reading.fetch(&job("c1")).await.unwrap();
    assert_eq!(seen.state, JobState::Cancelled);
    assert!(seen.cancelled);
    assert_eq!(seen.worker, None);
    assert_eq!(seen.lease, None);
    assert_eq!(
        handler.runs(),
        3,
        "the job ran past the heartbeat that carried the stop"
    );
    queue.drop_namespace().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queued_job_stopped_before_it_starts_is_never_claimed() {
    let queue = Queue::opened("stopq").await;
    let running = queue.handle().await;
    let stopping = queue.handle().await;
    let reading = queue.handle().await;
    running.submit(queued("c2", JobKind::Export)).await.unwrap();
    stopping.cancel(&job("c2")).await.expect("a queued job stops");

    let handler = Arc::new(Counting::of(1));
    let worker = working(Arc::clone(&running), Arc::clone(&handler), "one");
    assert_eq!(worker.poll().await.unwrap(), 0);

    assert_eq!(
        reading.fetch(&job("c2")).await.unwrap().state,
        JobState::Cancelled
    );
    assert_eq!(handler.runs(), 0);
    queue.drop_namespace().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_runs_no_more_at_once_than_the_limit_its_kind_carries() {
    let queue = Queue::opened("cap").await;
    let running = queue.handle().await;
    let reading = queue.handle().await;
    for slot in 0..3 {
        running
            .submit(queued(&format!("k{slot}"), JobKind::Export))
            .await
            .unwrap();
    }
    let handler = Arc::new(Counting::of(1));
    let worker = working(Arc::clone(&running), Arc::clone(&handler), "one")
        .with_batch(3)
        .with_limits(JobLimits::unlimited().running(JobKind::Export, 2));

    assert_eq!(worker.poll().await.unwrap(), 2, "the limit did not hold");

    let queued_now = reading
        .list(&fhir_store::JobFilter::in_state(JobState::Queued))
        .await
        .unwrap();
    assert_eq!(queued_now.len(), 1);
    assert_eq!(queued_now[0].id, job("k2"));
    assert_eq!(handler.runs(), 2);
    queue.drop_namespace().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kind_starts_no_more_often_than_its_gap() {
    let queue = Queue::opened("gap").await;
    let running = queue.handle().await;
    let reading = queue.handle().await;
    running.submit(queued("g0", JobKind::Export)).await.unwrap();
    running.submit(queued("g1", JobKind::Export)).await.unwrap();
    let handler = Arc::new(Counting::of(1));
    let worker = working(Arc::clone(&running), Arc::clone(&handler), "one")
        .with_batch(2)
        .with_limits(JobLimits::unlimited().every(JobKind::Export, 10_000));

    assert_eq!(worker.poll().await.unwrap(), 1, "the gap admits one start");
    assert_eq!(worker.poll().await.unwrap(), 0, "the gap had not elapsed");
    queue.ticker.advance(9_999);
    assert_eq!(worker.poll().await.unwrap(), 0, "the gap had not elapsed");
    queue.ticker.advance(1);
    assert_eq!(worker.poll().await.unwrap(), 1);

    assert_eq!(handler.runs(), 2);
    for id in ["g0", "g1"] {
        assert_eq!(
            reading.fetch(&job(id)).await.unwrap().state,
            JobState::Completed
        );
    }
    queue.drop_namespace().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopping_worker_hands_its_work_to_the_queue_it_shares() {
    let queue = Queue::opened("hand").await;
    let leaving = queue.handle().await;
    let reading = queue.handle().await;
    leaving.submit(queued("d1", JobKind::Export)).await.unwrap();
    let worker = Worker::per_instance(Arc::clone(&leaving), Arc::new(Orchestrator::new()), LEASE);
    leaving
        .claim(&Lease::new(worker.name().to_owned(), LEASE))
        .await
        .unwrap();

    assert_eq!(worker.stopping().await.unwrap(), 1);

    let seen = reading.fetch(&job("d1")).await.unwrap();
    assert_eq!(seen.state, JobState::Queued);
    assert_eq!(seen.worker, None);
    assert_eq!(seen.lease, None);
    assert_eq!(
        seen.available,
        queue.ticker.now(),
        "handed work waits for no backoff"
    );
    queue.drop_namespace().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_queue_reports_the_engine_it_cannot_reach() {
    let queue = Queue::opened("health").await;
    let live = queue.handle().await;
    assert_eq!(live.health().await, Ok(()));

    let closing = pool().await;
    let gone: Arc<dyn JobStore> =
        Arc::new(RelationalJobStore::new(closing.clone(), queue.namespace.clone()));
    closing.close().await;

    assert!(
        gone.health().await.is_err(),
        "a queue whose engine is gone must not answer healthy"
    );
    queue.drop_namespace().await;
}
