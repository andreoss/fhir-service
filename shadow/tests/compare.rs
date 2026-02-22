use fhir_shadow::case::{Answer, Case, Plan};
use fhir_shadow::compare::compare;
use fhir_shadow::gate::{Class, Gate, Severity, Verdict};
use fhir_shadow::runner::{shadow, Side};
use std::cell::RefCell;

struct Fixed {
    label: String,
    answers: RefCell<Vec<Result<Answer, String>>>,
}

impl Fixed {
    fn of(label: &str, answers: Vec<Result<Answer, String>>) -> Fixed {
        answers.iter().for_each(|_| ());
        Fixed {
            label: label.to_owned(),
            answers: RefCell::new(answers),
        }
    }
}

impl Side for Fixed {
    fn label(&self) -> &str {
        &self.label
    }

    fn send(&self, _case: &Case) -> Result<Answer, String> {
        let mut held = self.answers.borrow_mut();
        if held.is_empty() {
            return Err("the script ran out".to_owned());
        }
        held.remove(0)
    }
}

fn plan_of(shapes: &[(&'static str, &str, &'static str, &str)]) -> Plan {
    Plan::of(
        shapes
            .iter()
            .map(|(shape, name, method, path)| Case {
                shape,
                name: (*name).to_owned(),
                method,
                path: (*path).to_owned(),
                headers: Vec::new(),
                body: String::new(),
            })
            .collect(),
    )
}

fn answer(status: u16, headers: &[(&str, &str)], body: &str) -> Result<Answer, String> {
    Ok(Answer {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        body: body.to_owned(),
    })
}

fn one(left: Result<Answer, String>, right: Result<Answer, String>) -> Vec<(Class, Severity)> {
    let plan = plan_of(&[("read", "only", "GET", "/Patient/one")]);
    let run = shadow(
        &plan,
        &Fixed::of("candidate", vec![left]),
        &Fixed::of("incumbent", vec![right]),
    )
    .expect("the sides differ");
    compare(&plan, &run, &Gate::agreed())
        .differences
        .iter()
        .map(|difference| (difference.class, difference.severity))
        .collect()
}

fn classes(left: Result<Answer, String>, right: Result<Answer, String>) -> Vec<Class> {
    one(left, right)
        .into_iter()
        .map(|(class, _)| class)
        .collect()
}

const PATIENT: &str = "{\"resourceType\":\"Patient\",\"id\":\"one\",\"active\":true,\
                       \"meta\":{\"versionId\":\"1\",\"lastUpdated\":\"2020-01-01T00:00:00Z\"}}";

#[test]
fn matching_answers_hold_no_difference() {
    assert!(classes(
        answer(200, &[("etag", "W/\"1\"")], PATIENT),
        answer(200, &[("etag", "W/\"1\"")], PATIENT)
    )
    .is_empty());
}

#[test]
fn a_different_response_code_is_a_status_difference() {
    assert_eq!(
        classes(answer(200, &[], PATIENT), answer(201, &[], PATIENT)),
        vec![Class::Status]
    );
}

#[test]
fn a_different_issue_code_is_an_outcome_difference() {
    let refused = |code: &str| {
        format!(
            "{{\"resourceType\":\"OperationOutcome\",\"issue\":[\
             {{\"severity\":\"error\",\"code\":\"{code}\"}}]}}"
        )
    };
    assert_eq!(
        classes(
            answer(400, &[], &refused("invalid")),
            answer(400, &[], &refused("not-supported"))
        ),
        vec![Class::OutcomeIssue]
    );
}

#[test]
fn a_changed_field_is_a_content_difference() {
    let other = PATIENT.replace("\"active\":true", "\"active\":false");
    assert_eq!(
        classes(answer(200, &[], PATIENT), answer(200, &[], &other)),
        vec![Class::Content]
    );
}

#[test]
fn a_differing_modification_instant_is_not_a_content_difference() {
    let later = PATIENT.replace("2020-01-01T00:00:00Z", "2026-09-07T10:11:12Z");
    assert!(classes(answer(200, &[], PATIENT), answer(200, &[], &later)).is_empty());
}

