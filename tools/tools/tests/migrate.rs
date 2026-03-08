use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service};
use fhir_core::{Error, FhirInstant, FhirVersion, ResourceEnvelope, ResourceId, VersionId};
use fhir_store::ResourceStore;
use fhir_tools::{http, migrate};
use std::sync::Arc;

const CLOCK: &str = "2026-09-06T04:00:00.000Z";

struct Server {
    address: String,
    _stop: tokio::sync::oneshot::Sender<()>,
}

async fn start(version: FhirVersion) -> Server {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse(CLOCK).expect("a fixed instant")
    }));
    let service = Service::new(Arc::new(store), version, Vec::<Dependency>::new());
    let bound = service
        .bind("127.0.0.1:0".parse().expect("a loopback address"))
        .await
        .expect("bind");
    let address = bound.local_addr().expect("the bound port").to_string();
    let (stop, receiver) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = bound
            .serve_until(async move {
                let _ = receiver.await;
            })
            .await;
    });
    Server {
        address,
        _stop: stop,
    }
}

async fn write(address: &str, method: &str, path: &str, body: &str) -> u16 {
    let (status, _) = http::send(address, "localhost", method, path, body)
        .unwrap_or_else(|error| panic!("{method} {path} to {address} failed: {error}"));
    assert!(
        (200..300).contains(&status),
        "{method} {path} answered {status}"
    );
    status
}

fn fresh() -> MemoryStore {
    MemoryStore::default()
}

