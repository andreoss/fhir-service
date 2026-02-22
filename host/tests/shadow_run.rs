mod live;

use fhir_shadow::case::{Case, Plan};
use fhir_shadow::runner::{drive, shadow, HttpSide, Side, Targets};
use live::{spawn_server, stop};

fn probe(shape: &'static str, name: &str, path: &str) -> Case {
    Case {
        shape,
        name: name.to_owned(),
        method: "GET",
        path: path.to_owned(),
        headers: Vec::new(),
        body: String::new(),
    }
}

#[test]
fn a_run_against_one_side_leaves_the_other_untouched() {
    let (left_child, left_port) = spawn_server();
    let (right_child, right_port) = spawn_server();
    let targets = Targets::of(("127.0.0.1", left_port), ("127.0.0.1", right_port))
        .expect("two instances are two addresses");
    let left = HttpSide::of("candidate-left", targets.left().0, targets.left().1);
    let right = HttpSide::of("candidate-right", targets.right().0, targets.right().1);

    let plan = Plan::agreed();
    let run = drive(&plan, &left);

    let seen = right.send(&probe("read", "probe-read", "/Patient/shdw-p2"));
    let searched = right.send(&probe("search", "probe-search", "/Patient?_id=shdw-p2"));
    let seen_left = left.send(&probe("read", "probe-read", "/Patient/shdw-p2"));
    stop(left_child);
    stop(right_child);

    assert!(
        run.answers.iter().all(|answer| answer.is_ok()),
        "the driven side did not answer every case"
    );
    let seen_left = seen_left.expect("the driven side answers");
    assert_eq!(seen_left.status, 200, "the run wrote nothing at all");
    let seen = seen.expect("the untouched side answers");
    assert_eq!(seen.status, 404, "the run reached the other side");
    let searched = searched.expect("the untouched side answers");
    let bundle: serde_json::Value =
        serde_json::from_str(&searched.body).expect("a search answers with a bundle");
    assert_eq!(
        bundle["entry"].as_array().map(|entry| entry.len()),
        None,
        "the run left matches on the other side"
    );
}

#[test]
fn both_sides_answer_every_case_of_the_plan() {
    let (left_child, left_port) = spawn_server();
    let (right_child, right_port) = spawn_server();
    let left = HttpSide::of("candidate-left", "127.0.0.1", left_port);
    let right = HttpSide::of("candidate-right", "127.0.0.1", right_port);
    let plan = Plan::agreed();
    let run = shadow(&plan, &left, &right).expect("the sides differ");
    stop(left_child);
    stop(right_child);

    for (index, case) in plan.cases().iter().enumerate() {
        assert!(
            run.left.answers[index].is_ok(),
            "{} was unanswered on the left",
            case.name
        );
        assert!(
            run.right.answers[index].is_ok(),
            "{} was unanswered on the right",
            case.name
        );
    }
}
