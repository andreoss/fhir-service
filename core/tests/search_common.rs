use fhir_core::search::{
    active, common, common_in, lookup, lookup_in, Filter, SearchValue, Target, ValueType,
};
use fhir_core::{FhirInstant, FhirVersion, ResourceId};
use serde_json::json;

fn filter(name: &str, raw: &str) -> Filter {
    let def = lookup(None, name).expect("the common parameter is published");
    Filter::new(
        name,
        def.target.clone(),
        raw.split(',')
            .map(|part| SearchValue::parse(def.value_type, part).expect("the value parses"))
            .collect(),
    )
}

fn body() -> serde_json::Value {
    json!({
        "resourceType": "Patient",
        "id": "pt-1",
        "language": "en",
        "meta": {
            "versionId": "1",
            "lastUpdated": "2026-09-06T04:00:00Z",
            "source": "http://example.org/records/one",
            "profile": ["http://example.org/StructureDefinition/vip"],
            "tag": [{"system": "urn:tags", "code": "gold"}],
            "security": [{"system": "urn:sec", "code": "R"}]
        }
    })
}

fn matches(name: &str, raw: &str) -> bool {
    let id = ResourceId::parse("pt-1").unwrap();
    let updated = FhirInstant::parse("2026-09-06T04:00:00Z").unwrap();
    filter(name, raw).matches(&id, &updated, &body())
}

#[test]
fn the_id_parameter_selects_by_the_logical_id_and_never_by_the_body() {
    assert!(matches("_id", "pt-1"));
    assert!(!matches("_id", "pt-2"));
    assert!(matches("_id", "pt-2,pt-1"));
    assert!(!matches("_id", "Patient/pt-1"));
    let elsewhere = json!({"resourceType": "Patient", "id": "pt-9"});
    let id = ResourceId::parse("pt-1").unwrap();
    let updated = FhirInstant::parse("2026-09-06T04:00:00Z").unwrap();
    assert!(
        filter("_id", "pt-1").matches(&id, &updated, &elsewhere),
        "the id the store holds decides, not the one the body carries"
    );
}

#[test]
fn the_last_updated_parameter_compares_against_the_instant_the_store_stamped() {
    assert!(matches("_lastUpdated", "2026-09-06"));
    assert!(matches("_lastUpdated", "2026-09-06T04:00:00Z"));
    assert!(matches("_lastUpdated", "ge2026-09-06T04:00:00Z"));
    assert!(matches("_lastUpdated", "le2026-09-06T04:00:00Z"));
    assert!(matches("_lastUpdated", "gt2026-09-05"));
    assert!(!matches("_lastUpdated", "lt2026-09-05"));
    assert!(!matches("_lastUpdated", "gt2026-09-06"));
    assert!(matches("_lastUpdated", "ne2025"));
    assert!(!matches("_lastUpdated", "ne2026-09-06"));
}

#[test]
fn the_profile_parameter_matches_a_canonical_address_verbatim() {
    assert!(matches(
        "_profile",
        "http://example.org/StructureDefinition/vip"
    ));
    assert!(!matches(
        "_profile",
        "http://example.org/StructureDefinition/other"
    ));
    assert!(!matches(
        "_profile",
        "http://example.org/StructureDefinition"
    ));
}

#[test]
fn tag_and_security_match_the_way_the_system_was_supplied() {
    assert!(matches("_tag", "urn:tags|gold"));
    assert!(matches("_tag", "gold"));
    assert!(matches("_tag", "urn:tags|"));
    assert!(!matches("_tag", "|gold"));
    assert!(!matches("_tag", "urn:other|gold"));
    assert!(!matches("_tag", "urn:tags|silver"));
    assert!(matches("_security", "urn:sec|R"));
    assert!(!matches("_security", "urn:sec|N"));
    assert!(!matches("_security", "urn:tags|gold"));
}

#[test]
fn the_source_parameter_matches_the_address_the_meta_carries() {
    assert!(matches("_source", "http://example.org/records/one"));
    assert!(!matches("_source", "http://example.org/records/two"));
    assert!(!matches("_source", "http://example.org/records"));
    assert!(matches(
        "_source",
        "http://example.org/records/two,http://example.org/records/one"
    ));
}

#[test]
fn the_language_parameter_matches_the_language_the_resource_declares() {
    assert!(matches("_language", "en"));
    assert!(!matches("_language", "fr"));
    assert!(matches("_language", "fr,en"));
}

#[test]
fn a_common_parameter_is_answered_only_where_its_release_names_it() {
    for version in FhirVersion::ALL {
        let named: Vec<String> = common_in(version)
            .iter()
            .map(|def| def.name.clone())
            .collect();
        let holds = |name: &str| named.iter().any(|held| held == name);
        assert!(holds("_id") && holds("_lastUpdated"), "{version:?}");
        assert_eq!(
            holds("_source"),
            version != FhirVersion::Stu3,
            "_source {version:?}"
        );
        assert_eq!(
            holds("_language"),
            version == FhirVersion::R5,
            "_language {version:?}"
        );
        assert_eq!(
            lookup_in(version, None, "_source").is_some(),
            version != FhirVersion::Stu3,
            "_source {version:?}"
        );
        assert_eq!(
            lookup_in(version, None, "_language").is_some(),
            version == FhirVersion::R5,
            "_language {version:?}"
        );
    }
    assert_eq!(
        lookup_in(FhirVersion::R5, None, "_language").map(|def| def.paths()),
        Some(vec!["language".to_owned()])
    );
    assert_eq!(
        lookup_in(FhirVersion::R4, None, "_source").map(|def| def.paths()),
        Some(vec!["meta.source".to_owned()])
    );
}

