use crate::{Backend, Config};
use fhir_core::Error;
use fhir_store::{BulkStore, JobStore, ResourceStore};
use std::sync::Arc;

#[cfg(feature = "backend-memory")]
use fhir_adapter_memory::MemoryStore;

#[cfg(feature = "backend-document")]
use fhir_adapter_document::DocumentStore;

#[cfg(feature = "backend-relational")]
use fhir_adapter_relational::RelationalStore;

#[cfg(any(feature = "backend-relational", feature = "backend-document"))]
use fhir_store::Namespace;

pub type Stores = (
    Arc<dyn ResourceStore>,
    Option<Arc<dyn JobStore>>,
    Option<Arc<dyn BulkStore>>,
);

pub async fn open(config: &Config) -> Result<Stores, Error> {
    let store = resources(config).await?;
    let jobs = queue(config).await?;
    let outputs = outputs(config).await?;
    Ok((store, jobs, outputs))
}

pub async fn resources(config: &Config) -> Result<Arc<dyn ResourceStore>, Error> {
    match config.backend {
        #[cfg(feature = "backend-memory")]
        Backend::Memory => Ok(Arc::new(MemoryStore::default())),
        #[cfg(not(feature = "backend-memory"))]
        Backend::Memory => Err(Error::Config(
            "memory backend is not enabled in this build; rebuild with --features backend-memory".to_owned(),
        )),
        #[cfg(feature = "backend-relational")]
        Backend::Relational => {
            let store = RelationalStore::connect(&config.database_url, relational_namespace()?).await?;
            store.migrate().await?;
            Ok(Arc::new(store))
        }
        #[cfg(not(feature = "backend-relational"))]
        Backend::Relational => Err(Error::Config(
            "relational backend is not enabled in this build; rebuild with --features backend-relational or set FHIR_BACKEND=memory".to_owned(),
        )),
        #[cfg(feature = "backend-document")]
        Backend::Document => {
            let store = DocumentStore::connect(&config.document_url, document_namespace()?).await?;
            store.initialise().await?;
            Ok(Arc::new(store))
        }
        #[cfg(not(feature = "backend-document"))]
        Backend::Document => Err(Error::Config(
            "document backend is not enabled in this build; rebuild with --features backend-document or set FHIR_BACKEND=memory".to_owned(),
        )),
    }
}

pub async fn queue(config: &Config) -> Result<Option<Arc<dyn JobStore>>, Error> {
    match config.backend {
        #[cfg(feature = "backend-memory")]
        Backend::Memory => Ok(Some(Arc::new(fhir_adapter_memory::MemoryJobStore::default()))),
        #[cfg(not(feature = "backend-memory"))]
        Backend::Memory => Ok(None),
        #[cfg(feature = "backend-relational")]
        Backend::Relational => {
            let store = RelationalStore::connect(&config.database_url, relational_namespace()?).await?;
            Ok(Some(Arc::new(store.jobs())))
        }
        #[cfg(not(feature = "backend-relational"))]
        Backend::Relational => Ok(None),
        Backend::Document => Ok(None),
    }
}

pub async fn outputs(config: &Config) -> Result<Option<Arc<dyn BulkStore>>, Error> {
    match config.backend {
        #[cfg(feature = "backend-memory")]
        Backend::Memory => Ok(Some(Arc::new(fhir_adapter_memory::MemoryBulkStore::new()))),
        #[cfg(not(feature = "backend-memory"))]
        Backend::Memory => Ok(None),
        #[cfg(feature = "backend-relational")]
        Backend::Relational => {
            let store = RelationalStore::connect(&config.database_url, relational_namespace()?).await?;
            Ok(Some(Arc::new(store.outputs())))
        }
        #[cfg(not(feature = "backend-relational"))]
        Backend::Relational => Ok(None),
        Backend::Document => Ok(None),
    }
}

#[cfg(feature = "backend-relational")]
fn relational_namespace() -> Result<Namespace, Error> {
    match std::env::var(fhir_adapter_relational::ENV_NAMESPACE) {
        Ok(name) => Namespace::parse(&name),
        Err(_) => Ok(Namespace::default()),
    }
}

#[cfg(feature = "backend-document")]
fn document_namespace() -> Result<Namespace, Error> {
    match std::env::var(fhir_adapter_document::ENV_NAMESPACE) {
        Ok(name) => Namespace::parse(&name),
        Err(_) => Ok(Namespace::default()),
    }
}
