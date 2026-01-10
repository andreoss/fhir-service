use crate::resource::ResourceStore;
use async_trait::async_trait;
use fhir_core::Error;
use std::sync::Arc;

#[async_trait]
pub trait StoreScope: Send + Sync {
    fn store(&self) -> Arc<dyn ResourceStore>;

    async fn commit(&self) -> Result<(), Error>;

    async fn rollback(&self) -> Result<(), Error>;
}
