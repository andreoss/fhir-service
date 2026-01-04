use async_trait::async_trait;
use fhir_core::{Error, ResourceEnvelope, ResourceId, ResourceType, VersionId};

pub type SearchParam = (String, String);

pub type SearchParams = Vec<SearchParam>;

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

    async fn search(
        &self,
        resource_type: Option<ResourceType>,
        params: &SearchParams,
    ) -> Result<Vec<ResourceEnvelope>, Error>;

    fn health(&self) -> Result<(), Error>;
}