#[test]
fn a_differing_version_is_a_concurrency_difference() {
    let second = PATIENT.replace("\"versionId\":\"1\"", "\"versionId\":\"2\"");
    assert_eq!(
        classes(
            answer(200, &[("etag", "W/\"1\"")], PATIENT),
            answer(200, &[("etag", "W/\"2\"")], &second)
        ),
        vec![Class::Concurrency]
    );
}

#[test]
fn a_missing_version_tag_is_a_concurrency_difference() {
    assert_eq!(
        classes(
            answer(200, &[("etag", "W/\"1\"")], PATIENT),
            answer(200, &[], PATIENT)
        ),
        vec![Class::Concurrency]
    );
}

#[test]
fn a_missing_modification_instant_is_a_concurrency_difference() {
    assert_eq!(
        classes(
            answer(
                200,
                &[("last-modified", "Wed, 01 Jan 2020 00:00:00 GMT")],
                PATIENT
            ),
            answer(200, &[], PATIENT)
        ),
        vec![Class::Concurrency]
    );
}

fn searchset(ids: &[&str], links: &[&str], total: usize) -> String {
    let entries: Vec<String> = ids
        .iter()
        .map(|id| {
            format!(
                "{{\"fullUrl\":\"http://left:1/Patient/{id}\",\
                 \"resource\":{{\"resourceType\":\"Patient\",\"id\":\"{id}\"}},\
                 \"search\":{{\"mode\":\"match\"}}}}"
            )
        })
        .collect();
    let relations: Vec<String> = links
        .iter()
        .map(|relation| {
            format!("{{\"relation\":\"{relation}\",\"url\":\"http://left:1/Patient?ct=abc\"}}")
        })
        .collect();
    format!(
        "{{\"resourceType\":\"Bundle\",\"id\":\"bundle-one\",\"type\":\"searchset\",\
         \"total\":{total},\"link\":[{}],\"entry\":[{}]}}",
        relations.join(","),
        entries.join(",")
    )
}

#[test]
fn a_missing_match_is_a_search_set_difference() {
    assert_eq!(
        classes(
            answer(200, &[], &searchset(&["a", "b"], &["self"], 2)),
            answer(200, &[], &searchset(&["a"], &["self"], 1))
        ),
        vec![Class::SearchSet]
    );
}

#[test]
fn a_reordered_match_is_a_search_order_difference() {
    assert_eq!(
        classes(
            answer(200, &[], &searchset(&["a", "b"], &["self"], 2)),
            answer(200, &[], &searchset(&["b", "a"], &["self"], 2))
        ),
        vec![Class::SearchOrder]
    );
}

#[test]
fn a_missing_walk_forward_is_a_paging_difference() {
    assert_eq!(
        classes(
            answer(200, &[], &searchset(&["a"], &["self", "next"], 2)),
            answer(200, &[], &searchset(&["a"], &["self"], 2))
        ),
        vec![Class::PagingLink]
    );
}

#[test]
fn an_opaque_continuation_token_is_not_a_paging_difference() {
    let ours = searchset(&["a"], &["self", "next"], 2);
    let theirs = ours.replace("ct=abc", "ct=a-very-different-token");
    assert!(classes(answer(200, &[], &ours), answer(200, &[], &theirs)).is_empty());
}

#[test]
fn a_differing_address_alone_is_not_a_difference() {
    let ours = searchset(&["a"], &["self"], 1);
    let theirs = ours.replace("http://left:1", "http://right:2");
    assert!(classes(answer(200, &[], &ours), answer(200, &[], &theirs)).is_empty());
}

fn batch(statuses: &[&str]) -> String {
    let entries: Vec<String> = statuses
        .iter()
        .map(|status| format!("{{\"response\":{{\"status\":\"{status}\"}}}}"))
        .collect();
    format!(
        "{{\"resourceType\":\"Bundle\",\"type\":\"batch-response\",\"entry\":[{}]}}",
        entries.join(",")
    )
}

#[test]
fn a_different_entry_outcome_is_a_bundle_entry_difference() {
    assert_eq!(
        classes(
            answer(200, &[], &batch(&["201 Created", "404 Not Found"])),
            answer(200, &[], &batch(&["201 Created", "200 OK"]))
        ),
        vec![Class::BundleEntry]
    );
}

