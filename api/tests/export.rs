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
        check: Arc::new(|| Ok(())),
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
