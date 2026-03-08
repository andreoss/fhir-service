use fhir_adapter_memory::{MemoryBulkStore, MemoryJobStore, MemoryStore};
use fhir_jobs::{BulkUpdateJob, Orchestrator, Worker};
use fhir_store::{
    BulkStore, JobId, JobKind, JobRecord, JobRequest, JobState, JobStore, ResourceStore, StepTicker,
};
use fhir_store_contract::fixture::{id, observation, patient};
use serde_json::Value;
use std::sync::Arc;

fn job(raw: &str) -> JobId {
    JobId::parse(raw).expect("test job id is valid")
}

fn sink() -> Arc<MemoryBulkStore> {
    Arc::new(MemoryBulkStore::new())
}

async fn seeded() -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .create(observation("o1", "code-1", 3.0, "Patient/p1"))
        .await
        .unwrap();
    store
}

async fn ran(
    store: Arc<dyn ResourceStore>,
    sink: Arc<dyn BulkStore>,
    id: JobId,
    payload: &str,
) -> JobRecord {
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let orchestrator = Orchestrator::new().with(Arc::new(BulkUpdateJob::new(store, sink)));
    jobs.submit(JobRequest::new(id.clone(), JobKind::BulkUpdate, payload))
        .await
        .unwrap();
    let worker = Worker::new(
        Arc::clone(&jobs) as Arc<dyn JobStore>,
        Arc::new(orchestrator),
        "one",
        1_000,
    );
    assert_eq!(worker.poll().await.unwrap(), 1);
    jobs.fetch(&id).await.unwrap()
}

fn outcome_of(record: &JobRecord) -> Value {
    serde_json::from_str(record.outcome.as_deref().expect("a report is recorded"))
        .expect("the report is an object")
}

async fn body_of(store: &dyn ResourceStore, held: &str) -> Value {
    let kind = match held.starts_with('o') {
        true => "Observation",
        false => "Patient",
    };
    let entry = store
        .read(&fhir_core::ResourceKey::new(
            kind.parse().unwrap(),
            id(held),
        ))
        .await
        .expect("the resource is held");
    serde_json::from_slice(entry.raw()).expect("a stored resource is json")
}

#[tokio::test]
async fn a_patch_reaches_every_resource_of_the_named_type() {
    let store = seeded().await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("u1"),
        r#"{"_type":["Patient"],"patch":[{"op":"replace","path":"/active","value":false}]}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let outcome = outcome_of(&record);
    assert_eq!(outcome["handled"], 2);
    assert_eq!(outcome["Patient"]["patched"], 2);
    assert_eq!(body_of(store.as_ref(), "p1").await["active"], false);
    assert_eq!(body_of(store.as_ref(), "p2").await["active"], false);
    assert_eq!(body_of(store.as_ref(), "o1").await["status"], "final");
    assert_eq!(
        store
            .read(&fhir_core::ResourceKey::new(
                "Patient".parse().unwrap(),
                id("p1")
            ))
            .await
            .unwrap()
            .version_id()
            .as_str(),
        "2"
    );
}

#[tokio::test]
async fn progress_reaches_every_unit_and_ends_at_the_whole() {
    let store = seeded().await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("u2"),
        r#"{"patch":[{"op":"add","path":"/language","value":"en"}]}"#,
    )
    .await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["units"], 2);
    assert_eq!(outcome["handled"], 3);
    assert_eq!(record.progress.done, 2);
    assert_eq!(record.progress.total, Some(2));
    assert_eq!(record.progress.percent(), Some(100));
}

#[tokio::test]
async fn a_patch_that_changes_nothing_creates_no_version() {
    let store = seeded().await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("u3"),
        r#"{"_type":["Patient"],"patch":[{"op":"replace","path":"/active","value":true}]}"#,
    )
    .await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["handled"], 0);
    assert_eq!(outcome["unchanged"], 2);
    assert_eq!(outcome["Patient"]["unchanged"], 2);
    assert_eq!(
        store
            .read(&fhir_core::ResourceKey::new(
                "Patient".parse().unwrap(),
                id("p1")
            ))
            .await
            .unwrap()
            .version_id()
            .as_str(),
        "1"
    );
}

