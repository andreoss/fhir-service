use fhir_core::Error;
use fhir_store::{
    JobFilter, JobId, JobKind, JobProgress, JobRequest, JobResult, JobSignal, JobState, JobStore,
    JobLimits, Lease, StepTicker, RETRY_BACKOFF,
};

pub fn job(raw: &str) -> JobId {
    JobId::parse(raw).expect("suite job id is valid")
}

fn queued(id: &str, kind: JobKind) -> JobRequest {
    JobRequest::new(job(id), kind, "{}")
}

pub async fn submission(store: &dyn JobStore) {
    let submitted = store
        .submit(JobRequest::new(job("s1"), JobKind::Export, "{\"types\":[]}"))
        .await
        .unwrap();
    assert_eq!(submitted.state, JobState::Queued);
    assert_eq!(submitted.kind, JobKind::Export);
    assert_eq!(submitted.attempt, 0);
    assert_eq!(submitted.payload.as_deref(), Some("{\"types\":[]}"));

    let read = store.fetch(&job("s1")).await.unwrap();
    assert_eq!(read.id, submitted.id);
    assert_eq!(read.created, submitted.created);

    let repeated = store.submit(queued("s1", JobKind::Import)).await;
    assert!(matches!(repeated, Err(Error::Duplicate(_))), "{repeated:?}");

    let missing = store.fetch(&job("nobody")).await;
    assert!(matches!(missing, Err(Error::NotFound)), "{missing:?}");

    let owned = store
        .submit(JobRequest::new(job("s2"), JobKind::Export, "{}").owned_by("practitioner-1"))
        .await
        .unwrap();
    assert_eq!(owned.owner.as_deref(), Some("practitioner-1"));
    let read = store.fetch(&job("s2")).await.unwrap();
    assert_eq!(read.owner.as_deref(), Some("practitioner-1"));
    assert_eq!(submitted.owner, None);
}

pub async fn claiming(store: &dyn JobStore) {
    store.submit(queued("c1", JobKind::Import)).await.unwrap();
    store.submit(queued("c2", JobKind::Reindex)).await.unwrap();

    let held = store.claim(&Lease::new("one", 1_000)).await.unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].id, job("c1"));
    assert_eq!(held[0].state, JobState::Running);
    assert_eq!(held[0].attempt, 1);
    assert_eq!(held[0].worker.as_deref(), Some("one"));
    assert!(held[0].lease.is_some());

    let other = store.claim(&Lease::new("two", 1_000)).await.unwrap();
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].id, job("c2"));

    let empty = store.claim(&Lease::new("three", 1_000)).await.unwrap();
    assert!(empty.is_empty(), "{empty:?}");
}

pub async fn heartbeats(store: &dyn JobStore) {
    store.submit(queued("h1", JobKind::Export)).await.unwrap();
    let held = store.claim(&Lease::new("one", 1_000)).await.unwrap();
    let first = held[0].lease.expect("a claim holds a lease");

    let signal = store
        .heartbeat(&job("h1"), "one", 5_000, Some(JobProgress::of(2, 8)))
        .await
        .unwrap();
    assert_eq!(signal, JobSignal::Continue);

    let read = store.fetch(&job("h1")).await.unwrap();
    assert_eq!(read.progress.percent(), Some(25));
    assert!(read.lease.unwrap_or_default() >= first);

    let stale = store.heartbeat(&job("h1"), "other", 1_000, None).await;
    assert!(matches!(stale, Err(Error::VersionConflict)), "{stale:?}");
}

pub async fn completion(store: &dyn JobStore) {
    store.submit(queued("f1", JobKind::BulkDelete)).await.unwrap();
    store.claim(&Lease::new("one", 1_000)).await.unwrap();

    let done = store
        .finish(&job("f1"), "one", JobResult::Succeeded("42".to_owned()))
        .await
        .unwrap();
    assert_eq!(done.state, JobState::Completed);
    assert_eq!(done.outcome.as_deref(), Some("42"));
    assert!(done.lease.is_none());
    assert!(done.worker.is_none());

    let again = store
        .finish(&job("f1"), "one", JobResult::Succeeded("0".to_owned()))
        .await;
    assert!(matches!(again, Err(Error::VersionConflict)), "{again:?}");

    let beat = store.heartbeat(&job("f1"), "one", 1_000, None).await;
    assert!(matches!(beat, Err(Error::VersionConflict)), "{beat:?}");
}

