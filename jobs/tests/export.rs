use fhir_adapter_memory::{MemoryBulkStore, MemoryJobStore, MemoryStore};
use fhir_core::FhirInstant;
use fhir_jobs::{ExportJob, ExportRequest, Orchestrator, Worker};
use fhir_store::{
    BulkStore, JobId, JobKind, JobRecord, JobRequest, JobState, JobStore, ResourceStore, StepTicker,
};
use fhir_store_contract::fixture::{observation, patient};
use serde_json::Value;
use std::sync::{Arc, Mutex};

fn provenance(id: &str, recorded: &str, target: &str) -> fhir_core::ResourceEnvelope {
    fhir_store_contract::fixture::envelope(
        "Provenance",
        id,
        &format!(r#""recorded":"{recorded}","target":[{{"reference":"{target}"}}]"#),
    )
}

fn job(raw: &str) -> JobId {
    JobId::parse(raw).expect("test job id is valid")
}

struct Hand {
    now: Mutex<String>,
}

fn clock(hand: &Arc<Hand>) -> fhir_store::Clock {
    let hand = Arc::clone(hand);
    Arc::new(move || {
        FhirInstant::parse(&hand.now.lock().unwrap().clone()).expect("a written instant")
    })
}

fn at(hand: &Arc<Hand>, instant: &str) {
    *hand.now.lock().unwrap() = instant.to_owned();
}

async fn ran(
    store: Arc<MemoryStore>,
    sink: Arc<MemoryBulkStore>,
    id: JobId,
    payload: &str,
) -> JobRecord {
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    jobs.submit(JobRequest::new(id.clone(), JobKind::Export, payload))
        .await
        .unwrap();
    let orchestrator = Arc::new(Orchestrator::new().with(Arc::new(ExportJob::new(
        store as Arc<dyn ResourceStore>,
        sink as Arc<dyn BulkStore>,
    ))));
    let worker = Worker::new(
        Arc::clone(&jobs) as Arc<dyn JobStore>,
        orchestrator,
        "one",
        5_000,
    );
    worker.poll().await.unwrap();
    jobs.fetch(&id).await.unwrap()
}

fn rows(body: &[u8]) -> Vec<Value> {
    String::from_utf8(body.to_vec())
        .expect("ndjson is text")
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("every row is one resource"))
        .collect()
}

#[tokio::test]
async fn a_system_export_writes_every_resource_to_one_file_of_its_type() {
    let hand = Arc::new(Hand {
        now: Mutex::new("2026-09-06T04:00:00.000Z".to_owned()),
    });
    let store = Arc::new(MemoryStore::with_clock(clock(&hand)));
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .create(observation("o1", "code-1", 3.0, "Patient/p1"))
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());
    at(&hand, "2026-09-06T05:00:00.000Z");

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("x1"),
        r#"{"scope":"system","_till":"2026-09-06T05:00:00.000Z"}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let outcome: Value = serde_json::from_str(&record.outcome.unwrap()).unwrap();
    assert_eq!(outcome["handled"], 3);
    let files = sink.list(&job("x1")).await.unwrap();
    let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, vec!["Observation.ndjson", "Patient.ndjson"]);
    let patients = rows(&sink.read(&job("x1"), "Patient.ndjson").await.unwrap());
    let ids: Vec<&str> = patients
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["p1", "p2"]);
    assert_eq!(files[1].kind, "Patient");
    assert_eq!(files[1].count, 2);
}

#[tokio::test]
async fn an_export_reads_a_fixed_point_and_carries_every_resource_once() {
    let hand = Arc::new(Hand {
        now: Mutex::new("2026-09-06T04:00:00.000Z".to_owned()),
    });
    let store = Arc::new(MemoryStore::with_clock(clock(&hand)));
    store.create(patient("p1", "Stone", true)).await.unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    at(&hand, "2026-09-06T06:00:00.000Z");
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .update(patient("p1", "Fields", true), None)
        .await
        .unwrap();

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("x2"),
        r#"{"scope":"system","_type":["Patient"],"_till":"2026-09-06T05:00:00.000Z"}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let patients = rows(&sink.read(&job("x2"), "Patient.ndjson").await.unwrap());
    assert_eq!(patients.len(), 1);
    assert_eq!(patients[0]["id"], "p1");
    assert_eq!(patients[0]["name"][0]["family"], "Stone");
    assert_eq!(patients[0]["meta"]["versionId"], "1");
}

