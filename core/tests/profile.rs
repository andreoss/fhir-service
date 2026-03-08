use fhir_core::profile::{CodeSource, Profile};
use serde_json::{json, Value};

struct Held(Vec<(&'static str, Option<&'static str>, &'static str)>);

impl CodeSource for Held {
    fn codes(&self, value_set: &str) -> Option<Vec<(Option<String>, String)>> {
        let held: Vec<(Option<String>, String)> = self
            .0
            .iter()
            .filter(|(url, _, _)| *url == value_set)
            .map(|(_, system, code)| (system.map(str::to_owned), (*code).to_owned()))
            .collect();
        match held.is_empty() {
            true => None,
            false => Some(held),
        }
    }
}

struct Nothing;

impl CodeSource for Nothing {
    fn codes(&self, _value_set: &str) -> Option<Vec<(Option<String>, String)>> {
        None
    }
}

fn definition(elements: Value) -> Value {
    json!({
        "resourceType": "StructureDefinition",
        "url": "http://example.test/StructureDefinition/watched",
        "type": "Patient",
        "differential": {"element": elements}
    })
}

fn parsed(elements: Value) -> Profile {
    Profile::parse(&definition(elements)).expect("the definition reads")
}

fn diagnostics(issues: &[fhir_core::validate::Issue]) -> String {
    issues
        .iter()
        .map(|issue| issue.diagnostics.clone())
        .collect::<Vec<String>>()
        .join(" | ")
}

#[test]
fn a_definition_that_is_not_a_structure_definition_is_refused() {
    let error = Profile::parse(&json!({"resourceType": "Patient"})).unwrap_err();
    assert!(error.to_string().contains("StructureDefinition"), "{error}");
}

#[test]
fn a_definition_without_a_url_is_refused() {
    let error = Profile::parse(&json!({
        "resourceType": "StructureDefinition",
        "type": "Patient",
        "differential": {"element": []}
    }))
    .unwrap_err();
    assert!(error.to_string().contains("url"), "{error}");
}

#[test]
fn a_profile_names_the_type_it_constrains() {
    let profile = parsed(json!([]));
    assert_eq!(profile.base_type(), "Patient");
    assert_eq!(
        profile.url(),
        "http://example.test/StructureDefinition/watched"
    );
}

#[test]
fn an_element_the_profile_requires_is_reported_when_it_is_missing() {
    let profile = parsed(json!([{"path": "Patient.identifier", "min": 1, "max": "*"}]));
    let issues = profile.judge(&json!({"resourceType": "Patient"}), &Nothing);
    assert_eq!(issues.len(), 1, "{}", diagnostics(&issues));
    assert!(
        diagnostics(&issues).contains("carries 0 of the 1"),
        "{}",
        diagnostics(&issues)
    );
    assert_eq!(issues[0].expression.as_deref(), Some("Patient.identifier"));
}

#[test]
fn an_element_the_profile_requires_and_the_resource_carries_passes() {
    let profile = parsed(json!([{"path": "Patient.identifier", "min": 1, "max": "*"}]));
    let body = json!({"resourceType": "Patient", "identifier": [{"value": "a"}]});
    assert!(profile.judge(&body, &Nothing).is_empty());
}

#[test]
fn an_element_repeated_past_its_maximum_is_reported() {
    let profile = parsed(json!([{"path": "Patient.name", "min": 0, "max": "1"}]));
    let body = json!({
        "resourceType": "Patient",
        "name": [{"family": "One"}, {"family": "Two"}]
    });
    let issues = profile.judge(&body, &Nothing);
    assert_eq!(issues.len(), 1, "{}", diagnostics(&issues));
    assert!(
        diagnostics(&issues).contains("at most 1"),
        "{}",
        diagnostics(&issues)
    );
}

#[test]
fn cardinality_is_judged_under_each_parent_rather_than_over_the_resource() {
    let profile = parsed(json!([{"path": "Patient.name.given", "min": 1, "max": "*"}]));
    let body = json!({
        "resourceType": "Patient",
        "name": [{"given": ["Ada"]}, {"family": "Stone"}]
    });
    let issues = profile.judge(&body, &Nothing);
    assert_eq!(issues.len(), 1, "{}", diagnostics(&issues));
    assert_eq!(
        issues[0].expression.as_deref(),
        Some("Patient.name[1].given"),
        "{}",
        diagnostics(&issues)
    );
}

#[test]
fn a_value_the_profile_fixes_must_be_exactly_that_value() {
    let profile = parsed(json!([{"path": "Patient.gender", "fixedCode": "female"}]));
    let good = json!({"resourceType": "Patient", "gender": "female"});
    assert!(profile.judge(&good, &Nothing).is_empty());
    let bad = json!({"resourceType": "Patient", "gender": "male"});
    let issues = profile.judge(&bad, &Nothing);
    assert_eq!(issues.len(), 1, "{}", diagnostics(&issues));
    assert!(
        diagnostics(&issues).contains("fixes"),
        "{}",
        diagnostics(&issues)
    );
}

#[test]
fn a_pattern_names_what_must_be_there_and_leaves_the_rest_free() {
    let profile = parsed(json!([{
        "path": "Patient.identifier",
        "patternIdentifier": {"system": "urn:s"}
    }]));
    let good = json!({
        "resourceType": "Patient",
        "identifier": [{"system": "urn:s", "value": "anything"}]
    });
    assert!(
        profile.judge(&good, &Nothing).is_empty(),
        "{}",
        diagnostics(&profile.judge(&good, &Nothing))
    );
    let bad = json!({
        "resourceType": "Patient",
        "identifier": [{"system": "urn:other", "value": "anything"}]
    });
    assert_eq!(profile.judge(&bad, &Nothing).len(), 1);
}

#[test]
fn a_reference_may_only_point_at_a_type_the_profile_allows() {
    let profile = Profile::parse(&json!({
        "resourceType": "StructureDefinition",
        "url": "http://example.test/StructureDefinition/watched",
        "type": "Observation",
        "differential": {"element": [{
            "path": "Observation.subject",
            "type": [{
                "code": "Reference",
                "targetProfile": ["http://hl7.org/fhir/StructureDefinition/Patient"]
            }]
        }]}
    }))
    .unwrap();
    let good = json!({"resourceType": "Observation", "subject": {"reference": "Patient/1"}});
    assert!(profile.judge(&good, &Nothing).is_empty());
    let bad = json!({"resourceType": "Observation", "subject": {"reference": "Group/1"}});
    let issues = profile.judge(&bad, &Nothing);
    assert_eq!(issues.len(), 1, "{}", diagnostics(&issues));
    assert!(
        diagnostics(&issues).contains("Group"),
        "{}",
        diagnostics(&issues)
    );
}

#[test]
fn a_required_binding_refuses_a_code_the_set_does_not_hold() {
    let profile = parsed(json!([{
        "path": "Patient.maritalStatus",
        "binding": {"strength": "required", "valueSet": "http://example.test/ValueSet/marital"}
    }]));
    let codes = Held(vec![(
        "http://example.test/ValueSet/marital",
        Some("urn:s"),
        "M",
    )]);
    let good = json!({
        "resourceType": "Patient",
        "maritalStatus": {"coding": [{"system": "urn:s", "code": "M"}]}
    });
    assert!(profile.judge(&good, &codes).is_empty());
    let bad = json!({
        "resourceType": "Patient",
        "maritalStatus": {"coding": [{"system": "urn:s", "code": "Z"}]}
    });
    let issues = profile.judge(&bad, &codes);
    assert_eq!(issues.len(), 1, "{}", diagnostics(&issues));
    assert_eq!(issues[0].severity, fhir_core::IssueSeverity::Error);
}

#[test]
fn an_extensible_binding_warns_rather_than_refuses() {
    let profile = parsed(json!([{
        "path": "Patient.maritalStatus",
        "binding": {"strength": "extensible", "valueSet": "http://example.test/ValueSet/marital"}
    }]));
    let codes = Held(vec![(
        "http://example.test/ValueSet/marital",
        Some("urn:s"),
        "M",
    )]);
    let bad = json!({
        "resourceType": "Patient",
        "maritalStatus": {"coding": [{"system": "urn:s", "code": "Z"}]}
    });
    let issues = profile.judge(&bad, &codes);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].severity, fhir_core::IssueSeverity::Warning);
}

