mod support;

use fhir_tools::pipeline::Pipeline;
use std::path::PathBuf;

fn definition() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("ci")
        .join("pipeline.yaml");
    std::fs::read_to_string(&path).expect("the pipeline definition is present")
}

#[test]
fn the_pipeline_runs_build_tests_analysis_coverage_and_publish() {
    let held = Pipeline::parse(&definition()).expect("the definition parses");
    let names: Vec<&str> = held
        .stages
        .iter()
        .map(|stage| stage.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "build",
            "test",
            "analysis",
            "format",
            "dependencies",
            "bill of materials",
            "coverage",
            "publish"
        ]
    );
    for stage in &held.stages {
        assert!(!stage.run.trim().is_empty(), "{} runs nothing", stage.name);
    }
}

#[test]
fn the_pipeline_judges_what_it_builds_from_and_what_it_publishes() {
    let held = Pipeline::parse(&definition()).expect("the definition parses");
    let running = |name: &str| {
        held.stages
            .iter()
            .find(|stage| stage.name == name)
            .map(|stage| stage.run.clone())
            .unwrap_or_default()
    };
    let dependencies = running("dependencies");
    for judged in ["advisories", "licenses", "bans", "sources"] {
        assert!(
            dependencies.contains(judged),
            "the dependency stage judges {judged}: {dependencies}"
        );
    }
    assert!(
        running("bill of materials").contains("cyclonedx"),
        "a bill of materials is produced for the published binary"
    );
    assert!(running("format").contains("--check"));
}



#[test]
fn every_allowed_advisory_carries_a_reason() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deny.toml");
    let held = std::fs::read_to_string(&path).expect("the dependency policy is present");
    for line in held.lines().filter(|line| line.contains("id = \"RUSTSEC")) {
        assert!(line.contains("reason ="), "{line}");
        assert!(
            line.contains("Reviewed 20"),
            "an exception names the date it was reviewed: {line}"
        );
    }
    assert!(held.contains("[advisories]"));
    assert!(held.contains("[licenses]"));
    assert!(held.contains("[bans]"));
    assert!(held.contains("[sources]"));
}

#[test]
fn the_coverage_stage_carries_the_gate_that_fails_the_build() {
    let held = Pipeline::parse(&definition()).expect("the definition parses");
    let coverage = held
        .stages
        .iter()
        .find(|stage| stage.name == "coverage")
        .expect("a coverage stage");
    assert_eq!(coverage.gate, Some(85.0));
    assert!(held.stages.iter().any(|stage| stage.run.contains("clippy")));
}

#[test]
fn a_definition_without_a_command_is_refused() {
    assert!(Pipeline::parse("stages:\n  - name: build\n").is_err());
    assert!(Pipeline::parse("").is_err());
    assert!(Pipeline::parse("stages:\n  - run: true\n").is_err());
}

#[test]
fn the_runner_reports_what_it_takes() {
    let (code, text) = support::tool(env!("CARGO_BIN_EXE_pipeline"), &[], "fhir");
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("usage"), "{text}");
}

#[test]
fn a_failing_stage_stops_the_run() {
    let root = support::scratch("pipeline");
    let path = root.join("pipeline.yaml");
    std::fs::write(
        &path,
        "stages:\n  - name: first\n    run: true\n  - name: second\n    run: false\n  - name: third\n    run: echo reached\n",
    )
    .expect("the definition is written");

    let (code, text) = support::tool(
        env!("CARGO_BIN_EXE_pipeline"),
        &["run", path.to_str().expect("a usable path")],
        "fhir",
    );
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("first"), "{text}");
    assert!(text.contains("second"), "{text}");
    assert!(!text.contains("reached"), "{text}");

    let (code, text) = support::tool(
        env!("CARGO_BIN_EXE_pipeline"),
        &["list", path.to_str().expect("a usable path")],
        "fhir",
    );
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("third"), "{text}");

    std::fs::remove_dir_all(&root).expect("the scratch directory is removed");
}
