use crate::handler::{JobContext, JobHandler, UnitOutcome};
use fhir_core::Error;
use fhir_store::{
    JobKind, JobProgress, JobRecord, JobResult, JobSignal, JobStore, JobState,
};
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct Orchestrator {
    handlers: BTreeMap<JobKind, Arc<dyn JobHandler>>,
}

impl Default for Orchestrator {
    fn default() -> Orchestrator {
        Orchestrator::new()
    }
}

impl Orchestrator {
    pub fn new() -> Orchestrator {
        Orchestrator {
            handlers: BTreeMap::new(),
        }
    }

    pub fn with(mut self, handler: Arc<dyn JobHandler>) -> Orchestrator {
        self.handlers.insert(handler.kind(), handler);
        self
    }

    pub fn kinds(&self) -> Vec<JobKind> {
        self.handlers.keys().copied().collect()
    }

    pub fn handler(&self, kind: JobKind) -> Result<Arc<dyn JobHandler>, Error> {
        self.handlers
            .get(&kind)
            .map(Arc::clone)
            .ok_or_else(|| Error::UnsupportedParameter(format!("no handler for {kind} jobs")))
    }

    pub async fn run(
        &self,
        jobs: &dyn JobStore,
        record: &JobRecord,
        worker: &str,
        duration: i64,
    ) -> Result<JobRecord, Error> {
        let handler = match self.handler(record.kind) {
            Ok(handler) => handler,
            Err(error) => {
                return jobs
                    .finish(&record.id, worker, JobResult::Rejected(error.to_string()))
                    .await
            }
        };
        let context = JobContext::of(record);
        let units = match handler.plan(&context).await {
            Ok(units) => units,
            Err(error) => {
                return jobs
                    .finish(&record.id, worker, JobResult::Rejected(error.to_string()))
                    .await
            }
        };
        let total = units.len() as u64;
        let mut summary = UnitOutcome::default();
        for (position, unit) in units.iter().enumerate() {
            let progress = JobProgress {
                done: position as u64,
                total: Some(total),
                detail: Some(unit.label.clone()),
            };
            if jobs
                .heartbeat(&record.id, worker, duration, Some(progress))
                .await?
                == JobSignal::Cancel
            {
                return jobs.finish(&record.id, worker, JobResult::Cancelled).await;
            }
            match handler.process(&context, unit).await {
                Ok(outcome) => {
                    summary.handled += outcome.handled;
                    summary.unchanged += outcome.unchanged;
                    summary.failures.extend(outcome.failures);
                    summary.detail.extend(outcome.detail);
                }
                Err(error) => {
                    return jobs
                        .finish(&record.id, worker, JobResult::Failed(error.to_string()))
                        .await
                }
            }
        }
        let progress = JobProgress {
            done: total,
            total: Some(total),
            detail: None,
        };
        let _ = jobs
            .heartbeat(&record.id, worker, duration, Some(progress))
            .await?;
        jobs.finish(&record.id, worker, JobResult::Succeeded(report(total, &summary)))
            .await
    }
}

fn report(units: u64, summary: &UnitOutcome) -> String {
    let mut report = serde_json::Map::new();
    report.insert("units".to_owned(), serde_json::Value::from(units));
    report.insert("handled".to_owned(), serde_json::Value::from(summary.handled));
    report.insert(
        "unchanged".to_owned(),
        serde_json::Value::from(summary.unchanged),
    );
    report.insert(
        "failures".to_owned(),
        serde_json::Value::from(summary.failures.clone()),
    );
    for (name, value) in &summary.detail {
        report.insert(name.clone(), value.clone());
    }
    serde_json::Value::Object(report).to_string()
}

pub struct Worker {
    jobs: Arc<dyn JobStore>,
    orchestrator: Arc<Orchestrator>,
    name: String,
    duration: i64,
    batch: usize,
    limits: fhir_store::JobLimits,
}

impl Worker {
    pub fn new(
        jobs: Arc<dyn JobStore>,
        orchestrator: Arc<Orchestrator>,
        name: impl Into<String>,
        duration: i64,
    ) -> Worker {
        Worker {
            jobs,
            orchestrator,
            name: name.into(),
            duration: duration.max(1),
            batch: 1,
            limits: fhir_store::JobLimits::unlimited(),
        }
    }

    pub fn with_limits(self, limits: fhir_store::JobLimits) -> Worker {
        Worker { limits, ..self }
    }

    pub fn with_batch(self, batch: usize) -> Worker {
        Worker {
            batch: batch.max(1),
            ..self
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn poll(&self) -> Result<usize, Error> {
        let lease = fhir_store::Lease::new(self.name.clone(), self.duration)
            .with_limit(self.batch)
            .with_limits(self.limits);
        let claimed = self.jobs.claim(&lease).await?;
        let mut ended = 0;
        for record in claimed {
            let finished = self
                .orchestrator
                .run(self.jobs.as_ref(), &record, &self.name, self.duration)
                .await?;
            if finished.state.is_terminal() || finished.state == JobState::Queued {
                ended += 1;
            }
        }
        Ok(ended)
    }
}
