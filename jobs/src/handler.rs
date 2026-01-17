use async_trait::async_trait;
use fhir_core::Error;
use fhir_store::{JobId, JobKind, JobRecord};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub label: String,
    pub detail: String,
}

impl Unit {
    pub fn new(label: impl Into<String>, detail: impl Into<String>) -> Unit {
        Unit {
            label: label.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitOutcome {
    pub handled: u64,
    pub failures: Vec<String>,
    pub detail: serde_json::Map<String, serde_json::Value>,
}

impl UnitOutcome {
    pub fn handled(handled: u64) -> UnitOutcome {
        UnitOutcome {
            handled,
            ..UnitOutcome::default()
        }
    }
}

#[async_trait]
pub trait JobHandler: Send + Sync {
    fn kind(&self) -> JobKind;

    async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error>;

    async fn process(&self, job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error>;
}

pub struct JobContext {
    pub id: JobId,
    pub payload: String,
    pub submitted: i64,
}

impl JobContext {
    pub fn new(id: JobId, payload: impl Into<String>, submitted: i64) -> JobContext {
        JobContext {
            id,
            payload: payload.into(),
            submitted,
        }
    }

    pub fn of(record: &JobRecord) -> JobContext {
        JobContext::new(
            record.id.clone(),
            record.payload.clone().unwrap_or_else(|| "{}".to_owned()),
            record.created,
        )
    }
}
