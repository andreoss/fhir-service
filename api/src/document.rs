use axum::http::header::HeaderMap;
use axum::response::Response;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use fhir_core::security::scope::DataAction;
use fhir_core::{Error, ResourceEnvelope, ResourceId, ResourceKey, ResourceType};
use fhir_store::Interaction;
use serde_json::{json, Value};
use sha2::{Digest, Sha512};

use crate::app::AppState;
use crate::handlers::{allowed_doing, AppError};

const BINARY: &str = "Binary";

const DOCUMENT_REFERENCE: &str = "DocumentReference";

const ID_LENGTH: usize = 64;

pub fn identified(identifier: &str) -> String {
    let digest = Sha512::digest(identifier.as_bytes());
    let held: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    held.chars().take(ID_LENGTH).collect()
}

fn narrative(bundle: &Value) -> String {
    bundle
        .get("entry")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .and_then(|entry| entry.get("resource"))
        .and_then(|resource| resource.get("text"))
        .and_then(|text| text.get("div"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

pub async fn received(
    state: &AppState,
    bundle: &Value,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let reference_type = DOCUMENT_REFERENCE.parse::<ResourceType>()?;
    allowed_doing(
        state,
        headers,
        DataAction::Write,
        Interaction::Create,
        Some(reference_type),
        None,
    )
    .await?;
    let identifier = bundle
        .get("identifier")
        .and_then(|held| held.get("value"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::InvalidEnvelope(
                "a document carries an identifier, which is what names it".to_owned(),
            )
        })?;
    let id = identified(identifier);
    let held = narrative(bundle);
    if held.is_empty() {
        return Err(Error::InvalidEnvelope(
            "the document carries no narrative, so there is nothing to keep".to_owned(),
        )
        .into());
    }

    let binary_id = ResourceId::parse(&id)?;
    let binary_type = BINARY.parse::<ResourceType>()?;
    let mut binary = json!({
        "resourceType": BINARY,
        "id": id,
        "contentType": "text/html",
        "data": STANDARD.encode(held.as_bytes()),
    });
    fhir_core::with_assigned_meta(&mut binary)?;
    written(state, binary_type, binary_id.clone(), &binary).await?;

    let reference_id = ResourceId::parse(&id)?;
    let mut reference = json!({
        "resourceType": DOCUMENT_REFERENCE,
        "id": id,
        "status": "current",
        "identifier": [{"value": identifier}],
        "date": fhir_store::system_clock()().as_str(),
        "content": [{
            "attachment": {
                "contentType": "text/html",
                "url": format!("{BINARY}/{id}")
            }
        }],
    });
    fhir_core::with_assigned_meta(&mut reference)?;
    let stored = written(state, reference_type, reference_id, &reference).await?;
    Ok(crate::handlers::rendered(stored.raw().to_vec()))
}

async fn written(
    state: &AppState,
    resource_type: ResourceType,
    id: ResourceId,
    value: &Value,
) -> Result<ResourceEnvelope, Error> {
    let envelope = ResourceEnvelope::parse(
        state.version,
        &serde_json::to_vec(value).map_err(|error| Error::Internal(error.to_string()))?,
    )?;
    let key = ResourceKey::new(resource_type, id);
    match state.store.read(&key).await {
        Ok(_) => state.store.update(envelope, None).await,
        Err(Error::NotFound) => state.store.create(envelope).await,
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_identifier_always_gives_the_same_id() {
        let one = identified("urn:uuid:0000-1111");
        assert_eq!(one, identified("urn:uuid:0000-1111"));
        assert_ne!(one, identified("urn:uuid:0000-2222"));
        assert_eq!(one.len(), ID_LENGTH);
        assert!(one.chars().all(|held| held.is_ascii_hexdigit()));
    }

    #[test]
    fn the_narrative_is_the_documents_own_text() {
        let bundle = json!({
            "resourceType": "Bundle",
            "type": "document",
            "entry": [{"resource": {
                "resourceType": "Composition",
                "text": {"status": "generated", "div": "<div>held</div>"}
            }}]
        });
        assert_eq!(narrative(&bundle), "<div>held</div>");
        assert_eq!(narrative(&json!({"resourceType": "Bundle"})), "");
    }
}
