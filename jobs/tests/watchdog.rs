use fhir_adapter_memory::MemoryJobStore;
use fhir_jobs::{Schedule, Watchdog};
use fhir_store::{JobId, JobKind, JobRequest, JobResult, JobState, JobStore, Lease, StepTicker};
use std::sync::Arc;

fn job(raw: &str) -> JobId {
    JobId::parse(raw).expect("test job id is valid")
}

fn watched(jobs: &Arc<MemoryJobStore>, ticker: &StepTicker, schedule: Schedule) -> Watchdog {
    Watchdog::new(Arc::clone(jobs) as Arc<dyn JobStore>)
        .with_schedule(schedule)
        .with_ticker(ticker.ticker())
}

#[tokio::test]
async fn a_stalled_job_returns_to_the_queue_without_an_operator() {
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let watchdog = watched(&jobs, &ticker, Schedule::default());
    jobs.submit(JobRequest::new(job("s1"), JobKind::Export, "{}"))
        .await
        .unwrap();
    jobs.claim(&Lease::new("one", 1_000)).await.unwrap();

    assert!(watchdog.sweep().await.unwrap().is_empty());

    ticker.advance(6_000);
    let swept = watchdog.sweep().await.unwrap();

    assert_eq!(swept.reclaimed, 1);
    assert_eq!(
        jobs.fetch(&job("s1")).await.unwrap().state,
        JobState::Queued
    );
}

#[tokio::test]
async fn each_sweep_waits_for_its_own_schedule() {
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let schedule = Schedule {
        stalled: 1_000,
        defragment: 10_000,
        purge: 1_000_000,
        retention: fhir_jobs::RETENTION,
    };
    let watchdog = watched(&jobs, &ticker, schedule);
    jobs.submit(JobRequest::new(job("s2"), JobKind::Import, "{}"))
        .await
        .unwrap();
    jobs.claim(&Lease::new("one", 60_000)).await.unwrap();
    jobs.finish(&job("s2"), "one", JobResult::Succeeded("done".to_owned()))
        .await
        .unwrap();

    let first = watchdog.sweep().await.unwrap();
    assert_eq!(first.compacted, 1);

    jobs.submit(JobRequest::new(job("s3"), JobKind::Import, "{}"))
        .await
        .unwrap();
    jobs.claim(&Lease::new("one", 60_000)).await.unwrap();
    jobs.finish(&job("s3"), "one", JobResult::Succeeded("done".to_owned()))
        .await
        .unwrap();

    ticker.advance(2_000);
    let early = watchdog.sweep().await.unwrap();
    assert_eq!(early.compacted, 0, "compaction ran before it was due");

    ticker.advance(10_000);
    let late = watchdog.sweep().await.unwrap();
    assert_eq!(late.compacted, 1);
    assert!(jobs.fetch(&job("s3")).await.unwrap().payload.is_none());
}

#[tokio::test]
async fn a_sweep_leaves_a_job_whose_lease_still_runs_where_it_stands() {
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let watchdog = watched(&jobs, &ticker, Schedule::default());
    jobs.submit(JobRequest::new(job("n1"), JobKind::Export, r#"{"a":1}"#))
        .await
        .unwrap();
    jobs.claim(&Lease::new("one", 60_000)).await.unwrap();
    let before = jobs.fetch(&job("n1")).await.unwrap();

    let swept = watchdog.sweep().await.unwrap();

    assert!(swept.is_empty(), "{swept:?}");
    let after = jobs.fetch(&job("n1")).await.unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(after.worker, before.worker);
    assert_eq!(after.lease, before.lease);
    assert_eq!(after.attempt, before.attempt);
    assert_eq!(
        after.payload, before.payload,
        "a live job keeps its request"
    );
    assert_eq!(after.updated, before.updated);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_limit_holds_when_many_workers_claim_at_once() {
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    for slot in 0..12 {
        jobs.submit(JobRequest::new(
            job(&format!("load-{slot}")),
            JobKind::BulkUpdate,
            "{}",
        ))
        .await
        .unwrap();
    }
    let limits = fhir_store::JobLimits::unlimited().running(JobKind::BulkUpdate, 3);

    let mut claiming = Vec::new();
    for worker in 0..8 {
        let jobs = Arc::clone(&jobs);
        claiming.push(tokio::spawn(async move {
            let lease = Lease::new(format!("w{worker}"), 60_000)
                .with_limit(12)
                .with_limits(limits);
            jobs.claim(&lease).await.unwrap().len()
        }));
    }
    let mut taken = 0;
    for task in claiming {
        taken += task.await.unwrap();
    }

    assert_eq!(taken, 3, "the limit did not hold under load");
    let running = jobs
        .list(&fhir_store::JobFilter::in_state(JobState::Running))
        .await
        .unwrap();
    assert_eq!(running.len(), 3);
}

#[tokio::test]
async fn an_ended_job_is_purged_on_schedule_without_an_operator() {
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let schedule = Schedule {
        stalled: 1_000,
        defragment: 1_000,
        purge: 5_000,
        retention: 20_000,
    };
    let watchdog = watched(&jobs, &ticker, schedule);
    jobs.submit(JobRequest::new(job("r1"), JobKind::Export, "{}"))
        .await
        .unwrap();
    jobs.claim(&Lease::new("one", 60_000)).await.unwrap();
    jobs.finish(&job("r1"), "one", JobResult::Succeeded("done".to_owned()))
        .await
        .unwrap();

    assert_eq!(watchdog.sweep().await.unwrap().purged, 0);

    ticker.advance(10_000);
    assert_eq!(
        watchdog.sweep().await.unwrap().purged,
        0,
        "purged too early"
    );
    assert!(jobs.fetch(&job("r1")).await.is_ok());

    ticker.advance(20_000);
    let swept = watchdog.sweep().await.unwrap();

    assert_eq!(swept.purged, 1);
    assert!(jobs.fetch(&job("r1")).await.is_err());
}

#[tokio::test]
async fn a_job_still_running_is_never_purged() {
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let schedule = Schedule {
        stalled: 1_000_000,
        defragment: 1_000_000,
        purge: 1,
        retention: 0,
    };
    let watchdog = watched(&jobs, &ticker, schedule);
    jobs.submit(JobRequest::new(job("r2"), JobKind::Export, "{}"))
        .await
        .unwrap();
    jobs.claim(&Lease::new("one", 1_000_000)).await.unwrap();

    ticker.advance(100_000);
    let swept = watchdog.sweep().await.unwrap();

    assert_eq!(swept.purged, 0);
    assert_eq!(
        jobs.fetch(&job("r2")).await.unwrap().state,
        JobState::Running
    );
}
