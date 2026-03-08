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
    assert!(!Model::of(FhirVersion::Stu3).has_resource("EvidenceVariable"));
    for version in [FhirVersion::R4, FhirVersion::R4b, FhirVersion::R5] {
        assert!(
            Model::of(version).has_resource("EvidenceVariable"),
            "{version}"
        );
    }
}

#[test]
fn the_names_the_service_parses_are_the_names_the_versions_define() {
    let union: std::collections::BTreeSet<String> = FhirVersion::ALL
        .into_iter()
        .flat_map(|version| {
            Model::of(version)
                .resources()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    for name in &union {
        assert!(
            name.parse::<fhir_core::ResourceType>().is_ok(),
            "{name} is defined by a version and is not parsed"
        );
    }
    for held in fhir_core::ResourceType::all() {
        assert!(
            union.contains(held.as_str()),
            "{held} is parsed and no version defines it"
        );
    }
}

#[test]
fn a_type_a_version_does_not_define_is_not_served_by_it() {
    for version in FhirVersion::ALL {
        let model = Model::of(version);
        let served: Vec<String> = fhir_core::ResourceType::served(version)
            .iter()
            .map(|held| held.to_string())
            .collect();
        assert_eq!(served.len(), model.resources().count(), "{version}");
        assert!(
            served.iter().all(|name| model.has_resource(name)),
            "{version}"
        );
    }
    assert!(fhir_core::ResourceType::served(FhirVersion::Stu3)
        .iter()
        .all(|held| held.as_str() != "Citation"));
    assert!(fhir_core::ResourceType::served(FhirVersion::R5)
        .iter()
        .any(|held| held.as_str() == "Citation"));
}

#[test]
fn a_status_every_version_requires_of_an_observation_is_reported_when_it_is_absent() {
    for version in FhirVersion::ALL {
        let body = serde_json::json!({"resourceType": "Observation", "code": {"text": "x"}});
        let findings = Model::of(version).check(&body);
        assert!(
            findings.iter().any(|finding| {
                finding.rule == Rule::Cardinality && finding.path == "Observation.status"
            }),
            "{version}: {findings:?}"
        );
    }
}

#[test]
fn the_codes_administrative_gender_publishes_are_the_codes_a_patient_may_carry() {
    for version in FhirVersion::ALL {
        for code in ["male", "female", "other", "unknown"] {
            let body = serde_json::json!({"resourceType": "Patient", "gender": code});
            assert_eq!(
                Model::of(version).check(&body),
                Vec::new(),
                "{version} {code}"
            );
        }
        for code in ["Male", "m", "lady"] {
            let body = serde_json::json!({"resourceType": "Patient", "gender": code});
            assert!(
                Model::of(version)
                    .check(&body)
                    .iter()
                    .any(|finding| finding.rule == Rule::Binding),
                "{version} {code:?}"
            );
        }
        let empty = serde_json::json!({"resourceType": "Patient", "gender": ""});
        assert!(!Model::of(version).check(&empty).is_empty(), "{version}");
    }
}

#[test]
fn a_date_is_a_year_a_month_or_a_day_and_nothing_longer() {
    for version in FhirVersion::ALL {
        for held in ["1980", "1980-04", "1980-04-01"] {
            let body = serde_json::json!({"resourceType": "Patient", "birthDate": held});
            assert_eq!(
                Model::of(version).check(&body),
                Vec::new(),
                "{version} {held}"
            );
        }
        for held in ["19800401", "1980-04-01T00:00:00Z", "80-04-01", "1980-13-01"] {
            let body = serde_json::json!({"resourceType": "Patient", "birthDate": held});
            assert!(
                !Model::of(version).check(&body).is_empty(),
                "{version} accepted birthDate {held:?}"
            );
        }
    }
}

#[test]
fn a_choice_is_named_by_the_type_it_carries() {
    for version in FhirVersion::ALL {
        let held = serde_json::json!({"resourceType": "Patient", "deceasedBoolean": true});
        assert_eq!(Model::of(version).check(&held), Vec::new(), "{version}");
        let dated =
            serde_json::json!({"resourceType": "Patient", "deceasedDateTime": "1980-04-01"});
        assert_eq!(Model::of(version).check(&dated), Vec::new(), "{version}");
        let bare = serde_json::json!({"resourceType": "Patient", "deceased": true});
        assert!(
            Model::of(version)
                .check(&bare)
                .iter()
                .any(|finding| finding.rule == Rule::Structure),
            "{version}"
        );
    }
}

#[test]
fn an_element_a_version_dropped_is_served_by_no_later_version() {
    let body =
        serde_json::json!({"resourceType": "Patient", "animal": {"species": {"text": "dog"}}});
    assert_eq!(Model::of(FhirVersion::Stu3).check(&body), Vec::new());
    for version in [FhirVersion::R4, FhirVersion::R4b, FhirVersion::R5] {
        assert!(
            Model::of(version)
                .check(&body)
                .iter()
                .any(|finding| finding.rule == Rule::Structure && finding.path == "Patient.animal"),
            "{version}"
        );
    }
}
