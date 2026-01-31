
use async_trait::async_trait;
use fhir_core::security::scope::DataAction;
use fhir_core::{Error, ResourceId, ResourceType};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub actor: String,
    pub client: Option<String>,
    pub action: DataAction,
    pub resource_type: Option<ResourceType>,
    pub resource_id: Option<ResourceId>,
    pub granted: bool,
    pub recorded: String,
}

#[async_trait]
pub trait Audit: Send + Sync {
    async fn record(&self, event: AuditEvent) -> Result<(), Error>;
}

pub struct Unrecorded;

#[async_trait]
impl Audit for Unrecorded {
    async fn record(&self, _event: AuditEvent) -> Result<(), Error> {
        Ok(())
    }
}

impl AuditEvent {
    pub fn allowed(actor: &str, action: DataAction) -> AuditEvent {
        AuditEvent {
            actor: actor.to_owned(),
            client: None,
            action,
            resource_type: None,
            resource_id: None,
            granted: true,
            recorded: crate::clock::system_clock()().to_string(),
        }
    }

    pub fn of(self, resource_type: Option<ResourceType>, resource_id: Option<ResourceId>) -> AuditEvent {
        AuditEvent {
            resource_type,
            resource_id,
            ..self
        }
    }

    pub fn by(self, client: Option<String>) -> AuditEvent {
        AuditEvent { client, ..self }
    }

    pub fn refused(self) -> AuditEvent {
        AuditEvent {
            granted: false,
            ..self
        }
    }
}
