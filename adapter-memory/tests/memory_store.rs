use fhir_adapter_memory::MemoryStore;
use fhir_core::{Error, FhirInstant, FhirVersion, ResourceEnvelope, ResourceId, VersionId};
use fhir_store::ResourceStore;
use std::sync::Arc;

fn store() -> MemoryStore {
    MemoryStore::with_clock(Arc::new(|| FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()))
}

fn envelope(version: FhirVersion, id: &str, active: bool) -> ResourceEnvelope {
    let bytes = format!(
        r#"{{"resourceType":"Patient","id":"{id}","meta":{{"versionId":"0","lastUpdated":"2026-09-06T04:00:00Z"}},"active":{active}}}"#
    )
    .into_bytes();
    ResourceEnvelope::parse(version, &bytes).unwrap()
}

fn id(value: &str) -> ResourceId {
    ResourceId::parse(value).unwrap()
}

fn version(value: &str) -> VersionId {
    VersionId::parse(value).unwrap()
}

#[tokio::test]
async fn health_reports_alive() {
    let store = store();
    assert_eq!(store.health(), Ok(()));
}

#[tokio::test]
async fn create_read_round_trip_over_every_version() {
    for version in FhirVersion::ALL {
        let store = store();
        let created = store.create(envelope(version, "pt-1", true)).await.unwrap();
        assert_eq!(created.version_id().as_str(), "1");
        assert_eq!(created.version(), version);

        let read = store.read(&id("pt-1")).await.unwrap();
        assert_eq!(read.version_id().as_str(), "1");
        assert_eq!(read.raw(), created.raw());
        assert_eq!(read.version(), version);
    }
}

#[tokio::test]
async fn create_rejects_duplicate_id() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    let error = store.create(envelope(FhirVersion::R4, "pt-1", false)).await.unwrap_err();
    assert!(matches!(error, Error::Duplicate(_)));
    assert_eq!(error.http_status(), 409);
}

#[tokio::test]
async fn read_unknown_id_rejected() {
    let store = store();
    let error = store.read(&id("nobody")).await.unwrap_err();
    assert!(matches!(error, Error::NotFound));
    assert_eq!(error.http_status(), 404);
}

#[tokio::test]
async fn vread_returns_every_historical_version() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-2", true)).await.unwrap();
    let updated = store.update(envelope(FhirVersion::R4, "pt-2", false), Some(&version("1"))).await.unwrap();
    assert_eq!(updated.version_id().as_str(), "2");

    let v1 = store.vread(&id("pt-2"), &version("1")).await.unwrap();
    assert_eq!(v1.version_id().as_str(), "1");
    let old = std::str::from_utf8(v1.raw()).unwrap();
    assert!(old.contains("\"active\":true"));

    let v2 = store.vread(&id("pt-2"), &version("2")).await.unwrap();
    assert_eq!(v2.version_id().as_str(), "2");
    assert!(std::str::from_utf8(v2.raw()).unwrap().contains("\"active\":false"));

    let current = store.read(&id("pt-2")).await.unwrap();
    assert_eq!(current.version_id().as_str(), "2");
}

#[tokio::test]
async fn vread_unknown_id_and_version_rejected() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-3", true)).await.unwrap();

    let missing_id = store.vread(&id("nobody"), &version("1")).await.unwrap_err();
    assert!(matches!(missing_id, Error::NotFound));

    let missing_version = store.vread(&id("pt-3"), &version("99")).await.unwrap_err();
    assert!(matches!(missing_version, Error::NotFound));
    assert_eq!(missing_version.http_status(), 404);
}

#[tokio::test]
async fn update_with_expected_version_creates_new_version() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-4", true)).await.unwrap();
    let updated = store.update(envelope(FhirVersion::R4, "pt-4", false), Some(&version("1"))).await.unwrap();
    assert_eq!(updated.version_id().as_str(), "2");
    let current = store.read(&id("pt-4")).await.unwrap();
    assert_eq!(current.version_id().as_str(), "2");
}

#[tokio::test]
async fn stale_expected_version_conflicts() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-5", true)).await.unwrap();
    store.update(envelope(FhirVersion::R4, "pt-5", false), Some(&version("1"))).await.unwrap();
    let error = store.update(envelope(FhirVersion::R4, "pt-5", false), Some(&version("1"))).await.unwrap_err();
    assert!(matches!(error, Error::VersionConflict));
    assert_eq!(error.http_status(), 409);
}

#[tokio::test]
async fn noop_update_creates_no_version() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-6", true)).await.unwrap();
    let current = store.read(&id("pt-6")).await.unwrap();

    let first = store.update(current.clone(), Some(current.version_id())).await.unwrap();
    assert_eq!(first.version_id().as_str(), "1");

    let second = store.update(envelope(FhirVersion::R4, "pt-6", true), None).await.unwrap();
    assert_eq!(second.version_id().as_str(), "1");
    assert_eq!(second.raw(), current.raw());

    let read = store.read(&id("pt-6")).await.unwrap();
    assert_eq!(read.version_id().as_str(), "1");
}

#[tokio::test]
async fn update_without_expected_version_changes_content() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-7", true)).await.unwrap();
    let updated = store.update(envelope(FhirVersion::R4, "pt-7", false), None).await.unwrap();
    assert_eq!(updated.version_id().as_str(), "2");
}

#[tokio::test]
async fn update_unknown_id_rejected() {
    let store = store();
    let error = store.update(envelope(FhirVersion::R4, "nobody", true), None).await.unwrap_err();
    assert!(matches!(error, Error::NotFound));
    assert_eq!(error.http_status(), 404);
}

#[tokio::test]
async fn concurrent_updates_with_same_expected_version_one_conflicts() {
    let store = Arc::new(store());
    store.create(envelope(FhirVersion::R4, "pt-8", true)).await.unwrap();

    let first = Arc::clone(&store);
    let second = Arc::clone(&store);
    let winner = tokio::spawn(async move {
        first.update(envelope(FhirVersion::R4, "pt-8", false), Some(&version("1"))).await
    });
    let loser = tokio::spawn(async move {
        second.update(envelope(FhirVersion::R4, "pt-8", false), Some(&version("1"))).await
    });
    let (winner, loser) = tokio::join!(winner, loser);

    let outcomes = [winner.unwrap().err().map(|_| ()), loser.unwrap().err().map(|_| ())];
    assert_eq!(outcomes.iter().filter(|o| o.is_none()).count(), 1, "one update must succeed");
    assert_eq!(outcomes.iter().filter(|o| o.is_some()).count(), 1, "one update must conflict");
}

#[tokio::test]
async fn history_reaches_final_state_after_update_chain() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-9", true)).await.unwrap();
    store.update(envelope(FhirVersion::R4, "pt-9", false), Some(&version("1"))).await.unwrap();
    let v3 = store.update(envelope(FhirVersion::R4, "pt-9", true), Some(&version("2"))).await.unwrap();
    assert_eq!(v3.version_id().as_str(), "3");
    assert_eq!(store.vread(&id("pt-9"), &version("3")).await.unwrap().version_id().as_str(), "3");
    assert_eq!(store.vread(&id("pt-9"), &version("2")).await.unwrap().version_id().as_str(), "2");
    assert_eq!(store.vread(&id("pt-9"), &version("1")).await.unwrap().version_id().as_str(), "1");
}