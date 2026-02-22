mod live;

use fhir_shadow::case::Plan;
use fhir_shadow::compare::compare;
use fhir_shadow::gate::{Allowance, Class, Gate, Verdict};
use fhir_shadow::runner::{shadow, HttpSide};
use live::{spawn_server, spawn_with, stop};

#[test]
fn two_instances_of_one_build_hold_no_blocking_divergence() {
    let (left_child, left_port) = spawn_server();
    let (right_child, right_port) = spawn_server();
    let left = HttpSide::of("candidate-left", "127.0.0.1", left_port);
    let right = HttpSide::of("candidate-right", "127.0.0.1", right_port);
    let plan = Plan::agreed();
    let run = shadow(&plan, &left, &right).expect("the sides differ");
    let report = compare(&plan, &run, &Gate::agreed());
    stop(left_child);
    stop(right_child);

    let gate = Gate::agreed();
    let blocking: Vec<String> = report
        .differences
        .iter()
        .filter(|difference| gate.rule(difference.class).allowance == Allowance::Empty)
        .map(|difference| {
            format!(
                "{} {} {}",
                difference.case,
                difference.class.name(),
                difference.detail
            )
        })
        .collect();
    assert!(
        blocking.is_empty(),
        "one build diverged from itself: {blocking:#?}"
    );
    assert_eq!(report.facts.cases_compared, plan.cases().len());
    assert_eq!(report.verdict, Verdict::Passed, "{}", report.render());
}

#[test]
fn a_side_that_serves_another_release_is_caught_and_the_gate_fails() {
    let (left_child, left_port) = spawn_server();
    let (right_child, right_port) = spawn_with(&[("FHIR_VERSION", "STU3")]);
    let left = HttpSide::of("candidate-r4", "127.0.0.1", left_port);
    let right = HttpSide::of("candidate-stu3", "127.0.0.1", right_port);
    let plan = Plan::agreed();
    let run = shadow(&plan, &left, &right).expect("the sides differ");
    let report = compare(&plan, &run, &Gate::agreed());
    stop(left_child);
    stop(right_child);

    assert!(
        report
            .differences
            .iter()
            .any(|difference| difference.class == Class::Content),
        "no content divergence was found: {}",
        report.render()
    );
    assert!(
        matches!(report.verdict, Verdict::Failed(_)),
        "the gate passed two different releases: {}",
        report.render()
    );
    for difference in &report.differences {
        assert!(!difference.request.method.is_empty());
        assert!(difference.request.path.starts_with('/'));
    }
}
