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
#[tokio::test]
async fn search_without_parameters_returns_every_current_resource() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    store.create(envelope(FhirVersion::R4, "pt-2", false)).await.unwrap();
    let found = store.search(None, &Vec::new()).await.unwrap();
    assert_eq!(found.len(), 2);
}

#[tokio::test]
async fn search_matches_a_top_level_field() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    store.create(envelope(FhirVersion::R4, "pt-2", false)).await.unwrap();
    let params = vec![("active".to_owned(), "true".to_owned())];
    let found = store.search(None, &params).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id().as_str(), "pt-1");
}

#[tokio::test]
async fn search_matches_the_resource_id() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    store.create(envelope(FhirVersion::R4, "pt-2", true)).await.unwrap();
    let params = vec![("_id".to_owned(), "pt-2".to_owned())];
    let found = store.search(None, &params).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id().as_str(), "pt-2");
}

#[tokio::test]
async fn search_restricted_to_a_type_ignores_other_types() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    let observation = ResourceEnvelope::parse(
        FhirVersion::R4,
        br#"{"resourceType":"Observation","id":"ob-1","meta":{"versionId":"0","lastUpdated":"2026-09-06T04:00:00Z"},"status":"final"}"#,
    )
    .unwrap();
    store.create(observation).await.unwrap();
    let patients = store.search(Some("Patient".parse().unwrap()), &Vec::new()).await.unwrap();
    assert_eq!(patients.len(), 1);
    assert_eq!(patients[0].resource_type().as_str(), "Patient");
}

#[tokio::test]
async fn search_reports_no_match_for_an_unknown_field() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    let params = vec![("gender".to_owned(), "female".to_owned())];
    assert!(store.search(None, &params).await.unwrap().is_empty());
}

#[tokio::test]
async fn search_sees_the_current_version_only() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    store.update(envelope(FhirVersion::R4, "pt-1", false), None).await.unwrap();
    let params = vec![("active".to_owned(), "true".to_owned())];
    assert!(store.search(None, &params).await.unwrap().is_empty());
    let params = vec![("active".to_owned(), "false".to_owned())];
    assert_eq!(store.search(None, &params).await.unwrap().len(), 1);
}

#[tokio::test]
async fn delete_appends_a_marker_version() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d1", true)).await.unwrap();
    let marker = store.delete(&id("pt-d1")).await.unwrap();
    assert!(marker.is_deleted());
    assert_eq!(marker.version_id().as_str(), "2");
    let current = store.read(&id("pt-d1")).await.unwrap();
    assert!(current.is_deleted());
}

#[tokio::test]
async fn delete_leaves_earlier_versions_readable() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d2", true)).await.unwrap();
    store.delete(&id("pt-d2")).await.unwrap();
    let first = store.vread(&id("pt-d2"), &version("1")).await.unwrap();
    assert!(!first.is_deleted());
    assert_eq!(first.version_id().as_str(), "1");
}

#[tokio::test]
async fn delete_of_an_unknown_id_is_not_found() {
    let store = store();
    assert_eq!(store.delete(&id("pt-none")).await.unwrap_err(), Error::NotFound);
}

#[tokio::test]
async fn deleting_twice_reports_the_resource_as_deleted() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d3", true)).await.unwrap();
    store.delete(&id("pt-d3")).await.unwrap();
    assert_eq!(store.delete(&id("pt-d3")).await.unwrap_err(), Error::Deleted);
}

#[tokio::test]
async fn a_deleted_resource_is_invisible_to_search() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d4", true)).await.unwrap();
    store.delete(&id("pt-d4")).await.unwrap();
    assert!(store.search(None, &Vec::new()).await.unwrap().is_empty());
}

#[tokio::test]
async fn updating_a_deleted_resource_restores_it() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d5", true)).await.unwrap();
    store.delete(&id("pt-d5")).await.unwrap();
    let restored = store.update(envelope(FhirVersion::R4, "pt-d5", true), None).await.unwrap();
    assert!(!restored.is_deleted());
    assert_eq!(restored.version_id().as_str(), "3");
    assert_eq!(store.search(None, &Vec::new()).await.unwrap().len(), 1);
}

#[tokio::test]
async fn hard_delete_removes_every_version() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d6", true)).await.unwrap();
    store.update(envelope(FhirVersion::R4, "pt-d6", false), None).await.unwrap();
    store.hard_delete(&id("pt-d6")).await.unwrap();
    assert_eq!(store.read(&id("pt-d6")).await.unwrap_err(), Error::NotFound);
    assert_eq!(store.vread(&id("pt-d6"), &version("1")).await.unwrap_err(), Error::NotFound);
}

#[tokio::test]
async fn hard_delete_of_an_unknown_id_is_not_found() {
    let store = store();
    assert_eq!(store.hard_delete(&id("pt-none")).await.unwrap_err(), Error::NotFound);
}

#[tokio::test]
async fn purge_history_keeps_the_current_version_only() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d7", true)).await.unwrap();
    store.update(envelope(FhirVersion::R4, "pt-d7", false), None).await.unwrap();
    let purged = store.purge_history(&id("pt-d7")).await.unwrap();
    assert_eq!(purged, 1);
    assert_eq!(store.read(&id("pt-d7")).await.unwrap().version_id().as_str(), "2");
    assert_eq!(store.vread(&id("pt-d7"), &version("1")).await.unwrap_err(), Error::NotFound);
    assert_eq!(store.purge_history(&id("pt-d7")).await.unwrap(), 0);
}

#[tokio::test]
async fn purge_history_of_an_unknown_id_is_not_found() {
    let store = store();
    assert_eq!(store.purge_history(&id("pt-none")).await.unwrap_err(), Error::NotFound);
}