#[tokio::test]
async fn a_deleted_resource_leaves_the_export() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .delete(&fhir_store_contract::fixture::id("p2"))
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("x3"),
        r#"{"scope":"system","_type":["Patient"]}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let patients = rows(&sink.read(&job("x3"), "Patient.ndjson").await.unwrap());
    assert_eq!(patients.len(), 1);
    assert_eq!(patients[0]["id"], "p1");
}

#[tokio::test]
async fn a_patient_export_carries_the_compartment_and_a_group_export_its_members() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .create(observation("o1", "code-1", 3.0, "Patient/p1"))
        .await
        .unwrap();
    store
        .create(observation("o2", "code-2", 4.0, "Patient/p2"))
        .await
        .unwrap();
    let group = fhir_store_contract::fixture::envelope(
        "Group",
        "g1",
        r#""member":[{"entity":{"reference":"Patient/p1"}}]"#,
    );
    store.create(group).await.unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let all = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("x4"),
        r#"{"scope":"patient"}"#,
    )
    .await;
    assert_eq!(all.state, JobState::Completed);
    let listed: Vec<String> = sink
        .list(&job("x4"))
        .await
        .unwrap()
        .into_iter()
        .map(|file| file.name)
        .collect();
    
    
    
    
    assert_eq!(
        listed,
        vec!["Group.ndjson", "Observation.ndjson", "Patient.ndjson"]
    );
    assert_eq!(
        rows(&sink.read(&job("x4"), "Patient.ndjson").await.unwrap()).len(),
        2
    );

    let one = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("x5"),
        r#"{"scope":"group","id":"g1"}"#,
    )
    .await;
    assert_eq!(one.state, JobState::Completed);
    let patients = rows(&sink.read(&job("x5"), "Patient.ndjson").await.unwrap());
    assert_eq!(patients.len(), 1);
    assert_eq!(patients[0]["id"], "p1");
    let observations = rows(&sink.read(&job("x5"), "Observation.ndjson").await.unwrap());
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0]["id"], "o1");
}

#[tokio::test]
async fn a_type_filter_narrows_what_the_export_carries() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .create(observation("o1", "code-1", 3.0, "Patient/p1"))
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("f1"),
        r#"{"scope":"system","_type":["Patient","Observation"],"_typeFilter":["Patient?family=Stone"]}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let patients = rows(&sink.read(&job("f1"), "Patient.ndjson").await.unwrap());
    assert_eq!(patients.len(), 1);
    assert_eq!(patients[0]["id"], "p1");
    let observations = rows(&sink.read(&job("f1"), "Observation.ndjson").await.unwrap());
    assert_eq!(observations.len(), 1);
}

#[tokio::test]
async fn a_window_keeps_what_changed_inside_it() {
    let hand = Arc::new(Hand {
        now: Mutex::new("2026-09-06T04:00:00.000Z".to_owned()),
    });
    let store = Arc::new(MemoryStore::with_clock(clock(&hand)));
    store.create(patient("p1", "Stone", true)).await.unwrap();
    at(&hand, "2026-09-06T08:00:00.000Z");
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("f2"),
        r#"{"scope":"system","_type":["Patient"],"_since":"2026-09-06T06:00:00.000Z","_till":"2026-09-06T10:00:00.000Z"}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let patients = rows(&sink.read(&job("f2"), "Patient.ndjson").await.unwrap());
    assert_eq!(patients.len(), 1);
    assert_eq!(patients[0]["id"], "p2");
}

