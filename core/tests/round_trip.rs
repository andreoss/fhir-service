use fhir_core::{Error, FhirInstant, FhirVersion, ResourceEnvelope, ResourceId, VersionId};
use std::str::from_utf8;

const FIXTURE: &[u8] = include_bytes!("fixtures/patient-r4.json");

#[test]
fn fixture_raw_bytes_are_preserved_verbatim() {
    let envelope = ResourceEnvelope::parse(FhirVersion::R4, FIXTURE).expect("fixture must parse");
    assert_eq!(envelope.raw(), FIXTURE);
}

#[test]
fn fixture_round_trip_preserves_metadata() {
    let envelope = ResourceEnvelope::parse(FhirVersion::R4, FIXTURE).expect("fixture must parse");
    assert_eq!(envelope.resource_type().as_str(), "Patient");
    assert_eq!(envelope.id().as_str(), "pt-0001");
    assert_eq!(envelope.version_id().as_str(), "4");
    assert_eq!(envelope.last_updated().as_str(), "2026-09-06T04:00:00.000Z");
    assert_eq!(envelope.version(), FhirVersion::R4);

    let rendered = envelope.to_json();
    let reparsed = ResourceEnvelope::parse(FhirVersion::R4, &rendered).expect("rendered fixture must reparse");
    assert_eq!(reparsed.resource_type(), envelope.resource_type());
    assert_eq!(reparsed.id(), envelope.id());
    assert_eq!(reparsed.version_id(), envelope.version_id());
    assert_eq!(reparsed.last_updated(), envelope.last_updated());

    let text = from_utf8(&rendered).unwrap();
    assert!(text.contains("\"resourceType\":\"Patient\""));
    assert!(text.contains("\"id\":\"pt-0001\""));
    assert!(text.contains("\"versionId\":\"4\""));
    assert!(text.contains("\"lastUpdated\":\"2026-09-06T04:00:00.000Z\""));
}

#[test]
fn fixture_rejects_unknown_version_codes() {
    let result = ResourceEnvelope::parse(FhirVersion::R4, FIXTURE);
    assert!(result.is_ok());
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
    let error = ResourceEnvelope::parse(FhirVersion::R4, corrupted).expect_err("invalid envelope must be rejected");
    assert!(matches!(error, Error::InvalidVersion(_)));
    assert_eq!(error.to_operation_outcome().http_status(), 400);
}