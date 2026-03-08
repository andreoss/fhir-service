use axum::http::header::HeaderMap;
use fhir_core::security::scope::DataAction;
use fhir_core::security::Access;
use fhir_core::{Error, FhirVersion, ResourceEnvelope, ResourceId, ResourceType};
use fhir_store::{AuditEvent, Interaction};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;

pub const HEADER: &str = "x-provenance";

const PROVENANCE: &str = "Provenance";

const TARGET: &str = "target";

const PLACEHOLDER: &str = "Resource/0";

pub fn carried(headers: &HeaderMap, version: FhirVersion) -> Result<Option<Value>, Error> {
    let Some(raw) = headers.get(HEADER) else {
        return Ok(None);
    };
    let text = raw
        .to_str()
        .map_err(|_| Error::InvalidEnvelope(format!("{HEADER} is not ascii")))?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidJson(format!("{HEADER}: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| Error::InvalidEnvelope(format!("{HEADER} does not carry a resource")))?;
    match object.get("resourceType").and_then(Value::as_str) {
        Some(PROVENANCE) => {}
        Some(other) => {
            return Err(Error::InvalidEnvelope(format!(
                "{HEADER} carries a {other}, not a {PROVENANCE}"
            )))
        }
        None => {
            return Err(Error::InvalidEnvelope(format!(
                "{HEADER} names no resource type"
            )))
        }
    }
    if object.contains_key(TARGET) {
        return Err(Error::InvalidEnvelope(format!(
            "{HEADER} already names a target; the server assigns it"
        )));
    }
    judged(version, &value)?;
    Ok(Some(value))
}

fn judged(version: FhirVersion, value: &Value) -> Result<(), Error> {
    let mut candidate = value.clone();
    targeted(&mut candidate, &[PLACEHOLDER.to_owned()])?;
    let findings = fhir_core::Model::of(version).check(&candidate);
    if findings.is_empty() {
        return Ok(());
    }
    let listed: Vec<String> = findings
        .iter()
        .take(3)
        .map(|finding| format!("{} at {}: {}", finding.rule, finding.path, finding.detail))
        .collect();
    Err(Error::InvalidEnvelope(format!(
        "{HEADER}: {}",
        listed.join("; ")
    )))
}

fn targeted(value: &mut Value, references: &[String]) -> Result<(), Error> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| Error::InvalidEnvelope(format!("{HEADER} does not carry a resource")))?;
    let listed: Vec<Value> = references
        .iter()
        .map(|reference| json!({ "reference": reference }))
        .collect();
    object.insert(TARGET.to_owned(), Value::Array(listed));
    Ok(())
}

pub fn reference_of(envelope: &ResourceEnvelope) -> String {
    format!(
        "{}/{}/_history/{}",
        envelope.resource_type(),
        envelope.id(),
        envelope.version_id()
    )
}

pub fn reference_from(location: &str) -> Option<String> {
    let tail = match location.split_once("://") {
        Some((_, rest)) => rest.split_once('/').map(|(_, path)| path)?,
        None => location.trim_start_matches('/'),
    };
    match tail.contains("/_history/") {
        true => Some(tail.to_owned()),
        false => None,
    }
}

pub async fn record(
    state: &AppState,
    access: &Access,
    carried: Option<Value>,
    targets: &[String],
) -> Result<(), Error> {
    let Some(mut value) = carried else {
        return Ok(());
    };
    if targets.is_empty() {
        return Ok(());
    }
    let resource_type = PROVENANCE.parse::<ResourceType>()?;
    if !ResourceType::served(state.version).contains(&resource_type) {
        return Err(Error::UnsupportedParameter(format!(
            "{PROVENANCE} is not served in {}",
            state.version
        )));
    }
    targeted(&mut value, targets)?;
    let id = ResourceId::parse(&Uuid::new_v4().to_string())?;
    if let Some(object) = value.as_object_mut() {
        object.insert("id".to_owned(), Value::String(id.as_str().to_owned()));
    }
    fhir_core::with_assigned_meta(&mut value)?;
    let bytes =
        serde_json::to_vec(&value).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let envelope = ResourceEnvelope::parse(state.version, &bytes)?;
    let stored = state.store.create(envelope).await?;
    state
        .audit
        .record(
            AuditEvent::allowed(&access.actor, DataAction::Write)
                .doing(Interaction::Create)
                .by(access.client.clone())
                .of(Some(resource_type), Some(stored.id().clone())),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(text: &str) -> HeaderMap {
        let mut held = HeaderMap::new();
        held.insert(HEADER, HeaderValue::from_str(text).unwrap());
        held
    }

    fn provenance() -> String {
        json!({
            "resourceType": "Provenance",
            "recorded": "2026-01-01T00:00:00Z",
            "agent": [{"who": {"display": "a clinician"}}]
        })
        .to_string()
    }

    #[test]
    fn a_request_without_the_header_carries_nothing() {
        let held = carried(&HeaderMap::new(), FhirVersion::R4).unwrap();
        assert!(held.is_none());
    }

    #[test]
    fn a_provenance_in_the_header_is_read() {
        let held = carried(&headers(&provenance()), FhirVersion::R4)
            .unwrap()
            .unwrap();
        assert_eq!(held["resourceType"], "Provenance");
        assert!(held.get(TARGET).is_none());
    }

    #[test]
    fn a_header_that_is_not_json_is_refused() {
        let error = carried(&headers("not json"), FhirVersion::R4).unwrap_err();
        assert!(matches!(error, Error::InvalidJson(_)), "{error:?}");
    }

    #[test]
    fn a_header_carrying_another_resource_is_refused() {
        let body = json!({"resourceType": "Patient"}).to_string();
        let error = carried(&headers(&body), FhirVersion::R4).unwrap_err();
        assert!(error.to_string().contains("not a Provenance"), "{error}");
    }

    #[test]
    fn a_header_that_already_names_a_target_is_refused() {
        let body = json!({
            "resourceType": "Provenance",
            "recorded": "2026-01-01T00:00:00Z",
            "target": [{"reference": "Patient/1"}],
            "agent": [{"who": {"display": "a clinician"}}]
        })
        .to_string();
        let error = carried(&headers(&body), FhirVersion::R4).unwrap_err();
        assert!(
            error.to_string().contains("already names a target"),
            "{error}"
        );
    }

    #[test]
    fn a_provenance_that_does_not_validate_is_refused() {
        let body = json!({
            "resourceType": "Provenance",
            "recorded": 5,
            "agent": [{"who": {"display": "a clinician"}}]
        })
        .to_string();
        let error = carried(&headers(&body), FhirVersion::R4).unwrap_err();
        assert!(matches!(error, Error::InvalidEnvelope(_)), "{error:?}");
    }

    #[test]
    fn a_reference_is_taken_from_a_location_that_names_a_version() {
        assert_eq!(
            reference_from("http://localhost/Patient/1/_history/2").as_deref(),
            Some("Patient/1/_history/2")
        );
        assert_eq!(reference_from("http://localhost/Patient/1"), None);
    }
}
