use async_trait::async_trait;
use fhir_core::terminology::{Coding, Expansion, ExpansionRequest};
use fhir_core::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsumption {
    Below,
    Above,
}

#[async_trait]
pub trait Terminology: Send + Sync {
    async fn expand(&self, url: &str, request: &ExpansionRequest) -> Result<Expansion, Error>;

    
    
    
    
    async fn code_system(
        &self,
        url: &str,
        version: Option<&str>,
    ) -> Result<Option<serde_json::Value>, Error> {
        let _ = (url, version);
        Ok(None)
    }

    
    async fn value_set(
        &self,
        url: &str,
        version: Option<&str>,
    ) -> Result<Option<serde_json::Value>, Error> {
        let _ = (url, version);
        Ok(None)
    }

    
    async fn concept_maps(&self, url: Option<&str>) -> Result<Vec<serde_json::Value>, Error> {
        let _ = url;
        Ok(Vec::new())
    }

    async fn subsumption(
        &self,
        system: Option<&str>,
        code: &str,
        direction: Subsumption,
    ) -> Result<Vec<Coding>, Error>;
}