pub async fn listing(store: &dyn JobStore) {
    store.submit(queued("l1", JobKind::Import)).await.unwrap();
    store.submit(queued("l2", JobKind::Export)).await.unwrap();
    store.claim(&Lease::new("one", 1_000)).await.unwrap();

    let running = store.list(&JobFilter::in_state(JobState::Running)).await.unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].id, job("l1"));

    let exports = store
        .list(&JobFilter {
            kinds: vec![JobKind::Export],
            states: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].id, job("l2"));

    let every = store.list(&JobFilter::default()).await.unwrap();
    assert_eq!(every.len(), 2);
}

pub async fn cancellation(store: &dyn JobStore) {
    store.submit(queued("k1", JobKind::Export)).await.unwrap();
    store.submit(queued("k2", JobKind::Export)).await.unwrap();

    let stopped = store.cancel(&job("k1")).await.unwrap();
    assert_eq!(stopped.state, JobState::Cancelled);
    assert!(stopped.cancelled);

    let claimed = store.claim(&Lease::new("one", 1_000)).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, job("k2"));

    let running = store.cancel(&job("k2")).await.unwrap();
    assert_eq!(running.state, JobState::Cancelling);
    assert!(running.cancelled);

    store
        .finish(&job("k2"), "one", JobResult::Cancelled)
        .await
        .unwrap();
    assert_eq!(store.fetch(&job("k2")).await.unwrap().state, JobState::Cancelled);

    let ended = store.cancel(&job("k2")).await;
    assert!(matches!(ended, Err(Error::VersionConflict)), "{ended:?}");

    let missing = store.cancel(&job("nobody")).await;
    assert!(matches!(missing, Err(Error::NotFound)), "{missing:?}");
}

pub async fn retries(store: &dyn JobStore, ticker: &StepTicker) {
    store
        .submit(queued("t1", JobKind::Import).with_attempts(2))
        .await
        .unwrap();
    store.claim(&Lease::new("one", 1_000)).await.unwrap();

    let failed = store
        .finish(&job("t1"), "one", JobResult::Failed("first".to_owned()))
        .await
        .unwrap();
    assert_eq!(failed.state, JobState::Queued);
    assert_eq!(failed.attempt, 1);
    assert!(failed.available > ticker.now(), "a retry waits");

    let early = store.claim(&Lease::new("two", 1_000)).await.unwrap();
    assert!(early.is_empty(), "{early:?}");

    ticker.advance(RETRY_BACKOFF * 2);
    let late = store.claim(&Lease::new("two", 1_000)).await.unwrap();
    assert_eq!(late.len(), 1);
    assert_eq!(late[0].attempt, 2);

    let spent = store
        .finish(&job("t1"), "two", JobResult::Failed("second".to_owned()))
        .await
        .unwrap();
    assert_eq!(spent.state, JobState::Failed);
    assert_eq!(spent.outcome.as_deref(), Some("second"));
}

pub async fn recovery(store: &dyn JobStore, ticker: &StepTicker) {
    store.submit(queued("w1", JobKind::Export)).await.unwrap();
    let held = store.claim(&Lease::new("one", 1_000)).await.unwrap();
    assert_eq!(held.len(), 1);

    ticker.advance(1_500);
    let held_still = store.claim(&Lease::new("two", 1_000)).await.unwrap();
    assert!(held_still.is_empty(), "an unexpired claim is not handed on");

    let reclaimed = store.reclaim().await.unwrap();
    assert_eq!(reclaimed, vec![job("w1")]);
    assert_eq!(store.fetch(&job("w1")).await.unwrap().state, JobState::Queued);

    let resumed = store.claim(&Lease::new("two", 1_000)).await.unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].id, job("w1"));
    assert_eq!(resumed[0].attempt, 2);
    assert_eq!(resumed[0].worker.as_deref(), Some("two"));

    let stale = store.heartbeat(&job("w1"), "one", 1_000, None).await;
    assert!(matches!(stale, Err(Error::VersionConflict)), "{stale:?}");

    let done = store
        .finish(&job("w1"), "two", JobResult::Succeeded("resumed".to_owned()))
        .await
        .unwrap();
    assert_eq!(done.state, JobState::Completed);
}

