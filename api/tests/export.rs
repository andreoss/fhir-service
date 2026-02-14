use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::{MemoryBulkStore, MemoryJobStore, MemoryStore};
use fhir_api::{Dependency, Service};
use fhir_core::{FhirInstant, FhirVersion};
use fhir_jobs::{ExportJob, Orchestrator, Worker};
use fhir_store::{BulkStore, JobId, JobStore, ResourceStore, StepTicker};
use fhir_store_contract::fixture::{observation, patient};
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: String,
}

struct Harness {
    app: Service,
    jobs: Arc<MemoryJobStore>,
    store: Arc<MemoryStore>,
    sink: Arc<MemoryBulkStore>,
}

fn harness() -> Harness {
    let store = Arc::new(MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    })));
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let sink = Arc::new(MemoryBulkStore::new());
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Box::pin(async { Ok(()) })),
    }];
    let app = Service::new(Arc::clone(&store) as Arc<dyn ResourceStore>, FhirVersion::R4, dependencies)
        .with_jobs(Arc::clone(&jobs) as Arc<dyn JobStore>)
        .with_outputs(Arc::clone(&sink) as Arc<dyn BulkStore>);
    Harness {
        app,
        jobs,
        store,
        sink,
    }
}

async fn request(app: &Service, method: &str, uri: &str, body: &[u8]) -> Reply {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .body(Body::from(body.to_vec()))
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_str().unwrap_or_default().to_owned()))
        .collect();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn header<'a>(reply: &'a Reply, name: &str) -> &'a str {
    reply
        .headers
        .iter()
        .find(|(held, _)| held.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
}

fn submitted(reply: &Reply) -> JobId {
    let location = header(reply, "content-location");
    let tail = location.rsplit('/').next().expect("a location ends in an id");
    JobId::parse(tail).expect("the announced id is valid")
}

async fn work(held: &Harness) {
    let orchestrator = Arc::new(Orchestrator::new().with(Arc::new(ExportJob::new(
        Arc::clone(&held.store) as Arc<dyn ResourceStore>,
        Arc::clone(&held.sink) as Arc<dyn BulkStore>,
    ))));
    let worker = Worker::new(
        Arc::clone(&held.jobs) as Arc<dyn JobStore>,
        orchestrator,
        "one",
        5_000,
    );
    worker.poll().await.unwrap();
}

