use fhir_shadow::gate::{Allowance, Class, Gate, RunFacts, Severity, Tally, Verdict};

fn covered() -> Vec<String> {
    Gate::agreed()
        .must_cover()
        .iter()
        .map(|shape| (*shape).to_owned())
        .collect()
}

fn clean(cases: usize) -> RunFacts {
    RunFacts {
        cases_planned: cases,
        cases_compared: cases,
        covered: covered(),
        tallies: Vec::new(),
    }
}

#[test]
fn every_answer_class_must_be_empty() {
    let gate = Gate::agreed();
    for class in [
        Class::Status,
        Class::OutcomeIssue,
        Class::Content,
        Class::Concurrency,
        Class::SearchSet,
        Class::SearchOrder,
        Class::BundleEntry,
        Class::PagingLink,
        Class::Unreached,
    ] {
        assert_eq!(
            gate.rule(class).allowance,
            Allowance::Empty,
            "{} must be empty before a replacement",
            class.name()
        );
    }
}

#[test]
fn only_identity_and_representation_are_recorded_without_blocking() {
    let gate = Gate::agreed();
    let recorded: Vec<&'static str> = Class::ALL
        .iter()
        .filter(|class| gate.rule(**class).allowance == Allowance::Recorded)
        .map(|class| class.name())
        .collect();
    assert_eq!(recorded, vec!["server-identity", "representation"]);
}

#[test]
fn severity_separates_a_wrong_answer_from_a_wrong_order() {
    let gate = Gate::agreed();
    assert_eq!(gate.rule(Class::Status).severity, Severity::Blocking);
    assert_eq!(gate.rule(Class::Content).severity, Severity::Blocking);
    assert_eq!(gate.rule(Class::SearchOrder).severity, Severity::Serious);
    assert_eq!(gate.rule(Class::PagingLink).severity, Severity::Serious);
    assert_eq!(gate.rule(Class::ServerIdentity).severity, Severity::Noted);
}

#[test]
fn a_clean_full_run_passes() {
    let verdict = Gate::agreed().judge(&clean(40));
    assert_eq!(verdict, Verdict::Passed, "{verdict:?}");
}

#[test]
fn one_blocking_divergence_fails() {
    let mut facts = clean(40);
    facts.tallies.push(Tally {
        class: Class::Status,
        count: 1,
    });
    match Gate::agreed().judge(&facts) {
        Verdict::Failed(breaches) => {
            assert_eq!(breaches.len(), 1);
            assert_eq!(breaches[0].class, Some(Class::Status));
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_recorded_divergence_alone_passes() {
    let mut facts = clean(40);
    facts.tallies.push(Tally {
        class: Class::ServerIdentity,
        count: 9,
    });
    facts.tallies.push(Tally {
        class: Class::Representation,
        count: 3,
    });
    assert_eq!(Gate::agreed().judge(&facts), Verdict::Passed);
}

#[test]
fn an_empty_run_cannot_pass() {
    let facts = RunFacts {
        cases_planned: 0,
        cases_compared: 0,
        covered: covered(),
        tallies: Vec::new(),
    };
    match Gate::agreed().judge(&facts) {
        Verdict::Failed(breaches) => {
            assert!(breaches.iter().any(|breach| breach.class.is_none()));
        }
        other => panic!("a run that compared nothing must not pass, got {other:?}"),
    }
}

#[test]
fn a_run_below_the_agreed_breadth_cannot_pass() {
    let least = Gate::agreed().least_cases();
    assert!(least > 1);
    assert_eq!(Gate::agreed().judge(&clean(least)), Verdict::Passed);
    assert!(matches!(
        Gate::agreed().judge(&clean(least - 1)),
        Verdict::Failed(_)
    ));
}

#[test]
fn a_case_that_did_not_reach_both_sides_fails() {
    let facts = RunFacts {
        cases_planned: 40,
        cases_compared: 39,
        covered: covered(),
        tallies: Vec::new(),
    };
    match Gate::agreed().judge(&facts) {
        Verdict::Failed(breaches) => assert!(breaches
            .iter()
            .any(|breach| breach.reason.contains("did not reach"))),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_missing_request_shape_fails() {
    let gate = Gate::agreed();
    for missing in gate.must_cover() {
        let mut facts = clean(40);
        facts.covered.retain(|shape| shape != missing);
        match gate.judge(&facts) {
            Verdict::Failed(breaches) => assert!(
                breaches
                    .iter()
                    .any(|breach| breach.reason.contains(missing)),
                "{missing} was not named in the breach"
            ),
            other => panic!("{missing} was not exercised yet the gate passed: {other:?}"),
        }
    }
}

#[test]
fn the_agreed_shapes_cover_the_interactions_a_caller_depends_on() {
    let shapes = Gate::agreed().must_cover().to_vec();
    for shape in [
        "read",
        "vread",
        "create",
        "update",
        "conditional-update",
        "delete",
        "history",
        "search",
        "search-paging",
        "bundle-transaction",
        "bundle-batch",
        "capability",
    ] {
        assert!(shapes.contains(&shape), "{shape} was not agreed");
    }
}

#[test]
fn the_gate_carries_a_fingerprint_that_changes_with_its_rules() {
    let agreed = Gate::agreed();
    assert_eq!(agreed.fingerprint(), Gate::agreed().fingerprint());
    assert_eq!(agreed.fingerprint().len(), 64);
    let relaxed = Gate::of(
        agreed
            .rules()
            .iter()
            .map(|rule| {
                let mut rule = rule.clone();
                if rule.class == Class::Content {
                    rule.allowance = Allowance::Recorded;
                }
                rule
            })
            .collect(),
        agreed.least_cases(),
        agreed.must_cover().to_vec(),
    );
    assert_ne!(agreed.fingerprint(), relaxed.fingerprint());
}

#[test]
fn the_rendered_gate_names_every_class_and_its_allowance() {
    let text = Gate::agreed().render();
    for class in Class::ALL {
        assert!(text.contains(class.name()), "{} missing", class.name());
    }
    assert!(text.contains(&Gate::agreed().fingerprint()));
}

#[test]
fn every_excused_difference_names_the_decision_it_rests_on() {
    let gate = Gate::agreed();
    assert!(
        !gate.exemptions().is_empty(),
        "a difference kept on purpose is written down"
    );
    for excused in gate.exemptions() {
        assert!(
            !excused.rests_on.trim().is_empty(),
            "{} excuses {} on nothing",
            excused.class.name(),
            excused.detail
        );
        assert!(!excused.because.trim().is_empty());
        assert!(
            !excused.about.trim().is_empty() || !excused.detail.trim().is_empty(),
            "{} is excused whole, which empties the class",
            excused.class.name()
        );
    }
}

#[test]
fn the_gate_says_in_its_own_terms_what_it_excuses() {
    let terms = Gate::agreed().render();
    for excused in Gate::agreed().exemptions() {
        assert!(
            terms.contains(excused.rests_on) && terms.contains(excused.because),
            "{} is excused out of sight",
            excused.class.name()
        );
    }
}

#[test]
fn a_difference_no_exemption_names_still_blocks() {
    let gate = Gate::agreed();
    let mut facts = clean(gate.least_cases());
    facts.tallies = vec![Tally {
        class: Class::Status,
        count: 1,
    }];
    assert!(matches!(gate.judge(&facts), Verdict::Failed(_)));
}