#[test]
fn an_unanswered_case_is_unreached_and_is_not_compared() {
    let plan = plan_of(&[("read", "only", "GET", "/Patient/one")]);
    let run = shadow(
        &plan,
        &Fixed::of("candidate", vec![answer(200, &[], PATIENT)]),
        &Fixed::of("incumbent", vec![Err("no connection".to_owned())]),
    )
    .expect("the sides differ");
    let report = compare(&plan, &run, &Gate::agreed());
    assert_eq!(report.differences.len(), 1);
    assert_eq!(report.differences[0].class, Class::Unreached);
    assert_eq!(report.facts.cases_compared, 0);
    assert_eq!(report.facts.cases_planned, 1);
    assert!(matches!(report.verdict, Verdict::Failed(_)));
}

#[test]
fn the_software_naming_itself_is_recorded_and_does_not_block() {
    let statement = |name: &str| {
        format!(
            "{{\"resourceType\":\"CapabilityStatement\",\"status\":\"active\",\
             \"date\":\"2026-01-01T00:00:00Z\",\"fhirVersion\":\"4.0.1\",\
             \"software\":{{\"name\":\"{name}\",\"version\":\"9.9\"}},\
             \"rest\":[{{\"mode\":\"server\"}}]}}"
        )
    };
    let found = one(
        answer(200, &[], &statement("one")),
        answer(200, &[], &statement("another")),
    );
    assert_eq!(found, vec![(Class::ServerIdentity, Severity::Noted)]);
}

#[test]
fn a_declared_capability_that_differs_still_blocks() {
    let statement = |create: bool| {
        format!(
            "{{\"resourceType\":\"CapabilityStatement\",\"status\":\"active\",\
             \"date\":\"2026-01-01T00:00:00Z\",\"fhirVersion\":\"4.0.1\",\
             \"software\":{{\"name\":\"one\",\"version\":\"9.9\"}},\
             \"rest\":[{{\"mode\":\"server\",\"resource\":[{{\"type\":\"Patient\",\
             \"updateCreate\":{create}}}]}}]}}"
        )
    };
    assert_eq!(
        classes(
            answer(200, &[], &statement(true)),
            answer(200, &[], &statement(false))
        ),
        vec![Class::Content]
    );
}

#[test]
fn the_same_content_written_differently_is_recorded_not_blocking() {
    let spaced = "{ \"resourceType\" : \"Patient\" , \"id\" : \"one\" , \"active\" : true ,\
                  \"meta\" : { \"versionId\" : \"1\" , \"lastUpdated\" : \"2020-01-01T00:00:00Z\" } }";
    assert_eq!(
        one(answer(200, &[], PATIENT), answer(200, &[], spaced)),
        vec![(Class::Representation, Severity::Noted)]
    );
}

#[test]
fn a_body_that_is_not_json_against_one_that_is_is_a_content_difference() {
    assert_eq!(
        classes(
            answer(200, &[], PATIENT),
            answer(200, &[], "not json at all")
        ),
        vec![Class::Content]
    );
}

#[test]
fn every_difference_names_the_request_that_produced_it() {
    let plan = plan_of(&[("read", "the-case", "GET", "/Patient/one?_summary=true")]);
    let run = shadow(
        &plan,
        &Fixed::of("candidate", vec![answer(200, &[], PATIENT)]),
        &Fixed::of("incumbent", vec![answer(404, &[], PATIENT)]),
    )
    .expect("the sides differ");
    let report = compare(&plan, &run, &Gate::agreed());
    let difference = &report.differences[0];
    assert_eq!(difference.case, "the-case");
    assert_eq!(difference.shape, "read");
    assert_eq!(difference.request.method, "GET");
    assert_eq!(difference.request.path, "/Patient/one?_summary=true");
    let rendered = report.render();
    assert!(rendered.contains("GET /Patient/one?_summary=true"));
    assert!(rendered.contains(&Gate::agreed().fingerprint()));
    assert!(rendered.contains("candidate"));
    assert!(rendered.contains("incumbent"));
}