#[test]
fn a_preferred_binding_is_not_enforced() {
    let profile = parsed(json!([{
        "path": "Patient.maritalStatus",
        "binding": {"strength": "preferred", "valueSet": "http://example.test/ValueSet/marital"}
    }]));
    let bad = json!({
        "resourceType": "Patient",
        "maritalStatus": {"coding": [{"system": "urn:s", "code": "Z"}]}
    });
    assert!(profile.judge(&bad, &Nothing).is_empty());
}

#[test]
fn a_binding_this_instance_cannot_resolve_is_reported_as_unjudged() {
    let profile = parsed(json!([{
        "path": "Patient.maritalStatus",
        "binding": {"strength": "required", "valueSet": "http://example.test/ValueSet/absent"}
    }]));
    let body = json!({
        "resourceType": "Patient",
        "maritalStatus": {"coding": [{"system": "urn:s", "code": "Z"}]}
    });
    let issues = profile.judge(&body, &Nothing);
    assert_eq!(issues.len(), 1, "{}", diagnostics(&issues));
    assert_eq!(issues[0].severity, fhir_core::IssueSeverity::Warning);
    assert!(
        issues[0].diagnostics.contains("not judged"),
        "{}",
        issues[0].diagnostics
    );
}

#[test]
fn a_slice_is_counted_by_what_its_discriminator_picks_out() {
    let profile = parsed(json!([
        {
            "path": "Patient.identifier",
            "slicing": {"discriminator": [{"type": "value", "path": "system"}]},
            "min": 1,
            "max": "*"
        },
        {"path": "Patient.identifier", "sliceName": "national", "min": 1, "max": "1"},
        {
            "path": "Patient.identifier.system",
            "sliceName": "national",
            "fixedUri": "urn:national"
        }
    ]));
    let good = json!({
        "resourceType": "Patient",
        "identifier": [
            {"system": "urn:national", "value": "a"},
            {"system": "urn:local", "value": "b"}
        ]
    });
    assert!(
        profile.judge(&good, &Nothing).is_empty(),
        "{}",
        diagnostics(&profile.judge(&good, &Nothing))
    );
    let missing = json!({
        "resourceType": "Patient",
        "identifier": [{"system": "urn:local", "value": "b"}]
    });
    let issues = profile.judge(&missing, &Nothing);
    assert!(
        diagnostics(&issues).contains("slice national"),
        "{}",
        diagnostics(&issues)
    );
}

