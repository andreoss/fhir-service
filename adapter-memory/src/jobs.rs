use async_trait::async_trait;
use fhir_core::Error;
use fhir_store::{
    system_ticker, JobFilter, JobId, JobProgress, JobRecord, JobRequest, JobResult, JobSignal,
    JobState, JobStore, Lease, Ticker,
};
use std::sync::Mutex;

pub struct MemoryJobStore {
    ticker: Ticker,
    records: Mutex<Vec<JobRecord>>,
}

impl Default for MemoryJobStore {
    fn default() -> MemoryJobStore {
        MemoryJobStore::new(system_ticker())
    }
}

impl MemoryJobStore {
    pub fn new(ticker: Ticker) -> MemoryJobStore {
        MemoryJobStore {
            ticker,
            records: Mutex::new(Vec::new()),
        }
    }

    fn held(&self) -> Result<std::sync::MutexGuard<'_, Vec<JobRecord>>, Error> {
        self.records
            .lock()
            .map_err(|_| Error::Internal("the job queue is poisoned".to_owned()))
    }
}

fn locate<'a>(records: &'a mut [JobRecord], id: &JobId) -> Result<&'a mut JobRecord, Error> {
    records
        .iter_mut()
        .find(|record| &record.id == id)
        .ok_or(Error::NotFound)
}

fn owned(record: &JobRecord, worker: &str) -> bool {
    !record.state.is_terminal() && record.worker.as_deref() == Some(worker)
}

#[async_trait]
impl JobStore for MemoryJobStore {
    async fn submit(&self, request: JobRequest) -> Result<JobRecord, Error> {
        let now = (self.ticker)();
        let mut records = self.held()?;
        if records.iter().any(|record| record.id == request.id) {
            return Err(Error::Duplicate(request.id.as_str().to_owned()));
        }
        let record = JobRecord {
            id: request.id,
            kind: request.kind,
            state: JobState::Queued,
            payload: Some(request.payload),
            progress: JobProgress::default(),
            attempt: 0,
            attempts: request.attempts.max(1),
            outcome: None,
            created: now,
            updated: now,
            available: now,
            lease: None,
            worker: None,
            cancelled: false,
        };
        records.push(record.clone());
        Ok(record)
    }

    async fn fetch(&self, id: &JobId) -> Result<JobRecord, Error> {
        let mut records = self.held()?;
        locate(&mut records, id).map(|record| record.clone())
    }

    async fn claim(&self, lease: &Lease) -> Result<Vec<JobRecord>, Error> {
        let now = (self.ticker)();
        let mut records = self.held()?;
        let mut ready: Vec<usize> = records
            .iter()
            .enumerate()
            .filter(|(_, record)| record.state == JobState::Queued && record.available <= now)
            .map(|(slot, _)| slot)
            .collect();
        ready.sort_by(|left, right| {
            let first = &records[*left];
            let second = &records[*right];
            (first.created, &first.id).cmp(&(second.created, &second.id))
        });
        let mut taken = Vec::new();
        for slot in ready.into_iter().take(lease.limit) {
            let record = &mut records[slot];
            record.state = JobState::Running;
            record.attempt += 1;
            record.worker = Some(lease.worker.clone());
            record.lease = Some(now + lease.duration);
            record.updated = now;
            taken.push(record.clone());
        }
        Ok(taken)
    }

    async fn heartbeat(
        &self,
        id: &JobId,
        worker: &str,
        duration: i64,
        progress: Option<JobProgress>,
    ) -> Result<JobSignal, Error> {
        let now = (self.ticker)();
        let mut records = self.held()?;
        let record = locate(&mut records, id)?;
        if !owned(record, worker) {
            return Err(Error::VersionConflict);
        }
        record.lease = Some(now + duration.max(1));
        record.updated = now;
        if let Some(progress) = progress {
            record.progress = progress;
        }
        Ok(JobSignal::Continue)
    }

    async fn finish(
        &self,
        id: &JobId,
        worker: &str,
        result: JobResult,
    ) -> Result<JobRecord, Error> {
        let now = (self.ticker)();
        let mut records = self.held()?;
        let record = locate(&mut records, id)?;
        if !owned(record, worker) {
            return Err(Error::VersionConflict);
        }
        record.worker = None;
        record.lease = None;
        record.updated = now;
        match result {
            JobResult::Succeeded(detail) => {
                record.state = JobState::Completed;
                record.outcome = Some(detail);
            }
            JobResult::Failed(message) => {
                record.state = JobState::Failed;
                record.outcome = Some(message);
            }
            JobResult::Cancelled => {
                record.state = JobState::Cancelled;
            }
        }
        Ok(record.clone())
    }

    async fn list(&self, filter: &JobFilter) -> Result<Vec<JobRecord>, Error> {
        let records = self.held()?;
        let mut found: Vec<JobRecord> = records
            .iter()
            .filter(|record| filter.admits(record))
            .cloned()
            .collect();
        found.sort_by(|left, right| {
            (right.created, &right.id).cmp(&(left.created, &left.id))
        });
        Ok(found)
    }
}