#[tokio::test]
async fn a_container_names_the_files_and_an_unknown_format_is_refused() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let named = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("f3"),
        r#"{"scope":"system","_type":["Patient"],"_container":"nightly","_outputFormat":"application/fhir+ndjson"}"#,
    )
    .await;
    assert_eq!(named.state, JobState::Completed);
    let listed: Vec<String> = sink
        .list(&job("f3"))
        .await
        .unwrap()
        .into_iter()
        .map(|file| file.name)
        .collect();
    assert_eq!(listed, vec!["nightly/Patient.ndjson"]);

    let refused = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("f4"),
        r#"{"scope":"system","_outputFormat":"text/csv"}"#,
    )
    .await;
    assert_eq!(refused.state, JobState::Failed);
    assert!(refused.outcome.unwrap().contains("_outputFormat"));
}

#[tokio::test]
async fn a_filter_the_server_does_not_implement_is_refused() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("f5"),
        r#"{"scope":"system","_type":["Patient"],"_typeFilter":["Patient?nonesuch=1"]}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Failed);
    assert!(record.outcome.unwrap().contains("nonesuch"));
}

fn config(id: &str, redacted: &[&str]) -> fhir_core::ResourceEnvelope {
    let rules: Vec<String> = redacted
        .iter()
        .map(|path| format!(r#"{{"name":"redact","valueString":"{path}"}}"#))
        .collect();
    fhir_store_contract::fixture::envelope(
        "Basic",
        id,
        &format!(r#""parameter":[{}]"#, rules.join(",")),
    )
}

#[tokio::test]
async fn an_anonymised_export_redacts_what_its_configuration_names() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store
        .create(config("anon-1", &["Patient.name", "Patient.birthDate"]))
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("a1"),
        r#"{"scope":"system","_type":["Patient"],"_anonymizationConfig":"anon-1","_anonymizationConfigEtag":"1","_anonymizationConfigCollectionReference":"Basic"}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let outcome: Value = serde_json::from_str(&record.outcome.unwrap()).unwrap();
    assert_eq!(outcome["anonymized"], true);
    assert_eq!(outcome["anonymizationConfig"], "Basic/anon-1");
    assert_eq!(outcome["anonymizationConfigEtag"], "1");
    let patients = rows(&sink.read(&job("a1"), "Patient.ndjson").await.unwrap());
    assert_eq!(patients.len(), 1);
    assert_eq!(patients[0]["id"], "p1");
    let row = patients[0].as_object().expect("a row is an object");
    assert!(!row.contains_key("name"), "{row:?}");
    assert!(!row.contains_key("birthDate"), "{row:?}");
    assert_eq!(row["active"], true);
}

#[tokio::test]
async fn a_configuration_that_moved_on_stops_the_export() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store
        .create(config("anon-1", &["Patient.name"]))
        .await
        .unwrap();
    store
        .update(config("anon-1", &["Patient.birthDate"]), None)
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let stale = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("a2"),
        r#"{"scope":"system","_type":["Patient"],"_anonymizationConfig":"anon-1","_anonymizationConfigEtag":"1"}"#,
    )
    .await;
    assert_eq!(stale.state, JobState::Failed);
    assert!(stale.outcome.unwrap().contains("anon-1"));

    let absent = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("a3"),
        r#"{"scope":"system","_type":["Patient"],"_anonymizationConfig":"nowhere"}"#,
    )
    .await;
    assert_eq!(absent.state, JobState::Failed);
}

