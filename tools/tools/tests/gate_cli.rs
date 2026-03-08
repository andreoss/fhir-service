mod support;

use fhir_tools::coverage;

fn report(percent: f64) -> String {
    format!(
        "{{\"type\":\"llvm.coverage.json.export\",\"data\":[{{\"totals\":{{\"lines\":{{\"count\":100,\"covered\":90,\"percent\":{percent}}}}}}}]}}"
    )
}

#[test]
fn the_lines_covered_are_read_from_the_report() {
    assert_eq!(
        coverage::lines_percent(&report(93.15)).expect("a report reads"),
        93.15
    );
    assert!(coverage::lines_percent("{}").is_err());
    assert!(coverage::lines_percent("not a report").is_err());
    assert!(coverage::lines_percent("{\"data\":[]}").is_err());
}

#[test]
fn the_gate_reports_what_it_takes() {
    let (code, text) = support::tool(env!("CARGO_BIN_EXE_gate"), &[], "fhir");
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("usage"), "{text}");
}

#[test]
fn cover_below_the_gate_fails_the_build() {
    let root = support::scratch("gate");
    let low = root.join("low.json");
    let high = root.join("high.json");
    std::fs::write(&low, report(84.99)).expect("the report is written");
    std::fs::write(&high, report(85.0)).expect("the report is written");

    let (code, text) = support::tool(
        env!("CARGO_BIN_EXE_gate"),
        &[low.to_str().expect("a usable path"), "85"],
        "fhir",
    );
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("84.99"), "{text}");
    assert!(text.contains("85"), "{text}");

    let (code, text) = support::tool(
        env!("CARGO_BIN_EXE_gate"),
        &[high.to_str().expect("a usable path"), "85"],
        "fhir",
    );
    assert_eq!(code, 0, "{text}");

    let (code, text) = support::tool(
        env!("CARGO_BIN_EXE_gate"),
        &["scratch/absent-report.json", "85"],
        "fhir",
    );
    assert_eq!(code, 1, "{text}");

    std::fs::remove_dir_all(&root).expect("the scratch directory is removed");
}

#[test]
fn the_gate_the_pipeline_carries_is_the_one_the_project_holds() {
    let definition = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("ci")
            .join("pipeline.yaml"),
    )
    .expect("the pipeline definition is present");
    let held = fhir_tools::pipeline::Pipeline::parse(&definition).expect("the definition parses");
    let coverage = held
        .stages
        .iter()
        .find(|stage| stage.name == "coverage")
        .expect("a coverage stage");
    assert_eq!(coverage.gate, Some(coverage::REQUIRED));
    assert!(coverage.run.contains("gate"), "{}", coverage.run);
}

#[test]
fn a_comment_in_the_definition_is_not_a_field() {
    
    
    let held = fhir_tools::pipeline::Pipeline::parse(
        "stages:\n  # why this stage is here\n  - name: build\n    run: cargo build\n",
    )
    .expect("a comment is skipped");
    assert_eq!(held.stages.len(), 1);
    assert_eq!(held.stages[0].name, "build");
}
