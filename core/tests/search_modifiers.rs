use fhir_core::search::{lookup, Filter, Modifier, SearchValue, ValueType};
use fhir_core::{Error, FhirInstant, ResourceId};
use serde_json::{json, Value};

fn id() -> ResourceId {
    ResourceId::parse("p-1").unwrap()
}

fn updated() -> FhirInstant {
    FhirInstant::parse("2026-09-06T04:00:00Z").unwrap()
}

fn filter(type_name: &str, name: &str, modifier: Modifier, raw: &str) -> Filter {
    let resource_type = type_name.parse().unwrap();
    let def = lookup(Some(resource_type), name).expect("the parameter is published");
    let values: Vec<SearchValue> = raw
        .split(',')
        .map(|part| def.value_with(&modifier, part).expect("the value parses"))
        .collect();
    Filter {
        name: name.to_owned(),
        target: def.target.clone(),
        modifier,
        index: None,
        values,
    }
}

fn holds(type_name: &str, name: &str, modifier: Modifier, raw: &str, body: &Value) -> bool {
    filter(type_name, name, modifier, raw).matches(&id(), &updated(), body)
}

#[test]
fn missing_selects_by_the_presence_of_a_value_and_not_by_its_content() {
    let with = json!({"gender": "female"});
    let without = json!({"active": true});
    let leaf = json!({"name": [{"given": ["Ann"]}]});
    let one_of_many = json!({"name": [{"given": ["Ann"]}, {"family": "Ross"}]});
    assert!(holds("Patient", "gender", Modifier::Missing, "true", &without));
    assert!(!holds("Patient", "gender", Modifier::Missing, "true", &with));
    assert!(holds("Patient", "gender", Modifier::Missing, "false", &with));
    assert!(!holds("Patient", "gender", Modifier::Missing, "false", &without));
    assert!(holds("Patient", "family", Modifier::Missing, "true", &leaf));
    assert!(holds("Patient", "family", Modifier::Missing, "false", &one_of_many));
    let def = lookup(Some("Patient".parse().unwrap()), "gender").unwrap();
    assert!(matches!(
        def.value_with(&Modifier::Missing, "perhaps").unwrap_err(),
        Error::InvalidParameter(_)
    ));
}

#[test]
fn a_bare_string_is_a_case_insensitive_prefix_and_exact_is_the_whole_value() {
    let body = json!({"name": [{"family": "Sørensen"}]});
    assert!(holds("Patient", "family", Modifier::None, "sør", &body));
    assert!(holds("Patient", "family", Modifier::None, "SØRENSEN", &body));
    assert!(!holds("Patient", "family", Modifier::None, "rensen", &body));
    assert!(holds("Patient", "family", Modifier::Exact, "Sørensen", &body));
    assert!(!holds("Patient", "family", Modifier::Exact, "sørensen", &body));
    assert!(!holds("Patient", "family", Modifier::Exact, "Sør", &body));
}

#[test]
fn contains_matches_anywhere_in_the_value_and_ignores_case() {
    let body = json!({"name": [{"family": "Sørensen"}]});
    assert!(holds("Patient", "family", Modifier::Contains, "rens", &body));
    assert!(holds("Patient", "family", Modifier::Contains, "RENS", &body));
    assert!(holds("Patient", "family", Modifier::Contains, "Sørensen", &body));
    assert!(!holds("Patient", "family", Modifier::Contains, "zzz", &body));
}

#[test]
fn not_excludes_a_resource_any_of_whose_values_match() {
    let body = json!({"gender": "male"});
    assert!(holds("Patient", "gender", Modifier::Not, "female", &body));
    assert!(!holds("Patient", "gender", Modifier::Not, "male", &body));
    let repeated = json!({"category": [{"coding": [{"code": "vital-signs"}, {"code": "exam"}]}]});
    assert!(!holds("Observation", "category", Modifier::Not, "exam", &repeated));
    assert!(holds("Observation", "category", Modifier::Not, "survey", &repeated));
    assert!(!holds("Observation", "category", Modifier::Not, "survey,exam", &repeated));
    let absent = json!({"status": "final"});
    assert!(holds("Observation", "category", Modifier::Not, "exam", &absent));
}