#[tokio::test]
async fn a_member_the_record_lacks_is_itemised_in_a_failure_file() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store
        .create(fhir_store_contract::fixture::envelope(
            "Group",
            "g1",
            r#""member":[{"entity":{"reference":"Patient/p1"}},{"entity":{"reference":"Patient/p9"}}]"#,
        ))
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("e1"),
        r#"{"scope":"group","id":"g1"}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let outcome: Value = serde_json::from_str(&record.outcome.unwrap()).unwrap();
    let failures = outcome["failures"].as_array().expect("failures are listed");
    assert_eq!(failures.len(), 1);
    assert!(failures[0].as_str().unwrap().contains("p9"), "{failures:?}");
    let files = sink.list(&job("e1")).await.unwrap();
    let failure = files
        .iter()
        .find(|file| file.kind == "OperationOutcome")
        .expect("a failure file is written");
    assert_eq!(failure.count, 1);
    let reported = rows(&sink.read(&job("e1"), &failure.name).await.unwrap());
    assert_eq!(reported[0]["resourceType"], "OperationOutcome");
    assert!(reported[0]["issue"][0]["diagnostics"]
        .as_str()
        .unwrap()
        .contains("p9"));
    let carried = rows(&sink.read(&job("e1"), "Patient.ndjson").await.unwrap());
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0]["id"], "p1");
}

fn lines_of(body: &[u8]) -> Vec<String> {
    String::from_utf8(body.to_vec())
        .expect("ndjson is text")
        .split('\n')
        .map(str::to_owned)
        .collect()
}

fn shaped(body: &[u8]) -> Vec<Value> {
    let mut written = lines_of(body);
    assert_eq!(
        written.pop().as_deref(),
        Some(""),
        "a newline delimited file ends with a newline"
    );
    assert!(!written.is_empty(), "a written file carries a row");
    written
        .iter()
        .map(|line| {
            assert!(!line.trim().is_empty(), "a blank line is not a resource");
            assert!(
                !line.starts_with('[') && !line.starts_with(','),
                "rows are delimited by newlines, not by an array"
            );
            let row: Value = serde_json::from_str(line).expect("every line is one resource");
            assert!(
                row.get("resourceType").and_then(Value::as_str).is_some(),
                "every row names its type"
            );
            row
        })
        .collect()
}

async fn seeded_pair() -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .create(observation("o1", "code-1", 3.0, "Patient/p1"))
        .await
        .unwrap();
    store
        .create(observation("o2", "code-2", 4.0, "Patient/p2"))
        .await
        .unwrap();
    store
}

#[tokio::test]
async fn every_resource_is_carried_once_and_only_in_the_file_of_its_type() {
    let store = seeded_pair().await;
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("c1"),
        r#"{"scope":"system"}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let outcome: Value = serde_json::from_str(&record.outcome.unwrap()).unwrap();
    assert_eq!(outcome["units"], 2, "one unit per exported type");
    assert_eq!(outcome["handled"], 4);
    let files = sink.list(&job("c1")).await.unwrap();
    let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, vec!["Observation.ndjson", "Patient.ndjson"]);

    let mut carried: Vec<String> = Vec::new();
    for file in &files {
        for row in shaped(&sink.read(&job("c1"), &file.name).await.unwrap()) {
            let named = format!(
                "{}/{}",
                row["resourceType"].as_str().unwrap(),
                row["id"].as_str().unwrap()
            );
            assert_eq!(
                row["resourceType"].as_str().unwrap(),
                file.name.trim_end_matches(".ndjson"),
                "a row landed in the file of another type"
            );
            assert!(!carried.contains(&named), "{named} was carried twice");
            carried.push(named);
        }
    }
    carried.sort();
    assert_eq!(
        carried,
        vec![
            "Observation/o1",
            "Observation/o2",
            "Patient/p1",
            "Patient/p2"
        ]
    );
}

#[tokio::test]
async fn a_unit_run_again_replaces_its_file_rather_than_adding_to_it() {
    let store = seeded_pair().await;
    let sink = Arc::new(MemoryBulkStore::new());
    let payload = r#"{"scope":"system","_type":["Patient"]}"#;

    ran(Arc::clone(&store), Arc::clone(&sink), job("c2"), payload).await;
    let first = sink.read(&job("c2"), "Patient.ndjson").await.unwrap();

    let again = ran(Arc::clone(&store), Arc::clone(&sink), job("c2"), payload).await;

    assert_eq!(again.state, JobState::Completed);
    let repeated = sink.read(&job("c2"), "Patient.ndjson").await.unwrap();
    assert_eq!(
        repeated, first,
        "a unit run again must replace its file, not add to it"
    );
    assert_eq!(shaped(&repeated).len(), 2);
    let files = sink.list(&job("c2")).await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].count, 2);
}

