use async_trait::async_trait;
use fhir_core::{Error, ResourceEnvelope, ResourceId, VersionId};

#[async_trait]
pub trait ResourceStore: Send + Sync {
    async fn create(&self, envelope: ResourceEnvelope) -> Result<ResourceEnvelope, Error>;

    async fn read(&self, id: &ResourceId) -> Result<ResourceEnvelope, Error>;

    async fn vread(&self, id: &ResourceId, version: &VersionId) -> Result<ResourceEnvelope, Error>;

    async fn update(
        &self,
        envelope: ResourceEnvelope,
        expected_version: Option<&VersionId>,
    ) -> Result<ResourceEnvelope, Error>;
}