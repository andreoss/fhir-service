use fhir_tools::compartments;
use std::path::Path;

const ARTIFACTS: &str = "../../scratch/spec";
const COMMITTED: &str = "../../core/src/search/compartments.rs";

fn artifacts_present() -> bool {
    Path::new(ARTIFACTS)
        .join("R4-profiles-resources.json")
        .exists()
}

#[test]
fn the_committed_tables_are_what_the_published_definitions_produce() {
    if !artifacts_present() {
        eprintln!("skipped: {ARTIFACTS} holds no profiles-resources artifact");
        return;
    }
    let held = compartments::read(Path::new(ARTIFACTS)).expect("the artifacts are readable");
    let generated = compartments::source(&held);
    let committed = std::fs::read_to_string(COMMITTED).expect("the committed tables are readable");
    assert_eq!(
        generated, committed,
        "the committed compartment tables are not what the artifacts produce; \
         regenerate with `compartments {ARTIFACTS} {COMMITTED}`"
    );
}

#[test]
fn the_patient_compartment_gathers_what_the_publication_says_it_gathers() {
    if !artifacts_present() {
        eprintln!("skipped: {ARTIFACTS} holds no profiles-resources artifact");
        return;
    }
    let held = compartments::read(Path::new(ARTIFACTS)).expect("the artifacts are readable");
    let patient = held
        .get("Patient")
        .expect("the Patient compartment is published");
    for (named, param) in [
        ("Condition", "patient"),
        ("Procedure", "patient"),
        ("MedicationRequest", "subject"),
    ] {
        let params = patient
            .get(named)
            .unwrap_or_else(|| panic!("the publication gathers {named} into Patient"));
        assert!(
            params.contains(param),
            "{named} is gathered by {params:?}, not {param}"
        );
    }
    assert!(
        patient.len() > 50,
        "the publication gathers {} types into Patient",
        patient.len()
    );
}
