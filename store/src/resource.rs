use async_trait::async_trait;
use fhir_core::{Error, ResourceEnvelope, ResourceId, VersionId};

use crate::history::{HistoryPage, HistoryQuery, HistoryScope};
use crate::search::{SearchPage, SearchQuery};

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

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage, Error>;

    async fn delete(&self, id: &ResourceId) -> Result<ResourceEnvelope, Error>;

    async fn hard_delete(&self, id: &ResourceId) -> Result<(), Error>;

    async fn purge_history(&self, id: &ResourceId) -> Result<usize, Error>;

    async fn history(
        &self,
        scope: &HistoryScope,
        query: &HistoryQuery,
    ) -> Result<HistoryPage, Error>;

    fn health(&self) -> Result<(), Error>;
}