use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::{MemoryBulkStore, MemoryJobStore, MemoryStore};
use fhir_api::{Dependency, Service, CORRELATION};
use fhir_core::{CorrelationId, FhirInstant, FhirVersion};
use fhir_jobs::{ExportJob, Orchestrator, Worker};
use fhir_store::{JobId, JobStore, Lease, StepTicker};
use fhir_telemetry::{Held, Telemetry};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    correlation: String,
    location: String,
}

fn service(jobs: Arc<dyn JobStore>, telemetry: &Arc<Telemetry>) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-07T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Box::pin(async { Ok(()) })),
    }];
    Service::new(Arc::new(store), FhirVersion::R4, dependencies)
        .with_jobs(jobs)
        .with_outputs(Arc::new(MemoryBulkStore::new()))
        .reporting(Arc::clone(telemetry))
}

async fn call(app: &Service, method: &str, uri: &str, offered: Option<&str>) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    if let Some(offered) = offered {
        builder = builder.header(CORRELATION, offered);
    }
    let response = app
        .router()
        .oneshot(builder.body(Body::from(Vec::new())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let read = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|held| held.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let correlation = read(CORRELATION);
    let location = read("content-location");
    let _ = response.into_body().collect().await;
    Reply {
        status,
        correlation,
        location,
    }
}

fn recorder() -> (Held, Arc<Telemetry>) {
    let sink = Held::default();
    let ticker = StepTicker::starting_at(0).ticker();
    (sink.clone(), Arc::new(Telemetry::new(sink.sink(), ticker)))
}

#[tokio::test]
async fn every_answer_carries_an_identifier_the_service_issued() {
    let (sink, telemetry) = recorder();
    let ticker = StepTicker::starting_at(1_000);
    let app = service(Arc::new(MemoryJobStore::new(ticker.ticker())), &telemetry);
    let reply = call(&app, "GET", "/metadata", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(CorrelationId::parse(&reply.correlation).is_ok());
    assert!(sink
        .lines()
        .iter()
        .any(|line| line.contains(&format!("correlation={}", reply.correlation))));
}

#[tokio::test]
async fn an_identifier_of_another_shape_is_replaced_and_never_reported() {
    let (sink, telemetry) = recorder();
    let ticker = StepTicker::starting_at(1_000);
    let app = service(Arc::new(MemoryJobStore::new(ticker.ticker())), &telemetry);
    let offered = "patient-smith-4711";
    let reply = call(&app, "GET", "/metadata", Some(offered)).await;
    assert_ne!(reply.correlation, offered);
    assert!(CorrelationId::parse(&reply.correlation).is_ok());
    assert!(!sink.lines().join("\n").contains(offered));
}

#[tokio::test]
async fn an_identifier_of_our_own_shape_is_carried_through() {
    let (_sink, telemetry) = recorder();
    let ticker = StepTicker::starting_at(1_000);
    let app = service(Arc::new(MemoryJobStore::new(ticker.ticker())), &telemetry);
    let offered = CorrelationId::fresh();
    let reply = call(&app, "GET", "/metadata", Some(offered.as_str())).await;
    assert_eq!(reply.correlation, offered.as_str());
}

#[tokio::test]
async fn a_job_resumed_after_a_dropped_lease_reports_under_the_request_identifier() {
    let (sink, telemetry) = recorder();
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>, &telemetry);
    let offered = CorrelationId::fresh();
    let reply = call(&app, "POST", "/$export", Some(offered.as_str())).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);
    assert_eq!(reply.correlation, offered.as_str());

    let id = JobId::parse(reply.location.rsplit('/').next().unwrap()).unwrap();
    let held = jobs.claim(&Lease::new("one", 1_000)).await.unwrap();
    assert_eq!(held.len(), 1);
    ticker.advance(1_500);
    assert_eq!(jobs.reclaim().await.unwrap(), vec![id.clone()]);

    let store = MemoryStore::default();
    let orchestrator = Orchestrator::new()
        .with(Arc::new(ExportJob::new(
            Arc::new(store),
            Arc::new(MemoryBulkStore::new()),
        )))
        .reporting(Arc::clone(&telemetry));
    let worker = Worker::new(
        Arc::clone(&jobs) as Arc<dyn JobStore>,
        Arc::new(orchestrator),
        "two",
        1_000,
    );
    assert_eq!(worker.poll().await.unwrap(), 1);
    assert_eq!(
        jobs.fetch(&id).await.unwrap().correlation.as_ref(),
        Some(&offered)
    );

    let lines = sink.lines();
    let tied = lines
        .iter()
        .filter(|line| line.contains(&format!("correlation={offered}")))
        .count();
    assert_eq!(tied, 2, "the request and the resumed job both report: {lines:?}");
    assert!(lines
        .iter()
        .all(|line| line.contains("operation=export")));
}
