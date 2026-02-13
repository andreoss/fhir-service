use fhir_core::{FhirVersion, Model, Rule};

fn patient() -> serde_json::Value {
    serde_json::json!({
        "resourceType": "Patient",
        "id": "pt-1",
        "meta": {"versionId": "1", "lastUpdated": "2026-09-06T04:00:00.000Z"},
        "text": {"status": "generated", "div": "<div xmlns=\"http://www.w3.org/1999/xhtml\">a</div>"},
        "active": true,
        "gender": "female",
        "birthDate": "1980-04-01",
        "name": [{"use": "official", "family": "Stone", "given": ["Ada"]}],
        "telecom": [{"system": "phone", "value": "555", "use": "home"}],
        "address": [{"line": ["1 Long Road"], "city": "Ely", "postalCode": "CB7"}]
    })
}

#[test]
fn every_version_carries_its_own_generated_definitions() {
    for version in FhirVersion::ALL {
        let model = Model::of(version);
        assert_eq!(model.version(), version);
        assert_eq!(model.release(), version.release());
        assert!(model.resources().count() > 100, "{version}");
        assert!(model.has_resource("Patient"), "{version}");
        assert!(model.has_resource("Observation"), "{version}");
        assert!(!model.has_resource("Nonesuch"), "{version}");
        assert!(model.bound() > 100, "{version}");
    }
}

#[test]
fn a_resource_is_accepted_by_every_version_that_defines_it() {
    for version in FhirVersion::ALL {
        let findings = Model::of(version).check(&patient());
        assert_eq!(findings, Vec::new(), "{version}");
    }
}

#[test]
fn an_element_outside_the_definitions_is_reported_by_every_version() {
    for version in FhirVersion::ALL {
        let mut body = patient();
        body["favourite"] = serde_json::json!("tea");
        let findings = Model::of(version).check(&body);
        assert_eq!(findings.len(), 1, "{version}");
        assert_eq!(findings[0].rule, Rule::Structure, "{version}");
        assert_eq!(findings[0].path, "Patient.favourite", "{version}");
    }
}

#[test]
fn a_code_outside_a_bound_value_set_is_reported_by_every_version() {
    for version in FhirVersion::ALL {
        let mut body = patient();
        body["gender"] = serde_json::json!("lady");
        let findings = Model::of(version).check(&body);
        assert!(
            findings.iter().any(|finding| finding.rule == Rule::Binding),
            "{version}: {findings:?}"
        );
    }
}

#[test]
fn a_required_element_left_out_is_reported() {
    let body = serde_json::json!({"resourceType": "Observation", "status": "final"});
    let findings = Model::of(FhirVersion::R4).check(&body);
    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::Cardinality && finding.path == "Observation.code"),
        "{findings:?}"
    );
}

#[test]
fn the_types_a_version_defines_are_the_types_it_serves() {
    assert!(Model::of(FhirVersion::R5).has_resource("Citation"));
    assert!(!Model::of(FhirVersion::Stu3).has_resource("Citation"));
    assert!(Model::of(FhirVersion::Stu3).has_resource("ProcedureRequest"));
    assert!(!Model::of(FhirVersion::R4).has_resource("ProcedureRequest"));
    assert!(Model::of(FhirVersion::R4).has_resource("DeviceUseStatement"));
    assert!(!Model::of(FhirVersion::R5).has_resource("DeviceUseStatement"));
}
