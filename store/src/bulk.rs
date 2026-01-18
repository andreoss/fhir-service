use async_trait::async_trait;
use fhir_core::Error;

use crate::job::JobId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub name: String,
    pub kind: String,
    pub count: u64,
    pub size: usize,
}

impl Output {
    pub fn new(name: impl Into<String>, kind: impl Into<String>, count: u64) -> Output {
        Output {
            name: name.into(),
            kind: kind.into(),
            count,
            size: 0,
        }
    }
}

#[async_trait]
pub trait BulkStore: Send + Sync {
    async fn write(&self, job: &JobId, output: &Output, body: &[u8]) -> Result<(), Error>;

    async fn read(&self, job: &JobId, name: &str) -> Result<Vec<u8>, Error>;

    async fn list(&self, job: &JobId) -> Result<Vec<Output>, Error>;

    async fn purge(&self, job: &JobId) -> Result<usize, Error>;

    fn health(&self) -> Result<(), Error> {
        Ok(())
    }
}

pub const NDJSON: &str = "application/fhir+ndjson";

pub fn output_format(raw: &str) -> Result<String, Error> {
    match raw.replace(' ', "+").as_str() {
        "ndjson" | "application/ndjson" | "application/fhir+ndjson" => Ok(NDJSON.to_owned()),
        other => Err(Error::UnsupportedParameter(format!(
            "_outputFormat {other:?}"
        ))),
    }
}