pub async fn correlation(store: &dyn JobStore, ticker: &StepTicker) {
    let started = fhir_core::CorrelationId::fresh();
    let submitted = store
        .submit(
            JobRequest::new(job("k1"), JobKind::Export, "{}")
                .correlated(started.clone())
                .with_attempts(3),
        )
        .await
        .unwrap();
    assert_eq!(submitted.correlation.as_ref(), Some(&started));
    assert_eq!(
        store.fetch(&job("k1")).await.unwrap().correlation.as_ref(),
        Some(&started)
    );

    let held = store.claim(&Lease::new("one", 1_000)).await.unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].correlation.as_ref(), Some(&started));

    ticker.advance(1_500);
    assert_eq!(store.reclaim().await.unwrap(), vec![job("k1")]);

    let resumed = store.claim(&Lease::new("two", 1_000)).await.unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].attempt, 2);
    assert_eq!(
        resumed[0].correlation.as_ref(),
        Some(&started),
        "a resumed attempt keeps the identifier it started with"
    );

    let done = store
        .finish(&job("k1"), "two", JobResult::Succeeded("{}".to_owned()))
        .await
        .unwrap();
    assert_eq!(done.correlation.as_ref(), Some(&started));

    let none = store
        .submit(JobRequest::new(job("k2"), JobKind::Import, "{}"))
        .await
        .unwrap();
    assert_eq!(none.correlation, None);
}

pub async fn exhaustion(store: &dyn JobStore, ticker: &StepTicker) {
    store
        .submit(queued("x1", JobKind::Reindex).with_attempts(1))
        .await
        .unwrap();
    store.claim(&Lease::new("one", 1_000)).await.unwrap();

    ticker.advance(2_000);
    assert_eq!(store.reclaim().await.unwrap(), vec![job("x1")]);

    let record = store.fetch(&job("x1")).await.unwrap();
    assert_eq!(record.state, JobState::Failed);
    assert!(record.outcome.is_some());
    assert!(store.reclaim().await.unwrap().is_empty());
}

pub async fn stopped_while_cancelling(store: &dyn JobStore, ticker: &StepTicker) {
    store.submit(queued("z1", JobKind::Export)).await.unwrap();
    store.claim(&Lease::new("one", 1_000)).await.unwrap();
    store.cancel(&job("z1")).await.unwrap();

    ticker.advance(2_000);
    assert_eq!(store.reclaim().await.unwrap(), vec![job("z1")]);
    assert_eq!(
        store.fetch(&job("z1")).await.unwrap().state,
        JobState::Cancelled
    );
}

pub async fn rejection(store: &dyn JobStore) {
    store
        .submit(queued("j1", JobKind::BulkUpdate).with_attempts(5))
        .await
        .unwrap();
    store.claim(&Lease::new("one", 1_000)).await.unwrap();

    let rejected = store
        .finish(&job("j1"), "one", JobResult::Rejected("no patch".to_owned()))
        .await
        .unwrap();
    assert_eq!(rejected.state, JobState::Failed);
    assert_eq!(rejected.attempt, 1);
    assert_eq!(rejected.outcome.as_deref(), Some("no patch"));
}

pub async fn cancel_signal(store: &dyn JobStore) {
    store.submit(queued("g1", JobKind::BulkDelete)).await.unwrap();
    store.claim(&Lease::new("one", 1_000)).await.unwrap();

    let before = store.heartbeat(&job("g1"), "one", 1_000, None).await.unwrap();
    assert_eq!(before, JobSignal::Continue);

    store.cancel(&job("g1")).await.unwrap();

    let after = store.heartbeat(&job("g1"), "one", 1_000, None).await.unwrap();
    assert_eq!(after, JobSignal::Cancel);

    let stopped = store
        .finish(&job("g1"), "one", JobResult::Cancelled)
        .await
        .unwrap();
    assert_eq!(stopped.state, JobState::Cancelled);
    assert!(stopped.worker.is_none());
    assert!(stopped.lease.is_none());
}

pub async fn defragmentation(store: &dyn JobStore) {
    store.submit(queued("d1", JobKind::Import)).await.unwrap();
    store.submit(queued("d2", JobKind::Import)).await.unwrap();
    store.claim(&Lease::new("one", 1_000)).await.unwrap();
    store
        .finish(&job("d1"), "one", JobResult::Succeeded("done".to_owned()))
        .await
        .unwrap();

    assert_eq!(store.defragment().await.unwrap(), 1);
    let ended = store.fetch(&job("d1")).await.unwrap();
    assert_eq!(ended.payload, None);
    assert_eq!(ended.outcome.as_deref(), Some("done"));
    assert!(store.fetch(&job("d2")).await.unwrap().payload.is_some());

    assert_eq!(store.defragment().await.unwrap(), 0);
}

