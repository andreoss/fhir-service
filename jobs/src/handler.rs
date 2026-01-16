use async_trait::async_trait;
use fhir_core::Error;
use fhir_store::JobKind;

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
}

impl UnitOutcome {
    pub fn handled(handled: u64) -> UnitOutcome {
        UnitOutcome {
            handled,
            failures: Vec::new(),
        }
    }
}

#[async_trait]
pub trait JobHandler: Send + Sync {
    fn kind(&self) -> JobKind;

    async fn plan(&self, payload: &str) -> Result<Vec<Unit>, Error>;

    async fn process(&self, unit: &Unit) -> Result<UnitOutcome, Error>;
}