#[test]
fn text_matches_the_words_of_a_coded_element_and_never_its_code() {
    let body = json!({"code": {"text": "Body Temperature", "coding": [{"code": "8310-5"}]}});
    assert!(holds("Observation", "code", Modifier::Text, "temperature", &body));
    assert!(holds("Observation", "code", Modifier::Text, "Body", &body));
    assert!(!holds("Observation", "code", Modifier::Text, "8310-5", &body));
    let display = json!({"code": {"coding": [{"code": "x", "display": "Heart rate"}]}});
    assert!(holds("Observation", "code", Modifier::Text, "heart", &display));
    assert!(!holds("Observation", "code", Modifier::Text, "x", &display));
}

#[test]
fn a_set_membership_modifier_is_answered_only_once_the_set_is_expanded() {
    let body = json!({"code": {"coding": [{"system": "urn:s", "code": "a"}]}});
    let asked = filter("Observation", "code", Modifier::In, "http://x/vs");
    assert_eq!(asked.code_sets(), vec!["http://x/vs".to_owned()]);
    assert!(
        !asked.matches(&id(), &updated(), &body),
        "an address is not a code, so it selects nothing until it is expanded"
    );

    let member = SearchValue::parse(ValueType::Token, "urn:s|a").unwrap();
    let other = SearchValue::parse(ValueType::Token, "urn:s|z").unwrap();
    let inside = asked.resolved(std::slice::from_ref(&member));
    assert_eq!(inside.modifier, Modifier::None);
    assert!(inside.matches(&id(), &updated(), &body));
    assert!(!asked
        .resolved(std::slice::from_ref(&other))
        .matches(&id(), &updated(), &body));

    let refused = filter("Observation", "code", Modifier::NotIn, "http://x/vs");
    let outside = refused.resolved(std::slice::from_ref(&member));
    assert_eq!(outside.modifier, Modifier::Not);
    assert!(!outside.matches(&id(), &updated(), &body));
    assert!(refused
        .resolved(std::slice::from_ref(&other))
        .matches(&id(), &updated(), &body));
}

#[test]
fn a_subsumption_modifier_on_a_code_selects_only_what_a_terminology_expanded_to() {
    let body = json!({"code": {"coding": [{"system": "urn:cs", "code": "a.b.c"}]}});
    assert!(
        !holds("Observation", "code", Modifier::Below, "a.b", &body),
        "the spelling of a code carries no hierarchy"
    );
    assert!(
        !holds("Observation", "code", Modifier::Above, "a.b.c.d", &body),
        "the spelling of a code carries no hierarchy"
    );
    assert!(holds("Observation", "code", Modifier::Below, "a.b.c", &body));

    let covered = [
        SearchValue::parse(ValueType::Token, "urn:cs|a.b").unwrap(),
        SearchValue::parse(ValueType::Token, "urn:cs|a.b.c").unwrap(),
    ];
    let expanded = filter("Observation", "code", Modifier::Below, "urn:cs|a.b").expanded(&covered);
    assert_eq!(expanded.modifier, Modifier::None);
    assert!(expanded.matches(&id(), &updated(), &body));
}

#[test]
fn a_subsumption_modifier_on_an_address_walks_the_path_it_is_a_prefix_of() {
    let uri = json!({"url": "http://x/base/part"});
    assert!(holds("ValueSet", "url", Modifier::Below, "http://x/base", &uri));
    assert!(holds("ValueSet", "url", Modifier::Below, "http://x/base/part", &uri));
    assert!(!holds("ValueSet", "url", Modifier::Below, "http://x/other", &uri));
    assert!(!holds("ValueSet", "url", Modifier::Below, "http://x/bas", &uri));
    assert!(holds("ValueSet", "url", Modifier::Above, "http://x/base/part/deep", &uri));
    assert!(!holds("ValueSet", "url", Modifier::Above, "http://x/base", &uri));
    let urn = json!({"url": "urn:oid:1.2.3.4"});
    assert!(!holds("ValueSet", "url", Modifier::Below, "urn:oid:1.2", &urn));
    assert!(holds("ValueSet", "url", Modifier::Below, "urn:oid:1.2.3.4", &urn));
}

