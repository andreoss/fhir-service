use async_trait::async_trait;
use fhir_core::search::ParameterSpec;
use fhir_core::{Error, ResourceEnvelope, ResourceId, VersionId};

use crate::history::{HistoryPage, HistoryQuery, HistoryScope};
use crate::scope::StoreScope;
use std::sync::Arc;
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

    async fn reindex_resource(
        &self,
        specs: &[ParameterSpec],
        id: &ResourceId,
    ) -> Result<Vec<IndexReport>, Error> {
        let _ = (specs, id);
        Err(Error::UnsupportedParameter(
            "this store holds no parameter index".to_owned(),
        ))
    }

    async fn index_report(&self, url: &str) -> Result<Option<IndexReport>, Error> {
        let _ = url;
        Ok(None)
    }

    async fn adopt_parameter(&self, spec: &ParameterSpec) -> Result<(), Error> {
        let _ = spec;
        Ok(())
    }

    async fn begin(&self) -> Result<Arc<dyn StoreScope>, Error> {
        Err(Error::Internal("this store has no atomic scope".to_owned()))
    }

    async fn health(&self) -> Result<(), Error>;
}
#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::search::ParameterSpec;
    use serde_json::json;

    struct Bare;

    #[async_trait]
    impl ResourceStore for Bare {
        async fn create(&self, _envelope: ResourceEnvelope) -> Result<ResourceEnvelope, Error> {
            Err(Error::NotFound)
        }

        async fn read(&self, _id: &ResourceId) -> Result<ResourceEnvelope, Error> {
            Err(Error::NotFound)
        }

        async fn vread(
            &self,
            _id: &ResourceId,
            _version: &VersionId,
        ) -> Result<ResourceEnvelope, Error> {
            Err(Error::NotFound)
        }

        async fn update(
            &self,
            _envelope: ResourceEnvelope,
            _expected_version: Option<&VersionId>,
        ) -> Result<ResourceEnvelope, Error> {
            Err(Error::NotFound)
        }

        async fn search(&self, _query: &SearchQuery) -> Result<SearchPage, Error> {
            Err(Error::NotFound)
        }

        async fn delete(&self, _id: &ResourceId) -> Result<ResourceEnvelope, Error> {
            Err(Error::NotFound)
        }

        async fn hard_delete(&self, _id: &ResourceId) -> Result<(), Error> {
            Err(Error::NotFound)
        }

        async fn purge_history(&self, _id: &ResourceId) -> Result<usize, Error> {
            Err(Error::NotFound)
        }

        async fn history(
            &self,
            _scope: &HistoryScope,
            _query: &HistoryQuery,
        ) -> Result<HistoryPage, Error> {
            Err(Error::NotFound)
        }

        async fn health(&self) -> Result<(), Error> {
            Ok(())
        }
    }

    fn spec() -> ParameterSpec {
        ParameterSpec::parse(&json!({
            "resourceType": "SearchParameter",
            "url": "urn:p:a",
            "status": "active",
            "code": "a",
            "base": ["Patient"],
            "type": "token",
            "expression": "Patient.extension.valueCode"
        }))
        .expect("a valid definition")
    }

    fn unsupported(error: Error) -> bool {
        matches!(error, Error::UnsupportedParameter(_))
    }

    #[tokio::test]
    async fn a_store_without_an_index_says_so_rather_than_pretending() {
        let store = Bare;
        let id = ResourceId::parse("one").expect("a valid id");
        assert!(unsupported(store.index_parameter(&spec()).await.unwrap_err()));
        assert!(unsupported(store.drop_parameter("urn:p:a").await.unwrap_err()));
        assert!(unsupported(store.reindex(&[spec()]).await.unwrap_err()));
        assert!(unsupported(
            store.reindex_resource(&[spec()], &id).await.unwrap_err()
        ));
        assert_eq!(store.index_report("urn:p:a").await.unwrap(), None);
        assert!(store.adopt_parameter(&spec()).await.is_ok());
    }

    #[tokio::test]
    async fn a_store_without_an_atomic_scope_says_so_rather_than_pretending() {
        let store = Bare;
        assert!(matches!(store.begin().await.err(), Some(Error::Internal(_))));
        assert!(store.health().await.is_ok());
    }
}
