use fhir_core::{FhirVersion, Model};
use fhir_tools::generate;
use serde_json::Value;

fn rows(count: usize, seed: u64) -> Vec<Value> {
    generate::cohort(count, seed)
        .expect("a cohort generates")
        .lines()
        .map(|row| serde_json::from_str(row).expect("a row is a resource"))
        .collect()
}

#[test]
fn a_cohort_holds_more_than_one_type_per_subject() {
    let held = rows(4, 7);
    assert_eq!(held.len(), 4 * generate::COHORT_SHARE);
    let mut kinds: Vec<&str> = held
        .iter()
        .filter_map(|row| row.get("resourceType").and_then(Value::as_str))
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    assert!(kinds.len() >= 3, "a flat cohort is not a shape: {kinds:?}");
}

#[test]
fn every_generated_row_matches_the_definitions_of_every_version() {
    for version in FhirVersion::ALL {
        let model = Model::of(version);
        for row in rows(3, 11) {
            let findings = model.check(&row);
            assert!(
                findings.is_empty(),
                "{version:?} refused a generated row: {findings:?}"
            );
        }
    }
}

#[test]
fn a_dependent_row_names_the_subject_it_belongs_to() {
    let held = rows(2, 3);
    let subjects: Vec<String> = held
        .iter()
        .filter(|row| row["resourceType"] == "Patient")
        .map(|row| format!("Patient/{}", row["id"].as_str().expect("an id")))
        .collect();
    assert_eq!(subjects.len(), 2);
    let named = held
        .iter()
        .filter_map(|row| row.pointer("/subject/reference").and_then(Value::as_str))
        .filter(|reference| subjects.iter().any(|held| held == reference))
        .count();
    assert_eq!(named, held.len() - subjects.len());
}

#[test]
fn a_seed_fixes_the_whole_cohort() {
    let left = generate::cohort(2, 5).expect("a cohort generates");
    assert_eq!(left, generate::cohort(2, 5).expect("a cohort generates"));
    assert_ne!(left, generate::cohort(2, 6).expect("a cohort generates"));
    assert_eq!(
        generate::cohort(0, 5).expect("an empty cohort"),
        String::new()
    );
}
