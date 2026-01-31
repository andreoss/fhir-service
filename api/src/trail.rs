use async_trait::async_trait;
use fhir_core::{Error, FhirVersion, ResourceEnvelope};
use fhir_store::{Audit, AuditEvent, ResourceStore};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

const AUDIT_EVENT: &str = "AuditEvent";
const AGENT_TYPE: &str = "http://terminology.hl7.org/CodeSystem/extra-security-role-type";
const ACTION_SYSTEM: &str = "urn:fhir-service:data-action";
const OUTCOME_GRANTED: &str = "0";
const OUTCOME_REFUSED: &str = "8";

pub struct StoredTrail {
    store: Arc<dyn ResourceStore>,
    version: FhirVersion,
}

impl StoredTrail {
    pub fn new(store: Arc<dyn ResourceStore>, version: FhirVersion) -> StoredTrail {
        StoredTrail { store, version }
    }
}

pub fn record(event: &AuditEvent, id: &str) -> Value {
    let mut agent = json!({
        "type": {"coding": [{"system": AGENT_TYPE, "code": "humanuser"}]},
        "who": {"identifier": {"value": event.actor}},
        "requestor": true,
    });
    if let Some(client) = &event.client {
        agent["who"]["identifier"]["system"] = json!(client);
    }
    let mut entry = json!({
        "resourceType": AUDIT_EVENT,
        "id": id,
        "type": {"system": ACTION_SYSTEM, "code": event.action.as_str()},
        "recorded": event.recorded,
        "outcome": match event.granted {
            true => OUTCOME_GRANTED,
            false => OUTCOME_REFUSED,
        },
        "agent": [agent],
    });
    if let Some(kind) = event.resource_type {
        let reference = match &event.resource_id {
            Some(id) => json!({"reference": format!("{}/{}", kind.as_str(), id.as_str())}),
            None => json!({"type": kind.as_str()}),
        };
        entry["entity"] = json!([{"what": reference}]);
    }
    entry
}

#[async_trait]
impl Audit for StoredTrail {
    async fn record(&self, event: AuditEvent) -> Result<(), Error> {
        let id = Uuid::new_v4().to_string();
        let mut body = record(&event, &id);
        fhir_core::with_assigned_meta(&mut body)?;
        let bytes = serde_json::to_vec(&body).map_err(|error| Error::Internal(error.to_string()))?;
        let envelope = ResourceEnvelope::parse(self.version, &bytes)?;
        self.store.create(envelope).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::security::scope::DataAction;
    use fhir_core::ResourceId;

    fn event() -> AuditEvent {
        AuditEvent::allowed("practitioner-1", DataAction::Read)
            .by(Some("app-1".to_owned()))
            .of(
                Some("Patient".parse().unwrap()),
                Some(ResourceId::parse("pt-1").unwrap()),
            )
    }

    #[test]
    fn a_record_names_the_actor_the_action_and_the_resource() {
        let written = record(&event(), "au-1");
        assert_eq!(written["resourceType"], AUDIT_EVENT);
        assert_eq!(written["type"]["code"], "read");
        assert_eq!(written["agent"][0]["who"]["identifier"]["value"], "practitioner-1");
        assert_eq!(written["entity"][0]["what"]["reference"], "Patient/pt-1");
        assert_eq!(written["outcome"], OUTCOME_GRANTED);
        assert!(!written["recorded"].as_str().unwrap_or_default().is_empty());
    }

    #[test]
    fn a_refusal_is_recorded_as_one() {
        let written = record(&event().refused(), "au-2");
        assert_eq!(written["outcome"], OUTCOME_REFUSED);
    }

    #[test]
    fn a_record_without_a_resource_names_none() {
        let bare = AuditEvent::allowed("practitioner-1", DataAction::Export).of(None, None);
        let written = record(&bare, "au-3");
        assert!(written.get("entity").is_none());
        assert_eq!(written["type"]["code"], "export");
    }
}