#[tokio::test]
async fn a_resource_the_patch_is_refused_by_is_itemised_in_a_file() {
    let store = seeded().await;
    store
        .update(patient("p2", "Rivers", false), None)
        .await
        .unwrap();
    let files = sink();

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        Arc::clone(&files) as Arc<dyn BulkStore>,
        job("u4"),
        r#"{"_type":["Patient"],"patch":[{"op":"test","path":"/active","value":true},{"op":"replace","path":"/active","value":false}]}"#,
    )
    .await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["handled"], 1);
    let failures = outcome["failures"].as_array().expect("failures are listed");
    assert_eq!(failures.len(), 1);
    assert!(failures[0].as_str().unwrap().contains("p2"), "{failures:?}");

    let written = files.list(&job("u4")).await.unwrap();
    let failure = written
        .iter()
        .find(|file| file.name == "Patient-failures.ndjson")
        .expect("a failure file is written");
    assert_eq!(failure.count, 1);
    let body = files.read(&job("u4"), &failure.name).await.unwrap();
    let rendered = String::from_utf8(body).expect("a failure file is text");
    assert!(rendered.contains("OperationOutcome"), "{rendered}");
    assert_eq!(body_of(store.as_ref(), "p1").await["active"], false);
    assert_eq!(body_of(store.as_ref(), "p2").await["active"], false);
}

#[tokio::test]
async fn a_path_patch_is_applied_through_the_same_reader() {
    let store = seeded().await;
    let patch = serde_json::json!({
        "_type": ["Patient"],
        "patch": {
            "resourceType": "Parameters",
            "parameter": [{
                "name": "operation",
                "part": [
                    {"name": "type", "valueCode": "replace"},
                    {"name": "path", "valueString": "Patient.active"},
                    {"name": "value", "valueBoolean": false}
                ]
            }]
        }
    });

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("u5"),
        &patch.to_string(),
    )
    .await;

    assert_eq!(outcome_of(&record)["handled"], 2);
    assert_eq!(body_of(store.as_ref(), "p1").await["active"], false);
}

#[tokio::test]
async fn a_maximum_count_caps_what_one_update_patches() {
    let store = seeded().await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("u6"),
        r#"{"_type":["Patient"],"_maxCount":1,"patch":[{"op":"replace","path":"/active","value":false}]}"#,
    )
    .await;

    assert_eq!(outcome_of(&record)["handled"], 1);
    let first = body_of(store.as_ref(), "p1").await;
    let second = body_of(store.as_ref(), "p2").await;
    assert_eq!(first["active"], false, "the patched one is the first held");
    assert_eq!(
        second["active"], true,
        "the one beyond the cap is untouched"
    );
    assert_eq!(
        store
            .read(&fhir_core::ResourceKey::new(
                "Patient".parse().unwrap(),
                id("p2")
            ))
            .await
            .unwrap()
            .version_id()
            .as_str(),
        "1",
        "a resource beyond the cap gains no version"
    );
    assert_eq!(first["name"][0]["family"], "Stone", "nothing else moved");
    assert_eq!(second["name"][0]["family"], "Rivers");
}

#[tokio::test]
async fn an_excluded_type_is_left_as_it_stands() {
    let store = seeded().await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("u7"),
        r#"{"_exclude":["Observation"],"patch":[{"op":"add","path":"/language","value":"en"}]}"#,
    )
    .await;

    let outcome = outcome_of(&record);
    assert_eq!(outcome["units"], 1);
    assert_eq!(outcome["handled"], 2);
    assert_eq!(body_of(store.as_ref(), "o1").await["language"], Value::Null);
}

#[tokio::test]
async fn an_update_carrying_no_patch_is_rejected() {
    let store = seeded().await;

    let record = ran(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        sink(),
        job("u8"),
        r#"{"_type":["Patient"]}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Failed);
    assert!(
        record
            .outcome
            .as_deref()
            .unwrap_or_default()
            .contains("patch"),
        "{:?}",
        record.outcome
    );
}
