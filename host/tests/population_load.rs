mod live;

use fhir_core::FhirVersion;
use fhir_tools::population::{history, Record};
use fhir_tools::upload;
use live::{request, spawn_server, stop};
use std::time::Duration;

const PATIENCE: Duration = Duration::from_secs(30);

fn address(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

fn cohort(subjects: u64, seed: u64) -> Vec<Record> {
    (0..subjects)
        .flat_map(|position| history(FhirVersion::R4, seed, position))
        .collect()
}

fn total(port: u16, resource_type: &str) -> i64 {
    let reply = request(
        port,
        "GET",
        &format!("/{resource_type}?_summary=count"),
        &[],
        &[],
    );
    assert_eq!(reply.status, 200, "{resource_type}: {}", reply.body);
    let body: serde_json::Value =
        serde_json::from_str(&reply.body).expect("a search answers a bundle");
    body["total"].as_i64().unwrap_or(-1)
}

#[test]
fn a_population_loads_through_import_and_is_readable_afterwards() {
    let (child, port) = spawn_server();
    let records = cohort(6, 211);
    let rows = upload::rows(&records);
    let expected = rows.lines().count();

    let loaded = upload::import(&address(port), "localhost", &rows, PATIENCE)
        .expect("the import is accepted and finishes");
    assert_eq!(loaded.submitted, expected);
    assert_eq!(
        loaded.failures, 0,
        "the import reported failures: {loaded:?}"
    );

    for record in records.iter().filter(|r| r.resource_type == "Patient") {
        let reply = request(port, "GET", &format!("/Patient/{}", record.id), &[], &[]);
        assert_eq!(
            reply.status, 200,
            "{} is not readable: {}",
            record.id, reply.body
        );
    }
    assert!(total(port, "Patient") > 0);
    stop(child);
}

#[test]
fn re_running_the_same_seed_adds_no_version() {
    let (child, port) = spawn_server();
    let records = cohort(4, 223);

    upload::load(&address(port), "localhost", &records, PATIENCE).expect("the first load");
    let patient = records
        .iter()
        .find(|record| record.resource_type == "Patient")
        .expect("the cohort has a patient");
    let first = request(port, "GET", &format!("/Patient/{}", patient.id), &[], &[]);
    assert_eq!(first.status, 200, "{}", first.body);
    let before: serde_json::Value = serde_json::from_str(&first.body).expect("a patient");
    let before_version = before["meta"]["versionId"]
        .as_str()
        .unwrap_or("?")
        .to_owned();

    let outstanding = upload::pending(&address(port), "localhost", &records)
        .expect("the instance answers what it holds");
    assert!(
        outstanding.is_empty(),
        "the first load left {} records unsettled: {:?}",
        outstanding.len(),
        outstanding.iter().map(|r| &r.id).collect::<Vec<_>>()
    );
    let again = upload::rows(&outstanding);
    if !again.is_empty() {
        upload::import(&address(port), "localhost", &again, PATIENCE).expect("the second load");
    }

    let second = request(port, "GET", &format!("/Patient/{}", patient.id), &[], &[]);
    let after: serde_json::Value = serde_json::from_str(&second.body).expect("a patient");
    assert_eq!(
        after["meta"]["versionId"].as_str().unwrap_or("?"),
        before_version,
        "re-running the seed advanced the version of {}",
        patient.id
    );
    stop(child);
}

#[test]
fn a_transaction_bundle_carries_the_population_through_the_same_instance() {
    let (child, port) = spawn_server();
    let records = cohort(3, 227);
    for bundle in upload::transactions(&records) {
        let entries = bundle["entry"].as_array().expect("entries").len();
        let loaded = upload::transact(&address(port), "localhost", &bundle)
            .expect("the transaction is answered");
        assert_eq!(loaded.submitted, entries, "an entry went unanswered");
        assert_eq!(
            loaded.failures, 0,
            "the transaction reported failures: {loaded:?}"
        );
    }

    for record in &records {
        let reply = request(
            port,
            "GET",
            &format!("/{}/{}", record.resource_type, record.id),
            &[],
            &[],
        );
        let expected = match record.deleted() {
            true => 410,
            false => 200,
        };
        assert_eq!(
            reply.status, expected,
            "{}/{} answered {} : {}",
            record.resource_type, record.id, reply.status, reply.body
        );
    }
    stop(child);
}