#[test]
fn a_slice_past_its_maximum_is_reported() {
    let profile = parsed(json!([
        {
            "path": "Patient.identifier",
            "slicing": {"discriminator": [{"type": "value", "path": "system"}]}
        },
        {"path": "Patient.identifier", "sliceName": "national", "min": 1, "max": "1"},
        {
            "path": "Patient.identifier.system",
            "sliceName": "national",
            "fixedUri": "urn:national"
        }
    ]));
    let body = json!({
        "resourceType": "Patient",
        "identifier": [
            {"system": "urn:national", "value": "a"},
            {"system": "urn:national", "value": "b"}
        ]
    });
    let issues = profile.judge(&body, &Nothing);
    assert!(
        diagnostics(&issues).contains("at most 1"),
        "{}",
        diagnostics(&issues)
    );
}

#[test]
fn a_choice_element_is_matched_by_the_value_it_was_written_with() {
    let profile = Profile::parse(&json!({
        "resourceType": "StructureDefinition",
        "url": "http://example.test/StructureDefinition/watched",
        "type": "Observation",
        "differential": {"element": [{"path": "Observation.value[x]", "min": 1, "max": "1"}]}
    }))
    .unwrap();
    let good = json!({"resourceType": "Observation", "valueString": "held"});
    assert!(profile.judge(&good, &Nothing).is_empty());
    let missing = json!({"resourceType": "Observation"});
    assert_eq!(profile.judge(&missing, &Nothing).len(), 1);
}

#[test]
fn a_snapshot_is_read_where_the_definition_carries_one() {
    let profile = Profile::parse(&json!({
        "resourceType": "StructureDefinition",
        "url": "http://example.test/StructureDefinition/watched",
        "type": "Patient",
        "snapshot": {"element": [{"path": "Patient.identifier", "min": 2, "max": "*"}]},
        "differential": {"element": [{"path": "Patient.name", "min": 9, "max": "*"}]}
    }))
    .unwrap();
    let body = json!({"resourceType": "Patient", "identifier": [{"value": "a"}]});
    let issues = profile.judge(&body, &Nothing);
    assert_eq!(issues.len(), 1, "{}", diagnostics(&issues));
    assert!(
        diagnostics(&issues).contains("identifier"),
        "{}",
        diagnostics(&issues)
    );
}
