use async_trait::async_trait;
use fhir_core::Error;
use fhir_store::{BulkStore, JobId, Output};
use std::collections::BTreeMap;
use std::sync::Mutex;

type Files = BTreeMap<String, (Output, Vec<u8>)>;

#[derive(Default)]
pub struct MemoryBulkStore {
    jobs: Mutex<BTreeMap<String, Files>>,
}

impl MemoryBulkStore {
    pub fn new() -> MemoryBulkStore {
        MemoryBulkStore::default()
    }

    fn held(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, Files>>, Error> {
        self.jobs
            .lock()
            .map_err(|_| Error::Internal("the output sink is poisoned".to_owned()))
    }
}

#[async_trait]
impl BulkStore for MemoryBulkStore {
    async fn write(&self, job: &JobId, output: &Output, body: &[u8]) -> Result<(), Error> {
        let stored = Output {
            size: body.len(),
            ..output.clone()
        };
        self.held()?
            .entry(job.as_str().to_owned())
            .or_default()
            .insert(output.name.clone(), (stored, body.to_vec()));
        Ok(())
    }

    async fn read(&self, job: &JobId, name: &str) -> Result<Vec<u8>, Error> {
        self.held()?
            .get(job.as_str())
            .and_then(|files| files.get(name))
            .map(|(_, body)| body.clone())
            .ok_or(Error::NotFound)
    }

    async fn list(&self, job: &JobId) -> Result<Vec<Output>, Error> {
        Ok(self
            .held()?
            .get(job.as_str())
            .map(|files| files.values().map(|(output, _)| output.clone()).collect())
            .unwrap_or_default())
    }

    async fn purge(&self, job: &JobId) -> Result<usize, Error> {
        Ok(self
            .held()?
            .remove(job.as_str())
            .map(|files| files.len())
            .unwrap_or(0))
    }

    fn health(&self) -> Result<(), Error> {
        self.held().map(|_| ())
    }
}
