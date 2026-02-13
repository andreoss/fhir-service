use fhir_core::catalogue::Catalogue;
use fhir_core::FhirVersion;
use serde_json::json;

const CONFIDENTIALITY: &str = "http://terminology.hl7.org/CodeSystem/v3-Confidentiality";

#[test]
fn every_version_carries_published_code_system_content() {
    for version in FhirVersion::ALL {
        let held = Catalogue::of(version);
        assert!(held.systems().len() > 500, "{version} carries too little");
        assert_eq!(held.version(), version);
    }
}

#[test]
fn a_published_system_is_held_at_its_published_version() {
    let held = Catalogue::of(FhirVersion::R4);
    let found = held.system(CONFIDENTIALITY, None).expect("the system is held");
    assert_eq!(
        found.get("resourceType").and_then(|held| held.as_str()),
        Some("CodeSystem")
    );
    let versions = held.versions(CONFIDENTIALITY);
    assert!(!versions.is_empty());
    assert!(held.system(CONFIDENTIALITY, Some(versions[0])).is_some());
    assert!(held.system(CONFIDENTIALITY, Some("0.0.0")).is_none());
}

#[test]
fn subsumption_walks_a_published_hierarchy() {
    let held = Catalogue::of(FhirVersion::R4);
    let under: Vec<String> = held
        .descendants(Some(CONFIDENTIALITY), "_Confidentiality")
        .into_iter()
        .map(|concept| concept.code)
        .collect();
    assert!(under.contains(&"R".to_owned()));
    assert!(under.contains(&"_Confidentiality".to_owned()));
    let over: Vec<String> = held
        .ancestors(Some(CONFIDENTIALITY), "R")
        .into_iter()
        .map(|concept| concept.code)
        .collect();
    assert_eq!(over, vec!["R".to_owned(), "_Confidentiality".to_owned()]);
}

#[test]
fn subsumption_without_a_system_searches_every_system() {
    let held = Catalogue::of(FhirVersion::R4);
    let under = held.descendants(None, "_Confidentiality");
    assert!(!under.is_empty());
    assert!(held.descendants(None, "no-such-code").is_empty());
}

#[test]
fn content_published_elsewhere_is_named_rather_than_invented() {
    let held = Catalogue::of(FhirVersion::R4);
    let listed = held.unsupplied();
    assert!(!listed.is_empty());
    let found = listed
        .iter()
        .find(|held| held.url == "http://snomed.info/sct")
        .expect("a system published under its own licence is named");
    assert!(!found.reason.is_empty());
    assert!(held.system("http://snomed.info/sct", None).is_none());
}

#[test]
fn supplied_content_replaces_the_published_system() {
    let supplied = json!({
        "resourceType": "CodeSystem",
        "url": CONFIDENTIALITY,
        "content": "complete",
        "concept": [{"code": "local"}]
    });
    let held = Catalogue::of(FhirVersion::R4).with(vec![supplied]);
    let found = held.system(CONFIDENTIALITY, None).expect("the system is held");
    let concepts = found.get("concept").and_then(|held| held.as_array()).unwrap();
    assert_eq!(concepts.len(), 1);
    assert!(held.descendants(Some(CONFIDENTIALITY), "_Confidentiality").is_empty());
}

#[test]
fn supplied_content_adds_a_system_the_publication_does_not_carry() {
    let supplied = json!({
        "resourceType": "CodeSystem",
        "url": "http://snomed.info/sct",
        "version": "20260101",
        "content": "complete",
        "concept": [{"code": "1", "concept": [{"code": "2"}]}]
    });
    let held = Catalogue::of(FhirVersion::R4).with(vec![supplied]);
    let under: Vec<String> = held
        .descendants(Some("http://snomed.info/sct"), "1")
        .into_iter()
        .map(|concept| concept.code)
        .collect();
    assert_eq!(under, vec!["1".to_owned(), "2".to_owned()]);
    assert!(held
        .unsupplied()
        .iter()
        .all(|found| found.url != "http://snomed.info/sct"));
}

#[test]
fn a_body_that_is_not_a_code_system_is_refused() {
    let held = Catalogue::of(FhirVersion::R4);
    assert!(held.clone().loaded(&json!({"resourceType": "ValueSet"})).is_err());
    assert!(held.clone().loaded(&json!({"resourceType": "CodeSystem"})).is_err());
}

#[test]
fn a_malformed_catalogue_file_is_refused() {
    assert!(Catalogue::parse(FhirVersion::R4, "{").is_err());
    assert!(Catalogue::parse(FhirVersion::R4, "{\"systems\":[]}").is_err());
}