#[test]
fn a_type_modifier_restricts_a_reference_and_refuses_one_naming_another_type() {
    let body = json!({"subject": {"reference": "Patient/p-9"}});
    let patient = Modifier::Type("Patient".parse().unwrap());
    let group = Modifier::Type("Group".parse().unwrap());
    assert!(holds("Observation", "subject", patient.clone(), "p-9", &body));
    assert!(!holds("Observation", "subject", group.clone(), "p-9", &body));
    assert!(holds("Observation", "subject", patient, "Patient/p-9", &body));
    let def = lookup(Some("Observation".parse().unwrap()), "subject").unwrap();
    assert!(matches!(
        def.value_with(&group, "Patient/p-9").unwrap_err(),
        Error::UnsupportedParameter(_)
    ));
}

#[test]
fn an_identifier_modifier_reads_the_identifier_of_a_reference_not_its_address() {
    let body = json!({"subject": {"identifier": {"system": "urn:mrn", "value": "42"}}});
    assert!(holds("Observation", "subject", Modifier::Identifier, "urn:mrn|42", &body));
    assert!(holds("Observation", "subject", Modifier::Identifier, "42", &body));
    assert!(!holds("Observation", "subject", Modifier::Identifier, "urn:mrn|43", &body));
    let addressed = json!({"subject": {"reference": "Patient/42"}});
    assert!(!holds("Observation", "subject", Modifier::Identifier, "42", &addressed));
}

#[test]
fn of_type_needs_the_system_the_code_and_the_value_to_hold_together() {
    let body = json!({
        "identifier": [{
            "type": {"coding": [{"system": "urn:t", "code": "MR"}]},
            "value": "12345"
        }]
    });
    assert!(holds("Patient", "identifier", Modifier::OfType, "urn:t|MR|12345", &body));
    assert!(!holds("Patient", "identifier", Modifier::OfType, "urn:t|MR|99", &body));
    assert!(!holds("Patient", "identifier", Modifier::OfType, "urn:t|SB|12345", &body));
    assert!(!holds("Patient", "identifier", Modifier::OfType, "urn:o|MR|12345", &body));
    let def = lookup(Some("Patient".parse().unwrap()), "identifier").unwrap();
    assert!(matches!(
        def.value_with(&Modifier::OfType, "urn:t|MR").unwrap_err(),
        Error::InvalidParameter(_)
    ));
}

#[test]
fn a_modifier_the_value_type_does_not_carry_is_refused_as_unsupported() {
    let refused: &[(&str, &str, Modifier)] = &[
        ("Patient", "gender", Modifier::Exact),
        ("Patient", "gender", Modifier::Contains),
        ("Patient", "family", Modifier::OfType),
        ("Patient", "family", Modifier::Not),
        ("Patient", "family", Modifier::Text),
        ("Patient", "birthdate", Modifier::Exact),
        ("Observation", "value-quantity", Modifier::Below),
        ("Patient", "gender", Modifier::Identifier),
    ];
    for (type_name, name, modifier) in refused {
        let def = lookup(Some(type_name.parse().unwrap()), name).unwrap();
        assert!(
            matches!(
                def.value_with(modifier, "a").unwrap_err(),
                Error::UnsupportedParameter(_)
            ),
            "{type_name}.{name} {modifier:?}"
        );
    }
    let family = lookup(Some("Patient".parse().unwrap()), "family").unwrap();
    assert!(family.value_with(&Modifier::Exact, "Ann").is_ok());
    assert!(family.value_with(&Modifier::Missing, "true").is_ok());
}
