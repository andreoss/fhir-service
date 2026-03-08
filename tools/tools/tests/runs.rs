mod support;

use fhir_tools::generate;
use fhir_tools::measure::{compare, Run};

#[test]
fn a_supply_is_the_same_every_time_it_is_asked_for() {
    let left = generate::rows(4, "Patient", 7).expect("the supply is generated");
    let right = generate::rows(4, "Patient", 7).expect("the supply is generated");
    assert_eq!(left, right);
    assert_ne!(left, generate::rows(4, "Patient", 8).expect("another seed"));

    let ids: Vec<String> = left
        .lines()
        .map(|row| {
            let value: serde_json::Value = serde_json::from_str(row).expect("a row is a resource");
            assert_eq!(value["resourceType"], "Patient");
            value["id"]
                .as_str()
                .expect("a row carries an id")
                .to_owned()
        })
        .collect();
    assert_eq!(ids.len(), 4);
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 4);
}

#[test]
fn an_unknown_type_is_refused_before_anything_runs() {
    assert!(generate::rows(1, "NotAType", 1).is_err());
}

#[test]
fn a_run_reports_latency_and_throughput() {
    let run = Run::new("left", "create", vec![1_000, 2_000, 3_000, 4_000]);
    let summary = run.summary();
    assert_eq!(summary.count, 4);
    assert_eq!(summary.p50, 2_000);
    assert_eq!(summary.p95, 4_000);
    assert_eq!(summary.p99, 4_000);
    assert!(summary.per_second > 0.0);

    let text = summary.to_json();
    let read = fhir_tools::measure::Summary::parse(&text).expect("a summary reads back");
    assert_eq!(read.label, "left");
    assert_eq!(read.operation, "create");
    assert_eq!(read.p50, summary.p50);
}

#[test]
fn two_runs_of_one_shape_compare_and_others_are_refused() {
    let left = Run::new("left", "create", vec![2_000; 8]).summary();
    let right = Run::new("right", "create", vec![1_000; 8]).summary();
    let judged = compare(&left, &right).expect("runs of one shape compare");
    assert!(judged.ratio > 1.5, "{}", judged.ratio);
    assert_eq!(judged.verdict, "faster");

    let level = compare(&left, &Run::new("same", "create", vec![2_000; 8]).summary())
        .expect("runs of one shape compare");
    assert_eq!(level.verdict, "level");

    let other = Run::new("right", "read", vec![1_000; 8]).summary();
    assert!(compare(&left, &other).is_err());
    let shorter = Run::new("right", "create", vec![1_000; 4]).summary();
    assert!(compare(&left, &shorter).is_err());
}

#[tokio::test]
async fn a_measured_run_writes_a_record_that_compares() {
    let Some(pool) = support::engine().await else {
        return;
    };
    let namespace = support::namespace("measure");
    support::prepared(&pool, &namespace).await;
    let root = support::scratch("measure");
    let left = root.join("left.json");
    let right = root.join("right.json");

    for target in [&left, &right] {
        let (code, text) = support::tool(
            env!("CARGO_BIN_EXE_measure"),
            &["run", "8", target.to_str().expect("a usable path")],
            namespace.as_str(),
        );
        assert_eq!(code, 0, "{text}");
    }

    let (code, text) = support::tool(
        env!("CARGO_BIN_EXE_measure"),
        &[
            "compare",
            left.to_str().expect("a usable path"),
            right.to_str().expect("a usable path"),
        ],
        namespace.as_str(),
    );
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("ratio"), "{text}");

    std::fs::remove_dir_all(&root).expect("the scratch directory is removed");
    support::drop_namespace(&pool, &namespace).await;
}

#[test]
fn every_command_reports_what_it_takes() {
    for binary in [
        env!("CARGO_BIN_EXE_measure"),
        env!("CARGO_BIN_EXE_instances"),
        env!("CARGO_BIN_EXE_generate"),
    ] {
        let (code, text) = support::tool(binary, &[], "fhir");
        assert_eq!(code, 2, "{text}");
        assert!(text.contains("usage"), "{text}");
    }
}

#[test]
fn instances_answer_side_by_side_and_are_all_stopped() {
    let Some(binary) = support::service_binary() else {
        return;
    };
    let (code, text) = support::tool_with(
        env!("CARGO_BIN_EXE_instances"),
        &["2", binary.to_str().expect("a usable path")],
        "fhir",
        &[("FHIR_BACKEND", "memory")],
    );
    assert_eq!(code, 0, "{text}");
    assert_eq!(text.matches("\"status\":200").count(), 2, "{text}");
}
