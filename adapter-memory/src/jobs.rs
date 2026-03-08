use async_trait::async_trait;
use fhir_core::Error;
use fhir_store::{
    system_ticker, JobFilter, JobId, JobKind, JobProgress, JobRecord, JobRequest, JobResult,
    JobSignal, JobState, JobStore, Lease, Ticker, RETRY_BACKOFF,
};
use std::sync::Mutex;

const STOPPED: &str = "the worker holding the lease stopped";

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
            owner: request.owner,
            correlation: request.correlation,
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
            started: None,
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
        let mut running: Vec<usize> = vec![0; JobKind::ALL.len()];
        let mut started: Vec<Option<i64>> = vec![None; JobKind::ALL.len()];
        for record in records.iter() {
            let slot = record.kind.slot();
            if matches!(record.state, JobState::Running)
                || matches!(record.state, JobState::Cancelling)
            {
                running[slot] += 1;
            }
            if let Some(start) = record.started {
                started[slot] = Some(started[slot].map_or(start, |held: i64| held.max(start)));
            }
        }
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
        for slot in ready {
            if taken.len() >= lease.limit {
                break;
            }
            let kind = records[slot].kind;
            let position = kind.slot();
            if !lease
                .limits
                .admits(kind, running[position], started[position], now)
            {
                continue;
            }
            running[position] += 1;
            started[position] = Some(now);
            let record = &mut records[slot];
            record.state = JobState::Running;
            record.attempt += 1;
            record.worker = Some(lease.worker.clone());
            record.lease = Some(now + lease.duration);
            record.started = Some(now);
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
        match record.cancelled {
            true => Ok(JobSignal::Cancel),
            false => Ok(JobSignal::Continue),
        }
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
                record.outcome = Some(message);
                match record.attempt < record.attempts {
                    true => {
                        record.state = JobState::Queued;
                        record.available = now + RETRY_BACKOFF * record.attempt.max(1) as i64;
                    }
                    false => record.state = JobState::Failed,
                }
            }
            JobResult::Rejected(message) => {
                record.state = JobState::Failed;
                record.outcome = Some(message);
            }
            JobResult::Cancelled => {
                record.state = JobState::Cancelled;
            }
        }
        Ok(record.clone())
    }

    async fn hand_over(&self, worker: &str) -> Result<Vec<JobId>, Error> {
        let now = (self.ticker)();
        let mut records = self.held()?;
        let mut moved = Vec::new();
        for record in records.iter_mut() {
            let running = matches!(record.state, JobState::Running | JobState::Cancelling);
            if !running || record.worker.as_deref() != Some(worker) {
                continue;
            }
            record.worker = None;
            record.lease = None;
            record.updated = now;
            record.available = now;
            record.state = match record.cancelled {
                true => JobState::Cancelled,
                false => JobState::Queued,
            };
            moved.push(record.id.clone());
        }
        Ok(moved)
    }

    async fn reclaim(&self) -> Result<Vec<JobId>, Error> {
        let now = (self.ticker)();
        let mut records = self.held()?;
        let mut moved = Vec::new();
        for record in records.iter_mut() {
            let running = matches!(record.state, JobState::Running)
                || matches!(record.state, JobState::Cancelling);
            if !running || !record.lease.is_some_and(|until| until <= now) {
                continue;
            }
            record.worker = None;
            record.lease = None;
            record.updated = now;
            record.state = match (record.cancelled, record.attempt < record.attempts) {
                (true, _) => JobState::Cancelled,
                (false, true) => {
                    record.available = now;
                    JobState::Queued
                }
                (false, false) => {
                    record.outcome = Some(STOPPED.to_owned());
                    JobState::Failed
                }
            };
            moved.push(record.id.clone());
        }
        Ok(moved)
    }

    async fn cancel(&self, id: &JobId) -> Result<JobRecord, Error> {
        let now = (self.ticker)();
        let mut records = self.held()?;
        let record = locate(&mut records, id)?;
        if record.state.is_terminal() {
            return Err(Error::VersionConflict);
        }
        record.cancelled = true;
        record.updated = now;
        match record.state {
            JobState::Queued => {
                record.state = JobState::Cancelled;
                record.worker = None;
                record.lease = None;
            }
            _ => record.state = JobState::Cancelling,
        }
        Ok(record.clone())
    }

    async fn purge(&self, retention: i64) -> Result<usize, Error> {
        let now = (self.ticker)();
        let horizon = now - retention.max(0);
        let mut records = self.held()?;
        let before = records.len();
        records.retain(|record| !(record.state.is_terminal() && record.updated <= horizon));
        let removed = before - records.len();
        records.shrink_to_fit();
        Ok(removed)
    }

    async fn defragment(&self) -> Result<usize, Error> {
        let now = (self.ticker)();
        let mut records = self.held()?;
        let mut compacted = 0;
        for record in records.iter_mut() {
            if record.state.is_terminal() && record.payload.is_some() {
                record.payload = None;
                record.updated = now;
                compacted += 1;
            }
        }
        records.shrink_to_fit();
        Ok(compacted)
    }

    async fn list(&self, filter: &JobFilter) -> Result<Vec<JobRecord>, Error> {
        let records = self.held()?;
        let mut found: Vec<JobRecord> = records
            .iter()
            .filter(|record| filter.admits(record))
            .cloned()
            .collect();
        found.sort_by(|left, right| (right.created, &right.id).cmp(&(left.created, &left.id)));
        Ok(found)
    }
}
