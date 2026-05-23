use fhir_adapter_memory::MemoryJobStore;
use fhir_core::{Error, FhirVersion};
use fhir_host::Config;
use fhir_jobs::Orchestrator;
use fhir_store::{BulkStore, JobId, JobKind, JobRequest, JobState, JobStore, Lease, ResourceStore};
use std::sync::Arc;

const LEASE_MILLIS: i64 = 600_000;
const WORKER: &str = "offline";

pub struct Session {
    store: Arc<dyn ResourceStore>,
    version: FhirVersion,
}

impl Session {
    pub async fn open(config: &Config) -> Result<Session, Error> {
        Ok(Session {
            store: fhir_host::stores::resources(config).await?.0,
            version: config.version,
        })
    }

    pub fn store(&self) -> Arc<dyn ResourceStore> {
        Arc::clone(&self.store)
    }

    pub fn version(&self) -> FhirVersion {
        self.version
    }
}

pub async fn perform(
    orchestrator: &Orchestrator,
    kind: JobKind,
    payload: &str,
) -> Result<String, Error> {
    let queue = MemoryJobStore::default();
    let id = JobId::parse(WORKER)?;
    queue
        .submit(JobRequest::new(id, kind, payload).with_attempts(1))
        .await?;
    let lease = Lease::new(WORKER, LEASE_MILLIS).with_limit(1);
    let claimed = queue.claim(&lease).await?;
    let Some(record) = claimed.first() else {
        return Err(Error::Internal("the work was not claimed".to_owned()));
    };
    let finished = orchestrator
        .run(&queue, record, WORKER, LEASE_MILLIS)
        .await?;
    let outcome = finished.outcome.clone().unwrap_or_default();
    match finished.state {
        JobState::Completed => Ok(outcome),
        _ => Err(Error::Internal(outcome)),
    }
}

pub type Sink = Arc<dyn BulkStore>;
