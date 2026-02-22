use fhir_shadow::case::{Answer, Case, Plan};
use fhir_shadow::gate::Gate;
use fhir_shadow::runner::{address, shadow, Side, Targets};
use std::cell::RefCell;

struct Scripted {
    label: String,
    seen: RefCell<Vec<String>>,
    etag: String,
}

impl Scripted {
    fn of(label: &str, etag: &str) -> Scripted {
        Scripted {
            label: label.to_owned(),
            seen: RefCell::new(Vec::new()),
            etag: etag.to_owned(),
        }
    }
}

impl Side for Scripted {
    fn label(&self) -> &str {
        &self.label
    }

    fn send(&self, case: &Case) -> Result<Answer, String> {
        self.seen.borrow_mut().push(format!(
            "{} {} {} {}",
            case.name,
            case.method,
            case.path,
            case.headers
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<String>>()
                .join(",")
        ));
        Ok(Answer {
            status: 200,
            headers: vec![("etag".to_owned(), self.etag.clone())],
            body: String::new(),
        })
    }
}

#[test]
fn the_plan_exercises_every_shape_the_gate_agreed() {
    let plan = Plan::agreed();
    let shapes = plan.shapes();
    for agreed in Gate::agreed().must_cover() {
        assert!(shapes.contains(agreed), "{agreed} is not in the plan");
    }
    assert!(plan.cases().len() >= Gate::agreed().least_cases());
}

#[test]
fn every_case_carries_a_distinct_name() {
    let plan = Plan::agreed();
    let mut names: Vec<&str> = plan.cases().iter().map(|case| case.name.as_str()).collect();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    assert_eq!(names.len(), count);
}

#[test]
fn the_plan_assigns_every_identifier_itself() {
    for case in Plan::agreed().cases() {
        if case.method == "POST" && case.path == "/Patient" {
            continue;
        }
        assert!(
            !case.path.contains("{"),
            "{} leaves an identifier to the server",
            case.name
        );
    }
}

#[test]
fn a_placeholder_resolves_from_the_side_that_answered_it() {
    let plan = Plan::agreed();
    let left = Scripted::of("left", "W/\"1\"");
    let right = Scripted::of("right", "W/\"7\"");
    let run = shadow(&plan, &left, &right).expect("the sides differ");
    assert_eq!(run.left.answers.len(), plan.cases().len());
    assert_eq!(run.right.answers.len(), plan.cases().len());
    let sent = |side: &Scripted, name: &str| {
        side.seen
            .borrow()
            .iter()
            .find(|line| line.starts_with(&format!("{name} ")))
            .cloned()
            .unwrap_or_default()
    };
    assert!(sent(&left, "update-p1").contains("if-match=W/\"1\""));
    assert!(sent(&right, "update-p1").contains("if-match=W/\"7\""));
    assert!(!left.seen.borrow().join("\n").contains("W/\"7\""));
}

#[test]
fn no_placeholder_survives_into_a_request() {
    let plan = Plan::agreed();
    let left = Scripted::of("left", "W/\"1\"");
    let right = Scripted::of("right", "W/\"1\"");
    let _ = shadow(&plan, &left, &right).expect("the sides differ");
    for seen in left.seen.borrow().iter().chain(right.seen.borrow().iter()) {
        assert!(!seen.contains("{etag:"), "{seen}");
    }
}

#[test]
fn two_sides_at_one_address_are_refused() {
    let same = Targets::of(("127.0.0.1", 8080), ("127.0.0.1", 8080));
    assert!(same.is_err(), "one address cannot be two independent sides");
    assert!(Targets::of(("127.0.0.1", 8080), ("127.0.0.1", 8081)).is_ok());
}

#[test]
fn two_sides_with_one_label_are_refused() {
    let left = Scripted::of("same", "W/\"1\"");
    let right = Scripted::of("same", "W/\"1\"");
    assert!(shadow(&Plan::agreed(), &left, &right).is_err());
}

#[test]
fn an_address_may_name_the_path_a_side_serves_from() {
    let named = address("127.0.0.1:8080/fhir").expect("an address naming a prefix");
    assert_eq!(named, ("127.0.0.1".to_owned(), 8080, "/fhir".to_owned()));
    let bare = address("127.0.0.1:8081").expect("an address naming none");
    assert_eq!(bare, ("127.0.0.1".to_owned(), 8081, String::new()));
    assert!(address("127.0.0.1").is_none(), "a port is required");
    assert!(address("127.0.0.1:none").is_none(), "a port is a number");
}
