use fhir_core::{FhirInstant, FhirVersion, ResourceEnvelope, ResourceId, VersionId};

pub const SEED: &str = "2026-09-06T04:00:00Z";

pub fn envelope(resource_type: &str, id: &str, body: &str) -> ResourceEnvelope {
    let separator = if body.trim().is_empty() { "" } else { "," };
    let bytes = format!(
        r#"{{"resourceType":"{resource_type}","id":"{id}","meta":{{"versionId":"0","lastUpdated":"{SEED}"}}{separator}{body}}}"#
    )
    .into_bytes();
    ResourceEnvelope::parse(FhirVersion::R4, &bytes).expect("fixture body is a valid envelope")
}

pub fn patient(id: &str, family: &str, active: bool) -> ResourceEnvelope {
    envelope(
        "Patient",
        id,
        &format!(r#""active":{active},"name":[{{"family":"{family}"}}],"birthDate":"1980-05-06""#),
    )
}

pub fn observation(id: &str, code: &str, value: f64, subject: &str) -> ResourceEnvelope {
    envelope(
        "Observation",
        id,
        &format!(
            r#""status":"final","code":{{"coding":[{{"system":"urn:s","code":"{code}"}}]}},"valueQuantity":{{"value":{value},"system":"urn:u","code":"mg"}},"subject":{{"reference":"{subject}"}}"#
        ),
    )
}

pub fn id(value: &str) -> ResourceId {
    ResourceId::parse(value).expect("fixture id is valid")
}

pub fn version(value: &str) -> VersionId {
    VersionId::parse(value).expect("fixture version is valid")
}

pub fn instant(value: &str) -> FhirInstant {
    FhirInstant::parse(value).expect("fixture instant is valid")
}