#[test]
fn a_full_matching_run_of_the_agreed_plan_passes_the_gate() {
    let plan = Plan::agreed();
    let script = || {
        plan.cases()
            .iter()
            .map(|_| answer(200, &[("etag", "W/\"1\"")], PATIENT))
            .collect::<Vec<Result<Answer, String>>>()
    };
    let run = shadow(
        &plan,
        &Fixed::of("candidate", script()),
        &Fixed::of("incumbent", script()),
    )
    .expect("the sides differ");
    let report = compare(&plan, &run, &Gate::agreed());
    assert!(report.differences.is_empty());
    assert_eq!(report.verdict, Verdict::Passed, "{}", report.render());
}

struct Based {
    label: String,
    base: String,
    answers: RefCell<Vec<Result<Answer, String>>>,
}

impl Based {
    fn of(label: &str, base: &str, answers: Vec<Result<Answer, String>>) -> Based {
        Based {
            label: label.to_owned(),
            base: base.to_owned(),
            answers: RefCell::new(answers),
        }
    }
}

impl Side for Based {
    fn label(&self) -> &str {
        &self.label
    }

    fn base(&self) -> &str {
        &self.base
    }

    fn send(&self, _case: &Case) -> Result<Answer, String> {
        let mut held = self.answers.borrow_mut();
        if held.is_empty() {
            return Err("the script ran out".to_owned());
        }
        held.remove(0)
    }
}

#[test]
fn the_path_a_side_serves_from_is_no_part_of_its_answer() {
    let plan = plan_of(&[("search", "only", "GET", "/Patient?_id=one")]);
    let ours = answer(
        200,
        &[("location", "http://one/Patient/one/_history/1")],
        "{\"resourceType\":\"Bundle\",\"type\":\"searchset\",\"entry\":[\
         {\"fullUrl\":\"http://one/Patient/one\",\
         \"resource\":{\"resourceType\":\"Patient\",\"id\":\"one\"}}]}",
    );
    let theirs = answer(
        200,
        &[("location", "http://two/base/Patient/one/_history/1")],
        "{\"resourceType\":\"Bundle\",\"type\":\"searchset\",\"entry\":[\
         {\"fullUrl\":\"http://two/base/Patient/one\",\
         \"resource\":{\"resourceType\":\"Patient\",\"id\":\"one\"}}]}",
    );
    let run = shadow(
        &plan,
        &Based::of("candidate", "", vec![ours]),
        &Based::of("incumbent", "/base", vec![theirs]),
    )
    .expect("the sides differ");
    let report = compare(&plan, &run, &Gate::agreed());
    assert!(
        report.differences.is_empty(),
        "{:?}",
        report
            .differences
            .iter()
            .map(|difference| difference.detail.clone())
            .collect::<Vec<String>>()
    );
}

#[test]
fn an_excused_difference_is_reported_and_does_not_block() {
    let plan = plan_of(&[(
        "search-unknown-parameter",
        "search-unknown",
        "GET",
        "/Patient?nosuch=1",
    )]);
    let ours = answer(400, &[], "{\"resourceType\":\"OperationOutcome\",\"issue\":[{\"severity\":\"error\",\"code\":\"not-supported\"}]}");
    let theirs = answer(200, &[], "{\"resourceType\":\"OperationOutcome\",\"issue\":[{\"severity\":\"error\",\"code\":\"processing\"}]}");
    let run = shadow(
        &plan,
        &Fixed::of("candidate", vec![ours]),
        &Fixed::of("incumbent", vec![theirs]),
    )
    .expect("the sides differ");
    let report = compare(&plan, &run, &Gate::agreed());
    assert!(
        report
            .differences
            .iter()
            .any(|difference| difference.excused.is_some()),
        "the difference is not reported at all"
    );
    for difference in &report.differences {
        assert!(
            difference.excused.is_some(),
            "{} was not excused",
            difference.detail
        );
        assert!(difference
            .excused
            .as_ref()
            .expect("an excused difference")
            .contains("SRCH-09"));
    }
    assert!(
        report
            .facts
            .tallies
            .iter()
            .all(|tally| tally.class != Class::Status),
        "an excused difference was counted against the gate"
    );
}