#[tokio::test]
async fn a_patient_export_carries_every_observation_of_the_compartment() {
    let store = seeded_pair().await;
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("c3"),
        r#"{"scope":"patient"}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let observations = shaped(&sink.read(&job("c3"), "Observation.ndjson").await.unwrap());
    let ids: Vec<&str> = observations
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["o1", "o2"]);
    let patients = shaped(&sink.read(&job("c3"), "Patient.ndjson").await.unwrap());
    let ids: Vec<&str> = patients
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["p1", "p2"]);
}

#[tokio::test]
async fn every_accepted_output_format_renders_the_same_newline_delimited_file() {
    let store = seeded_pair().await;
    let sink = Arc::new(MemoryBulkStore::new());
    let mut rendered = Vec::new();
    for (slot, format) in ["ndjson", "application/ndjson", "application/fhir+ndjson"]
        .iter()
        .enumerate()
    {
        let id = job(&format!("o{slot}"));
        let record = ran(
            Arc::clone(&store),
            Arc::clone(&sink),
            id.clone(),
            &format!(r#"{{"scope":"system","_type":["Patient"],"_outputFormat":"{format}"}}"#),
        )
        .await;
        assert_eq!(record.state, JobState::Completed, "{format} was refused");
        let body = sink.read(&id, "Patient.ndjson").await.unwrap();
        assert_eq!(shaped(&body).len(), 2);
        rendered.push(body);
    }
    assert_eq!(rendered[0], rendered[1]);
    assert_eq!(rendered[1], rendered[2]);
}

#[tokio::test]
async fn a_container_carries_the_failure_file_beside_the_rows() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store
        .create(fhir_store_contract::fixture::envelope(
            "Group",
            "g1",
            r#""member":[{"entity":{"reference":"Patient/p1"}},{"entity":{"reference":"Patient/p9"}}]"#,
        ))
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("c4"),
        r#"{"scope":"group","id":"g1","_container":"nightly"}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let names: Vec<String> = sink
        .list(&job("c4"))
        .await
        .unwrap()
        .into_iter()
        .map(|file| file.name)
        .collect();
    assert_eq!(
        names,
        vec!["nightly/Patient-failures.ndjson", "nightly/Patient.ndjson"]
    );
    let reported = shaped(
        &sink
            .read(&job("c4"), "nightly/Patient-failures.ndjson")
            .await
            .unwrap(),
    );
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0]["resourceType"], "OperationOutcome");
    assert_eq!(reported[0]["issue"][0]["severity"], "error");
    assert!(reported[0]["issue"][0]["diagnostics"]
        .as_str()
        .unwrap()
        .contains("p9"));
    assert_eq!(
        shaped(
            &sink
                .read(&job("c4"), "nightly/Patient.ndjson")
                .await
                .unwrap()
        )
        .len(),
        1
    );
}

