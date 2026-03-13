mod live;

use fhir_core::FhirVersion;
use fhir_tools::population::{history, Record};
use fhir_tools::{upload, verify};
use live::{request, spawn_server, stop};
use std::time::Duration;

const PATIENCE: Duration = Duration::from_secs(60);

fn address(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

fn cohort(subjects: u64, seed: u64) -> Vec<Record> {
    (0..subjects)
        .flat_map(|position| history(FhirVersion::R4, seed, position))
        .collect()
}

fn loaded(port: u16, records: &[Record]) {
    upload::load(&address(port), "localhost", records, PATIENCE).expect("the population loads");
}

#[test]
fn the_counts_the_generator_predicted_are_the_counts_search_returns() {
    let (child, port) = spawn_server();
    let records = cohort(8, 401);
    loaded(port, &records);

    let predicted = verify::predicted(&records);
    let observed = verify::observed(&address(port), "localhost", predicted.keys().cloned())
        .expect("search answers a count for every type");
    let differences = verify::differences(&predicted, &observed);
    assert!(
        differences.is_empty(),
        "prediction and search disagree: {differences:?}"
    );
    stop(child);
}

#[test]
fn every_loaded_resource_passes_validate() {
    let (child, port) = spawn_server();
    let records = cohort(6, 409);
    loaded(port, &records);

    let refused = verify::validated(&address(port), "localhost", &records)
        .expect("the service answers $validate");
    assert!(
        refused.is_empty(),
        "the service refused what it stored: {refused:?}"
    );
    stop(child);
}

#[test]
fn everything_returns_the_compartment_the_generator_built() {
    let (child, port) = spawn_server();
    let records = cohort(5, 419);
    loaded(port, &records);

    for patient in records.iter().filter(|r| r.resource_type == "Patient") {
        let subject: Vec<Record> = records
            .iter()
            .filter(|record| {
                record.id == patient.id
                    || record
                        .current()
                        .and_then(|body| body["subject"]["reference"].as_str())
                        .is_some_and(|reference| reference == format!("Patient/{}", patient.id))
            })
            .cloned()
            .collect();
        let (expected, undecidable) = verify::built(&subject, &patient.id);
        let answered = verify::everything(&address(port), "localhost", &patient.id)
            .expect("$everything is answered");

        let missing: Vec<&String> = expected.difference(&answered).collect();
        assert!(
            missing.is_empty(),
            "$everything for {} omitted {missing:?} (undecidable here: {undecidable:?})",
            patient.id
        );
        for named in &answered {
            assert!(
                expected.contains(named) || undecidable.iter().any(|t| named.starts_with(t)),
                "$everything for {} returned {named}, which it did not build",
                patient.id
            );
        }
    }
    stop(child);
}

#[test]
fn a_clinical_date_filter_selects_by_the_time_the_generator_assigned() {
    let (child, port) = spawn_server();
    let records = cohort(12, 431);
    loaded(port, &records);
    let at = &address(port);

    let _ = at;
    let count = |query: &str| -> usize {
        let reply = request(port, "GET", query, &[], &[]);
        assert_eq!(reply.status, 200, "{query}: {}", reply.body);
        let bundle: serde_json::Value =
            serde_json::from_str(&reply.body).expect("a search answers a bundle");
        bundle["total"].as_u64().expect("a total") as usize
    };
    let total = count("/Observation?_summary=count");
    let before = count("/Observation?date=lt2020-01-01&_summary=count");
    let after = count("/Observation?date=ge2020-01-01&_summary=count");

    assert!(total > 0, "the population loaded no observation");
    assert_eq!(
        before + after,
        total,
        "a date filter lost or double-counted observations"
    );
    assert!(
        before > 0 && after > 0,
        "the observations all fall one side of the boundary: {before} before, {after} after"
    );

    let stamped_recently = count("/Observation?_lastUpdated=gt2025-01-01&_summary=count");
    assert_eq!(
        stamped_recently, total,
        "meta.lastUpdated is server-owned and should reflect the load"
    );
    stop(child);
}
