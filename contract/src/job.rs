use fhir_core::Error;
use fhir_store::{
    JobFilter, JobId, JobKind, JobProgress, JobRequest, JobResult, JobSignal, JobState, JobStore,
    Lease,
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
