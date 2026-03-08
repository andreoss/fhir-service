

use fhir_adapter_memory::MemoryStore;
use fhir_core::{FhirInstant, FhirVersion, ResourceKey};
use fhir_host::preload;
use fhir_store::ResourceStore;
use serde_json::json;
use std::sync::Arc;

fn store() -> Arc<dyn ResourceStore> {
    Arc::new(MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    })))
}




struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let held = std::env::temp_dir().join(format!(
            "fhir-preload-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&held);
        std::fs::create_dir_all(&held).expect("a directory");
        Scratch(held)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn directory() -> Scratch {
    Scratch::new()
}

fn write(at: &std::path::Path, name: &str, text: &str) {
    std::fs::write(at.join(name), text).expect("a file is written");
}

fn key(kind: &str, id: &str) -> ResourceKey {
    ResourceKey::new(
        kind.parse().expect("a known type"),
        id.parse().expect("an id"),
    )
}

#[tokio::test]
async fn a_directory_is_read_and_written() {
    let held = directory();
    write(
        held.path(),
        "patients.ndjson",
        &format!(
            "{}\n{}\n",
            json!({"resourceType": "Patient", "id": "p1", "active": true}),
            json!({"resourceType": "Patient", "id": "p2", "active": false})
        ),
    );
    write(
        held.path(),
        "organisation.json",
        &json!({"resourceType": "Organization", "id": "o1", "name": "One"}).to_string(),
    );
    write(held.path(), "notes.txt", "this is not a resource");

    let store = store();
    let read = preload::read(held.path(), FhirVersion::R4).expect("the directory is read");
    assert_eq!(read.len(), 3, "and the file that is not a resource is not");
    let loaded = preload::load(store.as_ref(), FhirVersion::R4, &read)
        .await
        .expect("the load");
    assert_eq!(loaded.written, 3);
    assert!(store.read(&key("Patient", "p1")).await.is_ok());
    assert!(store.read(&key("Organization", "o1")).await.is_ok());
}

#[tokio::test]
async fn starting_twice_leaves_what_starting_once_left() {
    let held = directory();
    write(
        held.path(),
        "one.json",
        &json!({"resourceType": "Patient", "id": "p1", "active": true}).to_string(),
    );
    let store = store();
    let read = preload::read(held.path(), FhirVersion::R4).expect("the directory is read");
    preload::load(store.as_ref(), FhirVersion::R4, &read)
        .await
        .expect("the first load");
    let again = preload::load(store.as_ref(), FhirVersion::R4, &read)
        .await
        .expect("the second load");
    assert_eq!(
        again.unchanged, 1,
        "the second start wrote nothing: {again:?}"
    );
    let stored = store
        .read(&key("Patient", "p1"))
        .await
        .expect("it is there");
    assert_eq!(
        stored.version_id().as_str(),
        "1",
        "and it is still the first version"
    );
}

#[tokio::test]
async fn a_changed_file_is_written_on_the_next_start() {
    let held = directory();
    write(
        held.path(),
        "one.json",
        &json!({"resourceType": "Patient", "id": "p1", "active": true}).to_string(),
    );
    let store = store();
    let first = preload::read(held.path(), FhirVersion::R4).expect("read");
    preload::load(store.as_ref(), FhirVersion::R4, &first)
        .await
        .expect("the first load");
    write(
        held.path(),
        "one.json",
        &json!({"resourceType": "Patient", "id": "p1", "active": false}).to_string(),
    );
    let second = preload::read(held.path(), FhirVersion::R4).expect("read");
    let loaded = preload::load(store.as_ref(), FhirVersion::R4, &second)
        .await
        .expect("the second load");
    assert_eq!(loaded.written, 1);
    let stored = store
        .read(&key("Patient", "p1"))
        .await
        .expect("it is there");
    assert_eq!(stored.version_id().as_str(), "2");
}

#[test]
fn a_resource_that_does_not_validate_stops_the_instance() {
    let held = directory();
    write(
        held.path(),
        "bad.json",
        &json!({"resourceType": "Patient", "id": "p1", "gender": "yes"}).to_string(),
    );
    let error = preload::read(held.path(), FhirVersion::R4).expect_err("it must not be read");
    assert!(
        error.to_string().contains("Patient/p1"),
        "the refusal names the file's resource so it can be fixed: {error}"
    );
}

#[test]
fn a_resource_with_no_id_is_refused() {
    let held = directory();
    write(
        held.path(),
        "bad.json",
        &json!({"resourceType": "Patient", "active": true}).to_string(),
    );
    let error = preload::read(held.path(), FhirVersion::R4).expect_err("it must not be read");
    assert!(
        error.to_string().contains("no id"),
        "a resource with no id cannot be written twice to the same place: {error}"
    );
}

#[test]
fn a_bundle_is_read_as_its_entries() {
    let held = directory();
    write(
        held.path(),
        "bundle.json",
        &json!({
            "resourceType": "Bundle",
            "type": "collection",
            "entry": [
                {"resource": {"resourceType": "Patient", "id": "p1", "active": true}},
                {"resource": {"resourceType": "Patient", "id": "p2", "active": true}},
            ],
        })
        .to_string(),
    );
    let read = preload::read(held.path(), FhirVersion::R4).expect("the directory is read");
    assert_eq!(read.len(), 2, "a set of resources handed over as a bundle");
}

#[test]
fn a_directory_that_is_not_there_is_said_so() {
    let error = preload::read(std::path::Path::new("/nowhere/at/all"), FhirVersion::R4)
        .expect_err("it must not be read");
    assert!(error.to_string().contains("cannot be read"), "{error}");
}

#[tokio::test]
async fn emptying_a_store_removes_what_is_in_it() {
    let store = store();
    let held = directory();
    write(
        held.path(),
        "two.ndjson",
        &format!(
            "{}\n{}\n",
            json!({"resourceType": "Patient", "id": "p1", "active": true}),
            json!({"resourceType": "Patient", "id": "p2", "active": true})
        ),
    );
    let read = preload::read(held.path(), FhirVersion::R4).expect("read");
    preload::load(store.as_ref(), FhirVersion::R4, &read)
        .await
        .expect("the load");
    let removed = store.empty().await.expect("the store is emptied");
    assert_eq!(removed, 2);
    assert!(store.read(&key("Patient", "p1")).await.is_err());
    let page = store
        .search(&fhir_store::SearchQuery::default())
        .await
        .expect("a search");
    assert!(page.entries.is_empty(), "and nothing is found afterwards");
}