#[tokio::test]
async fn a_redacted_element_is_gone_from_the_row_and_a_nested_one_leaves_its_parent() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store
        .create(config("anon-1", &["Patient.birthDate"]))
        .await
        .unwrap();
    store
        .create(config("anon-2", &["Patient.name.family"]))
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let plain = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("n0"),
        r#"{"scope":"system","_type":["Patient"]}"#,
    )
    .await;
    assert_eq!(plain.state, JobState::Completed);
    let held = shaped(&sink.read(&job("n0"), "Patient.ndjson").await.unwrap());
    let held = held[0].as_object().expect("a row is an object");
    assert!(
        held.contains_key("birthDate"),
        "the element to redact must be there to begin with"
    );
    assert_eq!(held["name"][0]["family"], "Stone");

    let whole = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("n1"),
        r#"{"scope":"system","_type":["Patient"],"_anonymizationConfig":"anon-1"}"#,
    )
    .await;
    assert_eq!(whole.state, JobState::Completed);
    let rows = shaped(&sink.read(&job("n1"), "Patient.ndjson").await.unwrap());
    let row = rows[0].as_object().expect("a row is an object");
    assert!(
        !row.contains_key("birthDate"),
        "a redacted element must carry no key at all"
    );
    assert!(row.contains_key("name"), "nothing else is removed");
    assert_eq!(row["active"], true);

    let nested = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("n2"),
        r#"{"scope":"system","_type":["Patient"],"_anonymizationConfig":"anon-2"}"#,
    )
    .await;
    assert_eq!(nested.state, JobState::Completed);
    let rows = shaped(&sink.read(&job("n2"), "Patient.ndjson").await.unwrap());
    let row = rows[0].as_object().expect("a row is an object");
    assert!(row.contains_key("name"), "the parent element stays");
    let named = row["name"][0].as_object().expect("a name is an object");
    assert!(
        !named.contains_key("family"),
        "a nested redaction must carry no key at all"
    );
    assert!(row.contains_key("birthDate"), "nothing else is removed");
}

#[tokio::test]
async fn an_unasked_associated_preset_is_refused_and_a_asked_one_is_carried() {
    let now = FhirInstant::parse("2026-09-06T05:00:00Z").unwrap();
    let refused = ExportRequest::parse(
        r#"{"scope":"system","includeAssociatedData":["_history"]}"#,
        &now,
    );
    let message = refused.unwrap_err().to_string();
    assert!(message.contains("_history"), "{message}");

    let denied = ExportRequest::parse(
        r#"{"scope":"system","includeAssociatedData":["_myCustomPreset"]}"#,
        &now,
    );
    let message = denied.unwrap_err().to_string();
    assert!(message.contains("_myCustomPreset"), "{message}");

    let carried = ExportRequest::parse(
        r#"{"scope":"system","includeAssociatedData":["LatestProvenanceResources","RelevantProvenanceResources"]}"#,
        &now,
    )
    .unwrap();
    assert!(carried.associated.latest);
    assert!(carried.associated.relevant);

    let none = ExportRequest::parse(r#"{"scope":"system"}"#, &now).unwrap();
    assert!(!none.associated.requested());
}

#[tokio::test]
async fn relevant_preset_exports_the_provenance_that_targets_exported_resources() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store
        .create(observation("o1", "code-1", 3.0, "Patient/p1"))
        .await
        .unwrap();
    store
        .create(provenance("pr1", "2026-09-06T04:01:00Z", "Patient/p1"))
        .await
        .unwrap();
    store
        .create(provenance("pr2", "2026-09-06T04:02:00Z", "Observation/o1"))
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("a1"),
        r#"{"scope":"system","_type":["Patient"],"includeAssociatedData":["RelevantProvenanceResources"]}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let names: Vec<String> = sink
        .list(&job("a1"))
        .await
        .unwrap()
        .into_iter()
        .map(|file| file.name)
        .collect();
    assert_eq!(names, vec!["Patient.ndjson", "Provenance.ndjson"]);
    let provs = rows(&sink.read(&job("a1"), "Provenance.ndjson").await.unwrap());
    assert_eq!(provs.len(), 1);
    assert_eq!(provs[0]["id"], "pr1");
    assert_eq!(provs[0]["target"][0]["reference"], "Patient/p1");
}

