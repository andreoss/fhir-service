use fhir_core::{Error, FhirInstant, FhirVersion, ResourceEnvelope, ResourceId, VersionId};

const FIXTURE: &[u8] = include_bytes!("fixtures/patient-r4.json");

fn fixture() -> ResourceEnvelope {
    ResourceEnvelope::parse(FhirVersion::R4, FIXTURE).expect("fixture must parse")
}

#[test]
fn fixture_raw_bytes_are_preserved_verbatim() {
    assert_eq!(fixture().raw(), FIXTURE);
}

#[test]
fn a_round_trip_returns_the_resource_that_went_in() {
    let supplied: serde_json::Value = serde_json::from_slice(FIXTURE).expect("the fixture is json");
    let rendered: serde_json::Value =
        serde_json::from_slice(&fixture().to_json()).expect("the rendering is json");
    assert_eq!(rendered, supplied);

    let reparsed =
        ResourceEnvelope::parse(FhirVersion::R4, &fixture().to_json()).expect("it must reparse");
    assert!(reparsed.content_eq(&fixture()));
    assert_eq!(reparsed.resource_type().as_str(), "Patient");
    assert_eq!(reparsed.id().as_str(), "pt-0001");
    assert_eq!(reparsed.version_id().as_str(), "4");
    assert_eq!(reparsed.last_updated().as_str(), "2026-09-06T04:00:00.000Z");
}

#[test]
fn a_round_trip_of_a_stored_version_returns_that_version() {
    let stored = fixture()
        .stored_with(
            VersionId::parse("5").unwrap(),
            FhirInstant::parse("2026-09-06T05:00:00.000Z").unwrap(),
        )
        .expect("a stored version is built");
    let rendered: serde_json::Value = serde_json::from_slice(&stored.to_json()).unwrap();
    assert_eq!(rendered["meta"]["versionId"], "5");
    assert_eq!(rendered["meta"]["lastUpdated"], "2026-09-06T05:00:00.000Z");
    assert_eq!(rendered["name"][0]["family"], "Smith");
    assert_eq!(rendered["name"][0]["given"][0], "Jane");
    assert_eq!(rendered["active"], true);
    assert!(stored.content_eq(&fixture()));
}

#[test]
fn a_version_code_no_release_publishes_is_rejected() {
    for code in ["", "2", "DSTU2", "R3", "R6", "4.1", "FHIR_R4"] {
        assert!(
            code.parse::<FhirVersion>().is_err(),
            "{code:?} is not a published version code"
        );
    }
    for (code, version) in [
        ("STU3", FhirVersion::Stu3),
        ("R4", FhirVersion::R4),
        ("R4B", FhirVersion::R4b),
        ("R5", FhirVersion::R5),
    ] {
        assert_eq!(code.parse::<FhirVersion>().unwrap(), version);
    }
}

#[test]
fn envelope_metadata_types_round_trip() {
    let envelope = ResourceEnvelope::from_metadata(
        FhirVersion::Stu3,
        "Encounter".parse().unwrap(),
        ResourceId::parse("enc-1").unwrap(),
        VersionId::parse("11").unwrap(),
        FhirInstant::parse("2026-09-06T04:00:00Z").unwrap(),
    );
    let reparsed = ResourceEnvelope::parse(FhirVersion::Stu3, &envelope.to_json()).unwrap();
    assert_eq!(reparsed.resource_type().as_str(), "Encounter");
    assert_eq!(reparsed.id().as_str(), "enc-1");
    assert_eq!(reparsed.version_id().as_str(), "11");
    assert_eq!(reparsed.last_updated().as_str(), "2026-09-06T04:00:00Z");
}

#[test]
fn invalid_envelope_is_rejected_at_the_edge() {
    let corrupted = b"{\"resourceType\":\"Patient\",\"id\":\"pt\",\"meta\":{\"versionId\":\"x/\",\"lastUpdated\":\"2026-09-06T04:00:00Z\"}}";
    let error = ResourceEnvelope::parse(FhirVersion::R4, corrupted)
        .expect_err("invalid envelope must be rejected");
    assert!(matches!(error, Error::InvalidVersion(_)));
    assert_eq!(error.to_operation_outcome().http_status(), 400);
}