pub async fn concurrency(store: &dyn JobStore) {
    for slot in 0..4 {
        store
            .submit(queued(&format!("n{slot}"), JobKind::Export))
            .await
            .unwrap();
    }
    let limits = JobLimits::unlimited().running(JobKind::Export, 2);

    let first = store
        .claim(&Lease::new("one", 10_000).with_limit(4).with_limits(limits))
        .await
        .unwrap();
    assert_eq!(first.len(), 2, "the limit did not hold");

    let second = store
        .claim(&Lease::new("two", 10_000).with_limit(4).with_limits(limits))
        .await
        .unwrap();
    assert!(second.is_empty(), "{second:?}");

    store
        .finish(&first[0].id, "one", JobResult::Succeeded("done".to_owned()))
        .await
        .unwrap();

    let freed = store
        .claim(&Lease::new("two", 10_000).with_limit(4).with_limits(limits))
        .await
        .unwrap();
    assert_eq!(freed.len(), 1, "a finished job did not free its place");
}

pub async fn throttling(store: &dyn JobStore, ticker: &StepTicker) {
    for slot in 0..3 {
        store
            .submit(queued(&format!("p{slot}"), JobKind::Reindex))
            .await
            .unwrap();
    }
    let limits = JobLimits::unlimited().every(JobKind::Reindex, 1_000);

    let first = store
        .claim(&Lease::new("one", 10_000).with_limit(3).with_limits(limits))
        .await
        .unwrap();
    assert_eq!(first.len(), 1, "a throttle admits one start at a time");

    let early = store
        .claim(&Lease::new("two", 10_000).with_limit(3).with_limits(limits))
        .await
        .unwrap();
    assert!(early.is_empty(), "{early:?}");

    ticker.advance(1_000);
    let late = store
        .claim(&Lease::new("two", 10_000).with_limit(3).with_limits(limits))
        .await
        .unwrap();
    assert_eq!(late.len(), 1);
}

pub async fn limits_are_per_kind(store: &dyn JobStore) {
    store.submit(queued("q1", JobKind::Export)).await.unwrap();
    store.submit(queued("q2", JobKind::Export)).await.unwrap();
    store.submit(queued("q3", JobKind::Import)).await.unwrap();
    let limits = JobLimits::unlimited().running(JobKind::Export, 1);

    let taken = store
        .claim(&Lease::new("one", 10_000).with_limit(5).with_limits(limits))
        .await
        .unwrap();

    let kinds: Vec<JobKind> = taken.iter().map(|record| record.kind).collect();
    assert_eq!(kinds.len(), 2, "{kinds:?}");
    assert!(kinds.contains(&JobKind::Export));
    assert!(kinds.contains(&JobKind::Import));
}

pub async fn retention(store: &dyn JobStore, ticker: &StepTicker) {
    store.submit(queued("y1", JobKind::Export)).await.unwrap();
    store.submit(queued("y2", JobKind::Export)).await.unwrap();
    store.claim(&Lease::new("one", 10_000)).await.unwrap();
    store
        .finish(&job("y1"), "one", JobResult::Succeeded("done".to_owned()))
        .await
        .unwrap();

    assert_eq!(store.purge(10_000).await.unwrap(), 0, "purged too early");
    assert!(store.fetch(&job("y1")).await.is_ok());

    ticker.advance(10_000);
    assert_eq!(store.purge(10_000).await.unwrap(), 1);

    let gone = store.fetch(&job("y1")).await;
    assert!(matches!(gone, Err(Error::NotFound)), "{gone:?}");
    assert!(store.fetch(&job("y2")).await.is_ok(), "a live job was purged");
    assert_eq!(store.purge(10_000).await.unwrap(), 0);
}

pub async fn handover(store: &dyn JobStore) {
    store.submit(queued("h1", JobKind::Export)).await.unwrap();
    store.submit(queued("h2", JobKind::Export)).await.unwrap();
    let held = store
        .claim(&Lease::new("leaving", 600_000).with_limit(2))
        .await
        .unwrap();
    assert_eq!(held.len(), 2);

    let waiting = store.claim(&Lease::new("staying", 1_000)).await.unwrap();
    assert!(waiting.is_empty(), "a held lease is not handed on by itself");

    let handed = store.hand_over("leaving").await.unwrap();
    assert_eq!(handed.len(), 2, "every held job is handed back");

    let resumed = store
        .claim(&Lease::new("staying", 1_000).with_limit(2))
        .await
        .unwrap();
    assert_eq!(resumed.len(), 2, "handed work is claimable at once");
    for record in &resumed {
        assert_eq!(record.worker.as_deref(), Some("staying"));
    }

    let stale = store.heartbeat(&job("h1"), "leaving", 1_000, None).await;
    assert!(matches!(stale, Err(Error::VersionConflict)), "{stale:?}");

    let none = store.hand_over("leaving").await.unwrap();
    assert!(none.is_empty(), "a worker holding nothing hands nothing back");
}
