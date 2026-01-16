use async_trait::async_trait;
use fhir_core::Error;
use std::str::FromStr;

const ID_LIMIT: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(String);

impl JobId {
    pub fn parse(raw: &str) -> Result<JobId, Error> {
        let valid = !raw.is_empty()
            && raw.len() <= ID_LIMIT
            && raw
                .chars()
                .all(|letter| letter.is_ascii_alphanumeric() || letter == '.' || letter == '-');
        match valid {
            true => Ok(JobId(raw.to_owned())),
            false => Err(Error::InvalidResourceId(raw.to_owned())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for JobId {
    type Err = Error;

    fn from_str(raw: &str) -> Result<JobId, Error> {
        JobId::parse(raw)
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum JobKind {
    Import,
    Export,
    BulkDelete,
    BulkUpdate,
    Reindex,
}

impl JobKind {
    pub const ALL: [JobKind; 5] = [
        JobKind::Import,
        JobKind::Export,
        JobKind::BulkDelete,
        JobKind::BulkUpdate,
        JobKind::Reindex,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            JobKind::Import => "import",
            JobKind::Export => "export",
            JobKind::BulkDelete => "bulk-delete",
            JobKind::BulkUpdate => "bulk-update",
            JobKind::Reindex => "reindex",
        }
    }

    pub fn slot(&self) -> usize {
        match self {
            JobKind::Import => 0,
            JobKind::Export => 1,
            JobKind::BulkDelete => 2,
            JobKind::BulkUpdate => 3,
            JobKind::Reindex => 4,
        }
    }
}

impl FromStr for JobKind {
    type Err = Error;

    fn from_str(raw: &str) -> Result<JobKind, Error> {
        JobKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == raw)
            .ok_or_else(|| Error::InvalidParameter(format!("unknown job kind {raw:?}")))
    }
}

impl std::fmt::Display for JobKind {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub const ALL: [JobState; 6] = [
        JobState::Queued,
        JobState::Running,
        JobState::Cancelling,
        JobState::Completed,
        JobState::Failed,
        JobState::Cancelled,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Cancelling => "cancelling",
            JobState::Completed => "completed",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(&self) -> bool {
        match self {
            JobState::Queued | JobState::Running | JobState::Cancelling => false,
            JobState::Completed | JobState::Failed | JobState::Cancelled => true,
        }
    }
}

impl FromStr for JobState {
    type Err = Error;

    fn from_str(raw: &str) -> Result<JobState, Error> {
        JobState::ALL
            .into_iter()
            .find(|state| state.as_str() == raw)
            .ok_or_else(|| Error::InvalidParameter(format!("unknown job state {raw:?}")))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobProgress {
    pub done: u64,
    pub total: Option<u64>,
    pub detail: Option<String>,
}

impl JobProgress {
    pub fn of(done: u64, total: u64) -> JobProgress {
        JobProgress {
            done,
            total: Some(total),
            detail: None,
        }
    }

    pub fn percent(&self) -> Option<u8> {
        match self.total {
            Some(total) if total > 0 => Some(((self.done.min(total) * 100) / total) as u8),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobRequest {
    pub id: JobId,
    pub kind: JobKind,
    pub payload: String,
    pub attempts: u32,
}

impl JobRequest {
    pub fn new(id: JobId, kind: JobKind, payload: impl Into<String>) -> JobRequest {
        JobRequest {
            id,
            kind,
            payload: payload.into(),
            attempts: 3,
        }
    }

    pub fn with_attempts(self, attempts: u32) -> JobRequest {
        JobRequest {
            attempts: attempts.max(1),
            ..self
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobRecord {
    pub id: JobId,
    pub kind: JobKind,
    pub state: JobState,
    pub payload: Option<String>,
    pub progress: JobProgress,
    pub attempt: u32,
    pub attempts: u32,
    pub outcome: Option<String>,
    pub created: i64,
    pub updated: i64,
    pub available: i64,
    pub lease: Option<i64>,
    pub worker: Option<String>,
    pub started: Option<i64>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobSignal {
    Continue,
    Cancel,
}

#[derive(Debug, Clone)]
pub enum JobResult {
    Succeeded(String),
    Failed(String),
    Rejected(String),
    Cancelled,
}

pub const RETRY_BACKOFF: i64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobLimits {
    running: [i64; 5],
    gap: [i64; 5],
}

impl Default for JobLimits {
    fn default() -> JobLimits {
        JobLimits::unlimited()
    }
}

impl JobLimits {
    pub const fn unlimited() -> JobLimits {
        JobLimits {
            running: [-1; 5],
            gap: [0; 5],
        }
    }

    pub fn running(mut self, kind: JobKind, most: usize) -> JobLimits {
        self.running[kind.slot()] = most as i64;
        self
    }

    pub fn every(mut self, kind: JobKind, millis: i64) -> JobLimits {
        self.gap[kind.slot()] = millis.max(0);
        self
    }

    pub fn most_running(&self, kind: JobKind) -> Option<usize> {
        match self.running[kind.slot()] {
            limit if limit < 0 => None,
            limit => Some(limit as usize),
        }
    }

    pub fn gap(&self, kind: JobKind) -> i64 {
        self.gap[kind.slot()]
    }

    pub fn admits(&self, kind: JobKind, held: usize, last_start: Option<i64>, now: i64) -> bool {
        let room = self.most_running(kind).is_none_or(|most| held < most);
        let waited = match (self.gap(kind), last_start) {
            (0, _) | (_, None) => true,
            (gap, Some(last)) => now - last >= gap,
        };
        room && waited
    }
}

#[derive(Debug, Clone)]
pub struct Lease {
    pub worker: String,
    pub duration: i64,
    pub limit: usize,
    pub limits: JobLimits,
}

impl Lease {
    pub fn new(worker: impl Into<String>, duration: i64) -> Lease {
        Lease {
            worker: worker.into(),
            duration: duration.max(1),
            limit: 1,
            limits: JobLimits::unlimited(),
        }
    }

    pub fn with_limit(self, limit: usize) -> Lease {
        Lease {
            limit: limit.max(1),
            ..self
        }
    }

    pub fn with_limits(self, limits: JobLimits) -> Lease {
        Lease { limits, ..self }
    }
}

#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    pub kinds: Vec<JobKind>,
    pub states: Vec<JobState>,
}

impl JobFilter {
    pub fn in_state(state: JobState) -> JobFilter {
        JobFilter {
            kinds: Vec::new(),
            states: vec![state],
        }
    }

    pub fn admits(&self, record: &JobRecord) -> bool {
        (self.kinds.is_empty() || self.kinds.contains(&record.kind))
            && (self.states.is_empty() || self.states.contains(&record.state))
    }
}

#[async_trait]
pub trait JobStore: Send + Sync {
    async fn submit(&self, request: JobRequest) -> Result<JobRecord, Error>;

    async fn fetch(&self, id: &JobId) -> Result<JobRecord, Error>;

    async fn claim(&self, lease: &Lease) -> Result<Vec<JobRecord>, Error>;

    async fn heartbeat(
        &self,
        id: &JobId,
        worker: &str,
        duration: i64,
        progress: Option<JobProgress>,
    ) -> Result<JobSignal, Error>;

    async fn finish(&self, id: &JobId, worker: &str, result: JobResult)
        -> Result<JobRecord, Error>;

    async fn reclaim(&self) -> Result<Vec<JobId>, Error>;

    async fn cancel(&self, id: &JobId) -> Result<JobRecord, Error>;

    async fn defragment(&self) -> Result<usize, Error>;

    async fn list(&self, filter: &JobFilter) -> Result<Vec<JobRecord>, Error>;

    fn health(&self) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identifier_admits_letters_digits_dots_and_hyphens() {
        assert_eq!(JobId::parse("job-1.a").unwrap().as_str(), "job-1.a");
        assert_eq!(JobId::parse("job-1.a").unwrap().to_string(), "job-1.a");
        for raw in ["", "a b", "a/b", "a_b", &"a".repeat(ID_LIMIT + 1)] {
            assert!(JobId::parse(raw).is_err(), "{raw:?} should be refused");
        }
    }

    #[test]
    fn every_kind_round_trips_through_its_text() {
        for kind in JobKind::ALL {
            assert_eq!(JobKind::from_str(kind.as_str()).unwrap(), kind);
            assert_eq!(kind.to_string(), kind.as_str());
            assert_eq!(JobKind::ALL[kind.slot()], kind);
        }
        assert!(JobKind::from_str("nothing").is_err());
    }

    #[test]
    fn every_state_round_trips_and_reports_finality() {
        for state in JobState::ALL {
            assert_eq!(JobState::from_str(state.as_str()).unwrap(), state);
        }
        assert!(!JobState::Queued.is_terminal());
        assert!(!JobState::Cancelling.is_terminal());
        assert!(JobState::Failed.is_terminal());
        assert!(JobState::Cancelled.is_terminal());
        assert!(JobState::Completed.is_terminal());
        assert!(JobState::from_str("nothing").is_err());
    }

    #[test]
    fn progress_reports_a_percentage_only_when_a_total_is_known() {
        assert_eq!(JobProgress::of(1, 4).percent(), Some(25));
        assert_eq!(JobProgress::of(9, 4).percent(), Some(100));
        assert_eq!(JobProgress::of(1, 0).percent(), None);
        assert_eq!(JobProgress::default().percent(), None);
    }

    #[test]
    fn a_request_allows_at_least_one_attempt() {
        let request = JobRequest::new(JobId::parse("r1").unwrap(), JobKind::Export, "{}");
        assert_eq!(request.attempts, 3);
        assert_eq!(request.with_attempts(0).attempts, 1);
    }

    #[test]
    fn a_lease_hands_out_at_least_one_job() {
        let lease = Lease::new("one", 0);
        assert_eq!(lease.duration, 1);
        assert_eq!(lease.limit, 1);
        assert_eq!(lease.with_limit(0).limit, 1);
    }
}