#[test]
fn the_common_parameters_are_the_ones_the_specification_gives_every_type() {
    let published: &[(&str, ValueType)] = &[
        ("_id", ValueType::Token),
        ("_lastUpdated", ValueType::Date),
        ("_profile", ValueType::Uri),
        ("_tag", ValueType::Token),
        ("_security", ValueType::Token),
        ("_source", ValueType::Uri),
        ("_language", ValueType::Token),
        ("_in", ValueType::Reference),
        ("_text", ValueType::String),
    ];
    for (name, value_type) in published {
        let def = lookup(None, name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(def.value_type, *value_type, "{name}");
        assert!(
            lookup(Some("Observation".parse().unwrap()), name).is_some(),
            "{name} applies to every type"
        );
    }
    assert_eq!(common().len(), published.len());
    assert_eq!(
        lookup(None, "_text").map(|def| def.target.clone()),
        Some(Target::Path(vec!["text".to_owned()]))
    );
    let narrative = json!({
        "resourceType": "Patient",
        "id": "pt-n",
        "text": {"status": "generated", "div": "<div><p>Fever and chills</p></div>"}
    });
    let id = ResourceId::parse("pt-n").unwrap();
    let updated = FhirInstant::parse("2026-09-06T04:00:00Z").unwrap();
    assert!(
        filter("_text", "fever AND chills").matches(&id, &updated, &narrative),
        "the narrative pools its words"
    );
    assert!(
        !filter("_text", "rash").matches(&id, &updated, &narrative),
        "only the narrative's words answer"
    );
    assert!(matches!(
        lookup(None, "_id").map(|def| def.target.clone()),
        Some(Target::Id)
    ));
    assert!(matches!(
        lookup(None, "_lastUpdated").map(|def| def.target.clone()),
        Some(Target::LastUpdated)
    ));
    assert_eq!(
        lookup(None, "_profile").map(|def| def.paths()),
        Some(vec!["meta.profile".to_owned()])
    );
    assert!(lookup(None, "_nonesuch").is_none());
    assert!(lookup(None, "name").is_none());
}

#[test]
fn the_membership_parameter_is_named_by_the_latest_release_alone() {
    for version in FhirVersion::ALL {
        let named: Vec<String> = common_in(version)
            .iter()
            .map(|def| def.name.clone())
            .collect();
        assert_eq!(
            named.iter().any(|name| name == "_in"),
            version == FhirVersion::R5,
            "_in {version:?} {named:?}"
        );
        assert_eq!(
            lookup_in(version, None, "_in").is_some(),
            version == FhirVersion::R5,
            "lookup _in {version:?}"
        );
    }
    let def = lookup_in(FhirVersion::R5, None, "_in").expect("the fifth release names it");
    assert_eq!(def.value_type, ValueType::Reference);
    assert!(matches!(def.target, Target::Collection));
    assert_eq!(
        def.targets,
        vec!["CareTeam".to_owned(), "Group".to_owned(), "List".to_owned()]
    );
}

#[test]
fn the_active_members_of_a_collection_are_the_ones_the_release_counts() {
    let now = FhirInstant::parse("2026-09-06T04:00:00Z").unwrap();
    let group = json!({
        "resourceType": "Group",
        "id": "grp-1",
        "actual": true,
        "member": [
            {"entity": {"reference": "Patient/101"}},
            {"entity": {"reference": "Patient/102"}, "inactive": true},
            {"entity": {"reference": "Patient/103"}, "period": {"start": "2020-01-01T00:00:00Z", "end": "2021-01-01T00:00:00Z"}},
            {"entity": {"reference": "Patient/104"}, "period": {"start": "2020-01-01T00:00:00Z", "end": "2027-01-01T00:00:00Z"}}
        ]
    });
    assert_eq!(
        active(&"Group".parse().unwrap(), &group, &now),
        vec!["101".to_owned(), "104".to_owned()]
    );
    let list = json!({
        "resourceType": "List",
        "id": "lst-1",
        "status": "current",
        "mode": "working",
        "entry": [
            {"item": {"reference": "Patient/201"}},
            {"item": {"reference": "Patient/202"}, "deleted": true},
            {"item": {"reference": "Patient/203"}, "period": {"start": "2026-09-01T00:00:00Z", "end": "2026-09-30T00:00:00Z"}}
        ]
    });
    assert_eq!(
        active(&"List".parse().unwrap(), &list, &now),
        vec!["201".to_owned(), "203".to_owned()]
    );
    let team = json!({
        "resourceType": "CareTeam",
        "id": "ct-1",
        "status": "active",
        "participant": [
            {"member": {"reference": "Patient/301"}, "period": {"start": "2026-01-01T00:00:00Z"}},
            {"member": {"reference": "Patient/302"}, "period": {"start": "2027-01-01T00:00:00Z"}}
        ]
    });
    assert_eq!(
        active(&"CareTeam".parse().unwrap(), &team, &now),
        vec!["301".to_owned()]
    );
    assert_eq!(
        active(&"Patient".parse().unwrap(), &group, &now),
        Vec::<String>::new()
    );
    assert_eq!(
        active(
            &"Group".parse().unwrap(),
            &json!({"resourceType": "Group", "id": "gp"}),
            &now
        ),
        Vec::<String>::new()
    );
}
