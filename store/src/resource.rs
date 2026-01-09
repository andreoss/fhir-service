use async_trait::async_trait;
use fhir_core::search::ParameterSpec;
use fhir_core::{Error, ResourceEnvelope, ResourceId, VersionId};

use crate::history::{HistoryPage, HistoryQuery, HistoryScope};
use crate::parameter::{IndexReport};
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

    async fn index_parameter(&self, spec: &ParameterSpec) -> Result<IndexReport, Error> {
        let _ = spec;
        Err(Error::UnsupportedParameter(
            "this store holds no parameter index".to_owned(),
        ))
    }

    async fn drop_parameter(&self, url: &str) -> Result<(), Error> {
        let _ = url;
        Err(Error::UnsupportedParameter(
            "this store holds no parameter index".to_owned(),
        ))
    }

    async fn reindex(&self, specs: &[ParameterSpec]) -> Result<Vec<IndexReport>, Error> {
        let _ = specs;
        Err(Error::UnsupportedParameter(
            "this store holds no parameter index".to_owned(),
        ))
    }

    fn index_report(&self, url: &str) -> Option<IndexReport> {
        let _ = url;
        None
    }

    fn health(&self) -> Result<(), Error>;
}