fn patient(id: &str, family: &str) -> String {
    format!(r#"{{"resourceType":"Patient","id":"{id}","name":[{{"family":"{family}"}}]}}"#)
}

fn observation(id: &str, subject: &str) -> String {
    format!(
        r#"{{"resourceType":"Observation","id":"{id}","status":"final","code":{{"text":"probe"}},"subject":{{"reference":"Patient/{subject}"}}}}"#
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_history_moves_into_a_fresh_store_verbatim() {
    let server = start(FhirVersion::R4).await;
    write(&server.address, "POST", "/Patient", &patient("m-a", "Root")).await;
    write(
        &server.address,
        "PUT",
        "/Patient/m-a",
        &patient("m-a", "Trek"),
    )
    .await;
    write(
        &server.address,
        "POST",
        "/Observation",
        &observation("m-o1", "m-a"),
    )
    .await;
    write(&server.address, "DELETE", "/Patient/m-a", "").await;

    let target = fresh();
    let report = migrate::pull(&target, &server.address, FhirVersion::R4)
        .await
        .expect("pull runs");
    assert_eq!(report.read, 4);
    assert_eq!(report.written, 4);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.deleted, 1);
    assert!(report.failures.is_empty(), "{:?}", report.failures);

    let first = target
        .vread(
            &fhir_core::ResourceKey::new(
                "Patient".parse().unwrap(),
                ResourceId::parse("m-a").unwrap(),
            ),
            &VersionId::parse("1").unwrap(),
        )
        .await
        .unwrap();
    assert!(!first.is_deleted());
    assert_eq!(first.last_updated().as_str(), CLOCK);
    let body: serde_json::Value = serde_json::from_slice(first.raw()).unwrap();
    assert_eq!(body["name"][0]["family"], "Root");

    let second = target
        .vread(
            &fhir_core::ResourceKey::new(
                "Patient".parse().unwrap(),
                ResourceId::parse("m-a").unwrap(),
            ),
            &VersionId::parse("2").unwrap(),
        )
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(second.raw()).unwrap();
    assert_eq!(body["name"][0]["family"], "Trek");

    let marker = target
        .vread(
            &fhir_core::ResourceKey::new(
                "Patient".parse().unwrap(),
                ResourceId::parse("m-a").unwrap(),
            ),
            &VersionId::parse("3").unwrap(),
        )
        .await
        .unwrap();
    assert!(marker.is_deleted());
    assert_eq!(marker.version_id().as_str(), "3");

    let current = target
        .read(&fhir_core::ResourceKey::new(
            "Patient".parse().unwrap(),
            ResourceId::parse("m-a").unwrap(),
        ))
        .await
        .unwrap();
    assert!(current.is_deleted());
    assert_eq!(current.version_id().as_str(), "3");

    let observation = target
        .vread(
            &fhir_core::ResourceKey::new(
                "Observation".parse().unwrap(),
                ResourceId::parse("m-o1").unwrap(),
            ),
            &VersionId::parse("1").unwrap(),
        )
        .await
        .unwrap();
    assert!(!observation.is_deleted());

    let again = migrate::pull(&target, &server.address, FhirVersion::R4)
        .await
        .expect("a repeated pull runs");
    assert_eq!(again.written, 0);
    assert_eq!(again.skipped, 4);
    assert!(again.failures.is_empty());

    let held = migrate::reconcile(&target, &server.address, FhirVersion::R4)
        .await
        .expect("reconcile runs");
    assert_eq!(held.source, 4);
    assert_eq!(held.target, 4);
    assert_eq!(held.kept, 4);
    assert!(held.mismatched.is_empty(), "{:?}", held.mismatched);
    assert!(held.extra.is_empty(), "{:?}", held.extra);
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_of_many_pages_moves_whole() {
    let server = start(FhirVersion::R4).await;
    write(
        &server.address,
        "POST",
        "/Patient",
        &patient("m-roll", "Base"),
    )
    .await;
    for ordinal in 1..=25 {
        let name = format!("Name{ordinal}");
        write(
            &server.address,
            "PUT",
            "/Patient/m-roll",
            &patient("m-roll", &name),
        )
        .await;
    }

    let target = fresh();
    let report = migrate::pull(&target, &server.address, FhirVersion::R4)
        .await
        .expect("pull runs");
    assert_eq!(report.read, 26);
    assert_eq!(report.written, 26);
    let held = migrate::reconcile(&target, &server.address, FhirVersion::R4)
        .await
        .expect("reconcile runs");
    assert_eq!(held.kept, 26);
    assert!(held.mismatched.is_empty());
    assert!(held.extra.is_empty());
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_target_reports_the_source_versions_missing() {
    let server = start(FhirVersion::R4).await;
    write(
        &server.address,
        "POST",
        "/Patient",
        &patient("m-miss", "Root"),
    )
    .await;

    let target = fresh();
    let held = migrate::reconcile(&target, &server.address, FhirVersion::R4)
        .await
        .expect("reconcile runs");
    assert_eq!(held.source, 1);
    assert_eq!(held.kept, 0);
    assert_eq!(held.mismatched.len(), 1);
    assert!(held.mismatched[0].contains("Patient / m-miss v1"));
    assert!(held.extra.is_empty());
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_target_named_extra_versions_reports_them() {
    let server = start(FhirVersion::R4).await;
    write(
        &server.address,
        "POST",
        "/Patient",
        &patient("m-pure", "Root"),
    )
    .await;

    let target = fresh();
    migrate::pull(&target, &server.address, FhirVersion::R4)
        .await
        .expect("pull runs");
    let own =
        ResourceEnvelope::parse_supplied(FhirVersion::R4, patient("m-own", "Local").as_bytes())
            .expect("a supplied patient");
    target.create(own).await.expect("a target write");
    assert_eq!(
        target
            .read(&fhir_core::ResourceKey::new(
                "Patient".parse().unwrap(),
                ResourceId::parse("m-own").unwrap(),
            ))
            .await
            .unwrap()
            .version_id()
            .as_str(),
        "1"
    );

    let held = migrate::reconcile(&target, &server.address, FhirVersion::R4)
        .await
        .expect("reconcile runs");
    assert_eq!(held.kept, 1);
    assert!(held.mismatched.is_empty(), "{:?}", held.mismatched);
    assert_eq!(held.extra.len(), 1);
    assert!(held.extra[0].contains("Patient / m-own v1"));
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_source_is_refused() {
    let target = fresh();
    assert!(migrate::pull(&target, "127.0.0.1:1", FhirVersion::R4)
        .await
        .is_err());
    let held = migrate::reconcile(&target, "127.0.0.1:1", FhirVersion::R4).await;
    assert!(held.is_err());
    assert!(!matches!(held, Err(Error::InvalidJson(_))));
}