async fn seeded(held: &Harness) {
    held.store.create(patient("p1", "Stone", true)).await.unwrap();
    held.store.create(patient("p2", "Rivers", true)).await.unwrap();
    held.store
        .create(observation("o1", "code-1", 3.0, "Patient/p1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_system_export_reports_its_files_and_serves_them() {
    let held = harness();
    seeded(&held).await;

    let accepted = request(&held.app, "GET", "/$export", b"").await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED);
    let id = submitted(&accepted);
    work(&held).await;

    let done = request(&held.app, "GET", &format!("/_jobs/{id}"), b"").await;
    assert_eq!(done.status, StatusCode::OK);
    let manifest: Value = serde_json::from_str(&done.body).unwrap();
    assert_eq!(manifest["state"], "completed");
    assert_eq!(manifest["requiresAccessToken"], false);
    let files = manifest["output"].as_array().expect("a manifest lists files");
    assert_eq!(files.len(), 2);
    let patients = files
        .iter()
        .find(|file| file["type"] == "Patient")
        .expect("the patients are listed");
    assert_eq!(patients["count"], 2);
    let url = patients["url"].as_str().unwrap();
    assert!(url.starts_with("http://localhost/_jobs/"), "{url}");

    let path = url.trim_start_matches("http://localhost");
    let file = request(&held.app, "GET", path, b"").await;
    assert_eq!(file.status, StatusCode::OK);
    assert_eq!(header(&file, "content-type"), "application/fhir+ndjson");
    let rows: Vec<&str> = file.body.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(rows.len(), 2);
    let first: Value = serde_json::from_str(rows[0]).unwrap();
    assert_eq!(first["resourceType"], "Patient");
}

#[tokio::test]
async fn a_patient_and_a_group_export_are_addressed_by_their_own_paths() {
    let held = harness();
    seeded(&held).await;
    held.store
        .create(fhir_store_contract::fixture::envelope(
            "Group",
            "g1",
            r#""member":[{"entity":{"reference":"Patient/p1"}}]"#,
        ))
        .await
        .unwrap();

    let patients = request(&held.app, "GET", "/Patient/$export", b"").await;
    assert_eq!(patients.status, StatusCode::ACCEPTED);
    let group = request(&held.app, "GET", "/Group/g1/$export", b"").await;
    assert_eq!(group.status, StatusCode::ACCEPTED);
    let id = submitted(&group);
    work(&held).await;
    work(&held).await;

    let done = request(&held.app, "GET", &format!("/_jobs/{id}"), b"").await;
    assert_eq!(done.status, StatusCode::OK);
    let manifest: Value = serde_json::from_str(&done.body).unwrap();
    let files = manifest["output"].as_array().unwrap();
    let patient_file = files
        .iter()
        .find(|file| file["type"] == "Patient")
        .expect("a group export carries its members");
    assert_eq!(patient_file["count"], 1);
}

#[tokio::test]
async fn an_unknown_file_of_a_job_is_not_found() {
    let held = harness();
    let missing = request(&held.app, "GET", "/_jobs/nobody/Patient.ndjson", b"").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_parameter_the_export_does_not_implement_is_refused() {
    let held = harness();
    seeded(&held).await;

    let refused = request(&held.app, "GET", "/$export?_nonesuch=1", b"").await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert!(refused.body.contains("_nonesuch"), "{}", refused.body);

    let empty = request(&held.app, "GET", "/$export?_type=", b"").await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);

    let format = request(&held.app, "GET", "/$export?_outputFormat=text/csv", b"").await;
    assert_eq!(format.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_listed_parameters_reach_the_submitted_description() {
    let held = harness();
    seeded(&held).await;

    let accepted = request(
        &held.app,
        "GET",
        "/$export?_type=Patient,Observation&_typeFilter=Patient%3Ffamily%3DStone\
&_since=2026-09-06T00:00:00.000Z&_till=2026-09-06T12:00:00.000Z\
&_outputFormat=application/fhir%2Bndjson&_container=nightly",
        b"",
    )
    .await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED);
    let id = submitted(&accepted);
    let held_job = held.jobs.fetch(&id).await.unwrap();
    let payload: Value = serde_json::from_str(held_job.payload.as_deref().unwrap()).unwrap();
    assert_eq!(payload["_type"][1], "Observation");
    assert_eq!(payload["_typeFilter"][0], "Patient?family=Stone");
    assert_eq!(payload["_since"], "2026-09-06T00:00:00.000Z");
    assert_eq!(payload["_till"], "2026-09-06T12:00:00.000Z");
    assert_eq!(payload["_container"], "nightly");

    work(&held).await;
    let done = request(&held.app, "GET", &format!("/_jobs/{id}"), b"").await;
    let manifest: Value = serde_json::from_str(&done.body).unwrap();
    assert_eq!(manifest["transactionTime"], "2026-09-06T12:00:00.000Z");
    let files = manifest["output"].as_array().unwrap();
    let patients = files.iter().find(|file| file["type"] == "Patient").unwrap();
    assert_eq!(patients["count"], 1);
    assert!(
        patients["url"].as_str().unwrap().contains("/nightly/"),
        "{}",
        patients["url"]
    );
}

#[tokio::test]
async fn a_running_export_reports_how_far_it_has_come() {
    let held = harness();
    seeded(&held).await;

    let accepted = request(&held.app, "GET", "/$export", b"").await;
    let id = submitted(&accepted);
    let queued = request(&held.app, "GET", &format!("/_jobs/{id}"), b"").await;
    assert_eq!(queued.status, StatusCode::ACCEPTED);
    assert_eq!(header(&queued, "x-progress"), "queued");

    let claimed = held
        .jobs
        .claim(&fhir_store::Lease::new("one", 5_000))
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    held.jobs
        .heartbeat(
            &id,
            "one",
            5_000,
            Some(fhir_store::JobProgress {
                done: 1,
                total: Some(2),
                detail: Some("Patient".to_owned()),
            }),
        )
        .await
        .unwrap();

    let running = request(&held.app, "GET", &format!("/_jobs/{id}"), b"").await;
    assert_eq!(running.status, StatusCode::ACCEPTED);
    assert_eq!(header(&running, "retry-after"), "1");
    assert_eq!(header(&running, "x-progress"), "running 1/2 50% Patient");
}

#[tokio::test]
async fn a_finished_export_details_the_request_and_itemises_what_it_missed() {
    let held = harness();
    seeded(&held).await;
    held.store
        .create(fhir_store_contract::fixture::envelope(
            "Group",
            "g1",
            r#""member":[{"entity":{"reference":"Patient/p1"}},{"entity":{"reference":"Patient/p9"}}]"#,
        ))
        .await
        .unwrap();

    let accepted = request(&held.app, "GET", "/Group/g1/$export?_type=Patient", b"").await;
    let id = submitted(&accepted);
    work(&held).await;

    let done = request(&held.app, "GET", &format!("/_jobs/{id}"), b"").await;
    assert_eq!(done.status, StatusCode::OK);
    let manifest: Value = serde_json::from_str(&done.body).unwrap();
    assert_eq!(manifest["progress"]["done"], 1);
    assert_eq!(manifest["progress"]["total"], 1);
    assert_eq!(manifest["request"]["scope"], "group");
    assert_eq!(manifest["request"]["id"], "g1");
    assert_eq!(manifest["request"]["_type"][0], "Patient");

    let errors = manifest["error"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["type"], "OperationOutcome");
    assert_eq!(errors[0]["count"], 1);
    let reported = request(
        &held.app,
        "GET",
        errors[0]["url"].as_str().unwrap().trim_start_matches("http://localhost"),
        b"",
    )
    .await;
    assert_eq!(reported.status, StatusCode::OK);
    assert!(reported.body.contains("Patient/p9"), "{}", reported.body);
}
