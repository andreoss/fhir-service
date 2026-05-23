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
fn the_pipeline_starts_the_engines_then_builds_tests_judges_and_publishes() {
    let held = Pipeline::parse(&definition()).expect("the definition parses");
    let names: Vec<&str> = held
        .stages
        .iter()
        .map(|stage| stage.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "engines",
            "build",
            "test",
            "analysis",
            "format",
            "dependencies",
            "hardening",
            "bill of materials",
            "coverage",
            "conformance",
            "publish",
            "image record"
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

fn workflow() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(".github")
        .join("workflows")
        .join("ci.yaml");
    std::fs::read_to_string(&path).expect("the hosted workflow is present")
}

#[test]
fn the_hosted_run_is_the_run_the_definition_describes() {
    let held = workflow();
    assert!(
        held.contains("ci/run.sh"),
        "the hosted workflow drives the definition: {held}"
    );
    for line in held.lines() {
        let Some(command) = line.trim().strip_prefix("run:") else {
            continue;
        };
        let command = command.trim();
        assert!(
            !command.starts_with("cargo") && !command.starts_with("docker"),
            "a stage the definition does not carry: {command}"
        );
    }
}

#[test]
fn the_engines_are_up_before_anything_is_judged() {
    let held = Pipeline::parse(&definition()).expect("the definition parses");
    let first = held.stages.first().expect("a first stage");
    assert_eq!(first.name, "engines");
    assert!(first.run.contains("compose"), "{}", first.run);
    let test = held
        .stages
        .iter()
        .position(|stage| stage.name == "test")
        .expect("a test stage");
    assert!(test > 0);
}

const CARGO_ITSELF: [&str; 6] = ["build", "test", "clippy", "fmt", "run", "update"];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn tools_named(run: &str) -> Vec<String> {
    let words: Vec<&str> = run.split_whitespace().collect();
    let mut named = Vec::new();
    for (at, word) in words.iter().enumerate() {
        match *word {
            "cargo" => {
                if let Some(next) = words.get(at + 1) {
                    if !CARGO_ITSELF.contains(next) && !next.starts_with('-') {
                        named.push(format!("cargo-{next}"));
                    }
                }
            }
            "sh" => {
                let Some(script) = words.get(at + 1) else {
                    continue;
                };
                let Ok(text) = std::fs::read_to_string(root().join(script)) else {
                    continue;
                };
                for line in text.lines() {
                    if let Some(rest) = line.trim().strip_prefix("command -v ") {
                        if let Some(name) = rest.split_whitespace().next() {
                            named.push(name.to_owned());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    named
}

#[test]
fn the_hosted_run_carries_every_tool_the_definition_names() {
    let held = Pipeline::parse(&definition()).expect("the definition parses");
    let hosted = workflow();
    let named: Vec<String> = held
        .stages
        .iter()
        .flat_map(|stage| tools_named(&stage.run))
        .collect();
    for expected in ["trivy", "cargo-deny", "cargo-cyclonedx", "cargo-llvm-cov"] {
        assert!(
            named.iter().any(|tool| tool == expected),
            "the definition names {expected}: {named:?}"
        );
    }
    for tool in &named {
        assert!(
            hosted.contains(tool.as_str()),
            "the hosted run installs nothing named {tool}: {hosted}"
        );
    }
}