#[tokio::test]
async fn latest_preset_exports_only_the_most_recent_provenance_per_target() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .create(provenance("pr-old", "2026-09-06T04:01:00Z", "Patient/p1"))
        .await
        .unwrap();
    store
        .create(provenance("pr-new", "2026-09-06T04:03:00Z", "Patient/p1"))
        .await
        .unwrap();
    store
        .create(provenance("pr-other", "2026-09-06T04:04:00Z", "Patient/p2"))
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("a2"),
        r#"{"scope":"system","_type":["Patient"],"includeAssociatedData":["LatestProvenanceResources"]}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let provs = rows(&sink.read(&job("a2"), "Provenance.ndjson").await.unwrap());
    let ids: Vec<&str> = provs
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["pr-new", "pr-other"]);
}

#[tokio::test]
async fn a_preset_that_associates_nothing_writes_no_provenance_file() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store
        .create(provenance(
            "pr1",
            "2026-09-06T04:01:00Z",
            "Organization/org1",
        ))
        .await
        .unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("a3"),
        r#"{"scope":"system","_type":["Patient"],"includeAssociatedData":["RelevantProvenanceResources"]}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let names: Vec<String> = sink
        .list(&job("a3"))
        .await
        .unwrap()
        .into_iter()
        .map(|file| file.name)
        .collect();
    assert_eq!(names, vec!["Patient.ndjson"]);
}

#[tokio::test]
async fn an_export_until_names_the_window_end_the_release_uses() {
    let hand = Arc::new(Hand {
        now: Mutex::new("2026-09-06T04:00:00.000Z".to_owned()),
    });
    let store = Arc::new(MemoryStore::with_clock(clock(&hand)));
    store.create(patient("p1", "Stone", true)).await.unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    at(&hand, "2026-09-06T06:00:00.000Z");
    store.create(patient("p2", "Rivers", true)).await.unwrap();
    store
        .update(patient("p1", "Fields", true), None)
        .await
        .unwrap();

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("a4"),
        r#"{"scope":"system","_type":["Patient"],"_until":"2026-09-06T05:00:00.000Z"}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let outcome: Value = serde_json::from_str(&record.outcome.unwrap()).unwrap();
    assert_eq!(outcome["transactionTime"], "2026-09-06T05:00:00.000Z");
    let patients = rows(&sink.read(&job("a4"), "Patient.ndjson").await.unwrap());
    assert_eq!(patients.len(), 1);
    assert_eq!(patients[0]["id"], "p1");
    assert_eq!(patients[0]["name"][0]["family"], "Stone");
    assert_eq!(patients[0]["meta"]["versionId"], "1");
}

#[tokio::test]
async fn an_until_that_does_not_parse_fails_the_export_naming_it() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("a5"),
        r#"{"scope":"system","_until":"whenever"}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Failed);
    assert!(record.outcome.unwrap().contains("whenever"));
}

#[tokio::test]
async fn a_parameter_a_lenient_kick_off_dropped_is_itemised_in_a_failure_file() {
    let store = Arc::new(MemoryStore::default());
    store.create(patient("p1", "Stone", true)).await.unwrap();
    let sink = Arc::new(MemoryBulkStore::new());

    let record = ran(
        Arc::clone(&store),
        Arc::clone(&sink),
        job("l1"),
        r#"{"scope":"system","_unsupported":["_elements"]}"#,
    )
    .await;

    assert_eq!(record.state, JobState::Completed);
    let reported = shaped(
        &sink
            .read(&job("l1"), "parameters-failures.ndjson")
            .await
            .unwrap(),
    );
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0]["resourceType"], "OperationOutcome");
    assert_eq!(reported[0]["issue"][0]["severity"], "error");
    assert!(reported[0]["issue"][0]["diagnostics"]
        .as_str()
        .unwrap()
        .contains("_elements"));
    assert_eq!(
        rows(&sink.read(&job("l1"), "Patient.ndjson").await.unwrap()).len(),
        1
    );
}

#[test]
fn a_request_carries_the_parameters_a_lenient_kick_off_dropped() {
    let request = ExportRequest::parse(
        r#"{"scope":"system","_unsupported":["_elements","_since"]}"#,
        &FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap(),
    )
    .unwrap();
    assert_eq!(request.unsupported, vec!["_elements", "_since"]);
}
