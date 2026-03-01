use crate::handler::{JobContext, JobHandler, UnitOutcome};
use fhir_core::Error;
use fhir_store::{JobKind, JobProgress, JobRecord, JobResult, JobSignal, JobState, JobStore};
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct Orchestrator {
    handlers: BTreeMap<JobKind, Arc<dyn JobHandler>>,
    telemetry: Arc<fhir_telemetry::Telemetry>,
    ticker: fhir_store::Ticker,
}

pub fn measured(kind: JobKind) -> fhir_telemetry::Operation {
    match kind {
        JobKind::Import => fhir_telemetry::Operation::Import,
        JobKind::Export => fhir_telemetry::Operation::Export,
        JobKind::BulkDelete => fhir_telemetry::Operation::BulkDelete,
        JobKind::BulkUpdate => fhir_telemetry::Operation::BulkUpdate,
        JobKind::Reindex => fhir_telemetry::Operation::Reindex,
        JobKind::Interaction => fhir_telemetry::Operation::Search,
    }
}

fn ended(result: &JobResult) -> fhir_telemetry::Outcome {
    match result {
        JobResult::Succeeded(_) | JobResult::Cancelled => fhir_telemetry::Outcome::Success,
        JobResult::Rejected(_) => fhir_telemetry::Outcome::ClientFault,
        JobResult::Failed(_) => fhir_telemetry::Outcome::ServerFault,
    }
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
            telemetry: Arc::new(fhir_telemetry::Telemetry::silent()),
            ticker: fhir_store::system_ticker(),
        }
    }

    pub fn with(mut self, handler: Arc<dyn JobHandler>) -> Orchestrator {
        self.handlers.insert(handler.kind(), handler);
        self
    }

    pub fn reporting(self, telemetry: Arc<fhir_telemetry::Telemetry>) -> Orchestrator {
        Orchestrator { telemetry, ..self }
    }

    pub fn timed(self, ticker: fhir_store::Ticker) -> Orchestrator {
        Orchestrator { ticker, ..self }
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
        let started = (self.ticker)();
        let handler = match self.handler(record.kind) {
            Ok(handler) => handler,
            Err(error) => {
                let refused = JobResult::Rejected(error.to_string());
                return self.concluded(jobs, record, worker, refused, started).await;
            }
        };
        let context = JobContext::of(record);
        let units = match handler.plan(&context).await {
            Ok(units) => units,
            Err(error) => {
                let refused = JobResult::Rejected(error.to_string());
                return self.concluded(jobs, record, worker, refused, started).await;
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
                return self
                    .concluded(jobs, record, worker, JobResult::Cancelled, started)
                    .await;
            }
            match handler.process(&context, unit).await {
                Ok(outcome) => {
                    summary.handled += outcome.handled;
                    summary.unchanged += outcome.unchanged;
                    summary.failures.extend(outcome.failures);
                    summary.detail.extend(outcome.detail);
                }
                Err(error) => {
                    let failed = JobResult::Failed(error.to_string());
                    return self.concluded(jobs, record, worker, failed, started).await;
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
        let done = JobResult::Succeeded(report(total, &summary));
        self.concluded(jobs, record, worker, done, started).await
    }

    async fn concluded(
        &self,
        jobs: &dyn JobStore,
        record: &JobRecord,
        worker: &str,
        result: JobResult,
        started: i64,
    ) -> Result<JobRecord, Error> {
        let dimensions = fhir_telemetry::Dimensions::of(measured(record.kind), ended(&result));
        let correlation = record.correlation.clone();
        let finished = jobs.finish(&record.id, worker, result).await?;
        let millis = (self.ticker)().saturating_sub(started).max(0) as u64;
        self.telemetry.record_for(dimensions, millis, correlation);
        Ok(finished)
    }
}

fn report(units: u64, summary: &UnitOutcome) -> String {
    let mut report = serde_json::Map::new();
    report.insert("units".to_owned(), serde_json::Value::from(units));
    report.insert(
        "handled".to_owned(),
        serde_json::Value::from(summary.handled),
    );
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

fn named() -> String {
    format!("worker-{}", fhir_core::CorrelationId::fresh().as_str())
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

    pub fn per_instance(
        jobs: Arc<dyn JobStore>,
        orchestrator: Arc<Orchestrator>,
        duration: i64,
    ) -> Worker {
        Worker::new(jobs, orchestrator, named(), duration)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn stopping(&self) -> Result<usize, Error> {
        self.jobs
            .hand_over(&self.name)
            .await
            .map(|moved| moved.len())
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
