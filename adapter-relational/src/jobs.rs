use fhir_store::Namespace;
use crate::store::faulted;
use async_trait::async_trait;
use fhir_core::Error;
use fhir_store::{
    system_ticker, JobFilter, JobId, JobKind, JobProgress, JobRecord, JobRequest, JobResult,
    JobSignal, JobState, JobStore, Lease, Ticker, RETRY_BACKOFF,
};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use std::str::FromStr;

const STOPPED: &str = "the worker holding the lease stopped";

const QUEUE_LOCK: i64 = 0x6a_6f_62_71;

const COLUMNS: &str = "job_id, kind, state, payload, progress_done, progress_total, \
                       progress_detail, attempt, attempts, outcome, created_ms, updated_ms, \
                       available_ms, lease_ms, worker, started_ms, cancelled, owner, \
                       correlation";

pub struct RelationalJobStore {
    pool: PgPool,
    namespace: Namespace,
    ticker: Ticker,
}

impl RelationalJobStore {
    pub fn new(pool: PgPool, namespace: Namespace) -> RelationalJobStore {
        RelationalJobStore {
            pool,
            namespace,
            ticker: system_ticker(),
        }
    }

    pub fn with_ticker(self, ticker: Ticker) -> RelationalJobStore {
        RelationalJobStore { ticker, ..self }
    }

    fn table(&self) -> String {
        format!("{}.job", self.namespace.as_str())
    }

    async fn read(&self, id: &JobId) -> Result<JobRecord, Error> {
        let statement = format!("select {COLUMNS} from {} where job_id = $1", self.table());
        let row = sqlx::query(&statement)
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| faulted("reading a job", error))?;
        row.ok_or(Error::NotFound).and_then(|row| record_of(&row))
    }
}

fn record_of(row: &PgRow) -> Result<JobRecord, Error> {
    let text = |name: &str| -> Result<String, Error> {
        row.try_get::<String, _>(name)
            .map_err(|error| faulted("reading a job column", error))
    };
    let maybe = |name: &str| -> Result<Option<String>, Error> {
        row.try_get::<Option<String>, _>(name)
            .map_err(|error| faulted("reading a job column", error))
    };
    let number = |name: &str| -> Result<i64, Error> {
        row.try_get::<i64, _>(name)
            .map_err(|error| faulted("reading a job column", error))
    };
    let count = |name: &str| -> Result<i32, Error> {
        row.try_get::<i32, _>(name)
            .map_err(|error| faulted("reading a job column", error))
    };
    Ok(JobRecord {
        id: JobId::parse(&text("job_id")?)?,
        owner: maybe("owner")?,
        correlation: maybe("correlation")?
            .and_then(|held: String| fhir_core::CorrelationId::parse(&held).ok()),
        kind: JobKind::from_str(&text("kind")?)?,
        state: JobState::from_str(&text("state")?)?,
        payload: maybe("payload")?,
        progress: JobProgress {
            done: number("progress_done")?.max(0) as u64,
            total: row
                .try_get::<Option<i64>, _>("progress_total")
                .map_err(|error| faulted("reading a job column", error))?
                .map(|total| total.max(0) as u64),
            detail: maybe("progress_detail")?,
        },
        attempt: count("attempt")?.max(0) as u32,
        attempts: count("attempts")?.max(1) as u32,
        outcome: maybe("outcome")?,
        created: number("created_ms")?,
        updated: number("updated_ms")?,
        available: number("available_ms")?,
        lease: row
            .try_get::<Option<i64>, _>("lease_ms")
            .map_err(|error| faulted("reading a job column", error))?,
        worker: maybe("worker")?,
        started: row
            .try_get::<Option<i64>, _>("started_ms")
            .map_err(|error| faulted("reading a job column", error))?,
        cancelled: row
            .try_get::<bool, _>("cancelled")
            .map_err(|error| faulted("reading a job column", error))?,
    })
}

#[async_trait]
impl JobStore for RelationalJobStore {
    async fn submit(&self, request: JobRequest) -> Result<JobRecord, Error> {
        let now = (self.ticker)();
        let statement = format!(
            "insert into {} ({COLUMNS}) values \
             ($1, $2, $3, $4, 0, null, null, 0, $5, null, $6, $6, $6, null, null, null, false, $7, $8) \
             on conflict (job_id) do nothing",
            self.table()
        );
        let written = sqlx::query(&statement)
            .bind(request.id.as_str())
            .bind(request.kind.as_str())
            .bind(JobState::Queued.as_str())
            .bind(&request.payload)
            .bind(request.attempts.max(1) as i32)
            .bind(now)
            .bind(request.owner.as_deref())
            .bind(request.correlation.as_ref().map(|held| held.as_str()))
            .execute(&self.pool)
            .await
            .map_err(|error| faulted("submitting a job", error))?;
        match written.rows_affected() {
            0 => Err(Error::Duplicate(request.id.as_str().to_owned())),
            _ => self.read(&request.id).await,
        }
    }

    async fn fetch(&self, id: &JobId) -> Result<JobRecord, Error> {
        self.read(id).await
    }

    async fn claim(&self, lease: &Lease) -> Result<Vec<JobRecord>, Error> {
        let now = (self.ticker)();
        let table = self.table();
        let kinds: Vec<String> = JobKind::ALL
            .iter()
            .map(|kind| kind.as_str().to_owned())
            .collect();
        let most: Vec<i64> = JobKind::ALL
            .iter()
            .map(|kind| match lease.limits.most_running(*kind) {
                Some(limit) => limit as i64,
                None => -1,
            })
            .collect();
        let gaps: Vec<i64> = JobKind::ALL
            .iter()
            .map(|kind| lease.limits.gap(*kind))
            .collect();
        let statement = format!(
            "with caps as (select * from unnest($1::text[], $2::bigint[], $3::bigint[]) \
             as c(kind, most, gap)), \
             busy as (select kind, count(*) as held from {table} \
             where state in ($4, $5) group by kind), \
             latest as (select kind, max(started_ms) as started from {table} group by kind), \
             ranked as (select j.job_id, \
             row_number() over (partition by j.kind order by j.created_ms, j.job_id) as rank, \
             coalesce(b.held, 0) as held, c.most, c.gap \
             from {table} j \
             join caps c on c.kind = j.kind \
             left join busy b on b.kind = j.kind \
             left join latest l on l.kind = j.kind \
             where j.state = $6 and j.available_ms <= $7 \
             and (c.gap <= 0 or l.started is null or $7 - l.started >= c.gap)), \
             ready as (select job_id from ranked \
             where (most < 0 or held + rank <= most) and (gap <= 0 or rank = 1) \
             order by job_id limit $8) \
             update {table} set state = $4, attempt = attempt + 1, worker = $9, \
             lease_ms = $10, started_ms = $7, updated_ms = $7 \
             where job_id in (select job_id from ready) returning {COLUMNS}"
        );
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| faulted("claiming a job", error))?;
        sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(QUEUE_LOCK)
            .execute(&mut *transaction)
            .await
            .map_err(|error| faulted("holding the queue", error))?;
        let rows = sqlx::query(&statement)
            .bind(&kinds)
            .bind(&most)
            .bind(&gaps)
            .bind(JobState::Running.as_str())
            .bind(JobState::Cancelling.as_str())
            .bind(JobState::Queued.as_str())
            .bind(now)
            .bind(lease.limit as i64)
            .bind(&lease.worker)
            .bind(now + lease.duration)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|error| faulted("claiming a job", error))?;
        transaction
            .commit()
            .await
            .map_err(|error| faulted("claiming a job", error))?;
        let mut claimed: Vec<JobRecord> = rows.iter().map(record_of).collect::<Result<_, _>>()?;
        claimed.sort_by(|left, right| (left.created, &left.id).cmp(&(right.created, &right.id)));
        Ok(claimed)
    }


    async fn heartbeat(
        &self,
        id: &JobId,
        worker: &str,
        duration: i64,
        progress: Option<JobProgress>,
    ) -> Result<JobSignal, Error> {
        let now = (self.ticker)();
        let progress = progress.unwrap_or(JobProgress {
            done: u64::MAX,
            total: None,
            detail: None,
        });
        let keep = progress.done == u64::MAX;
        let statement = format!(
            "update {} set lease_ms = $1, updated_ms = $2, \
             progress_done = case when $3 then progress_done else $4 end, \
             progress_total = case when $3 then progress_total else $5 end, \
             progress_detail = case when $3 then progress_detail else $6 end \
             where job_id = $7 and worker = $8 and state in ($9, $10) returning cancelled",
            self.table()
        );
        let row = sqlx::query(&statement)
            .bind(now + duration.max(1))
            .bind(now)
            .bind(keep)
            .bind(progress.done.min(i64::MAX as u64) as i64)
            .bind(progress.total.map(|total| total.min(i64::MAX as u64) as i64))
            .bind(progress.detail.as_deref())
            .bind(id.as_str())
            .bind(worker)
            .bind(JobState::Running.as_str())
            .bind(JobState::Cancelling.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| faulted("recording a heartbeat", error))?;
        let Some(row) = row else {
            self.read(id).await?;
            return Err(Error::VersionConflict);
        };
        let cancelled: bool = row
            .try_get("cancelled")
            .map_err(|error| faulted("reading a job column", error))?;
        match cancelled {
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
        let held = self.read(id).await?;
        let (state, outcome, available) = match result {
            JobResult::Succeeded(detail) => (JobState::Completed, Some(detail), held.available),
            JobResult::Cancelled => (JobState::Cancelled, held.outcome.clone(), held.available),
            JobResult::Rejected(message) => (JobState::Failed, Some(message), held.available),
            JobResult::Failed(message) => match held.attempt < held.attempts {
                true => (
                    JobState::Queued,
                    Some(message),
                    now + RETRY_BACKOFF * held.attempt.max(1) as i64,
                ),
                false => (JobState::Failed, Some(message), held.available),
            },
        };
        let statement = format!(
            "update {} set state = $1, outcome = $2, available_ms = $3, updated_ms = $4, \
             worker = null, lease_ms = null \
             where job_id = $5 and worker = $6 and state in ($7, $8) returning {COLUMNS}",
            self.table()
        );
        let row = sqlx::query(&statement)
            .bind(state.as_str())
            .bind(outcome)
            .bind(available)
            .bind(now)
            .bind(id.as_str())
            .bind(worker)
            .bind(JobState::Running.as_str())
            .bind(JobState::Cancelling.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| faulted("finishing a job", error))?;
        match row {
            Some(row) => record_of(&row),
            None => Err(Error::VersionConflict),
        }
    }

    async fn hand_over(&self, worker: &str) -> Result<Vec<JobId>, Error> {
        let now = (self.ticker)();
        let statement = format!(
            "update {} set state = case when cancelled then $1 else $2 end, \
             available_ms = $3, updated_ms = $3, worker = null, lease_ms = null \
             where worker = $4 and state in ($5, $6) \
             returning job_id",
            self.table()
        );
        let rows = sqlx::query(&statement)
            .bind(JobState::Cancelled.as_str())
            .bind(JobState::Queued.as_str())
            .bind(now)
            .bind(worker)
            .bind(JobState::Running.as_str())
            .bind(JobState::Cancelling.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| faulted("handing a job back", error))?;
        rows.iter()
            .map(|row| {
                let id: String = row
                    .try_get("job_id")
                    .map_err(|error| faulted("reading a job column", error))?;
                JobId::parse(&id)
            })
            .collect()
    }

    async fn reclaim(&self) -> Result<Vec<JobId>, Error> {
        let now = (self.ticker)();
        let statement = format!(
            "update {} set state = case when cancelled then $1 \
             when attempt < attempts then $2 else $3 end, \
             outcome = case when not cancelled and attempt >= attempts then $4 else outcome end, \
             available_ms = case when not cancelled and attempt < attempts \
             then $5 else available_ms end, \
             updated_ms = $5, worker = null, lease_ms = null \
             where state in ($6, $7) and lease_ms is not null and lease_ms <= $5 \
             returning job_id",
            self.table()
        );
        let rows = sqlx::query(&statement)
            .bind(JobState::Cancelled.as_str())
            .bind(JobState::Queued.as_str())
            .bind(JobState::Failed.as_str())
            .bind(STOPPED)
            .bind(now)
            .bind(JobState::Running.as_str())
            .bind(JobState::Cancelling.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| faulted("reclaiming a job", error))?;
        rows.iter()
            .map(|row| {
                let id: String = row
                    .try_get("job_id")
                    .map_err(|error| faulted("reading a job column", error))?;
                JobId::parse(&id)
            })
            .collect()
    }

    async fn cancel(&self, id: &JobId) -> Result<JobRecord, Error> {
        let now = (self.ticker)();
        let statement = format!(
            "update {} set cancelled = true, updated_ms = $1, \
             state = case when state = $2 then $3 else $4 end, \
             worker = case when state = $2 then null else worker end, \
             lease_ms = case when state = $2 then null else lease_ms end \
             where job_id = $5 and state in ($2, $6, $4) returning {COLUMNS}",
            self.table()
        );
        let row = sqlx::query(&statement)
            .bind(now)
            .bind(JobState::Queued.as_str())
            .bind(JobState::Cancelled.as_str())
            .bind(JobState::Cancelling.as_str())
            .bind(id.as_str())
            .bind(JobState::Running.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| faulted("cancelling a job", error))?;
        match row {
            Some(row) => record_of(&row),
            None => {
                self.read(id).await?;
                Err(Error::VersionConflict)
            }
        }
    }

    async fn purge(&self, retention: i64) -> Result<usize, Error> {
        let horizon = (self.ticker)() - retention.max(0);
        let statement = format!(
            "delete from {} where state in ($1, $2, $3) and updated_ms <= $4",
            self.table()
        );
        let gone = sqlx::query(&statement)
            .bind(JobState::Completed.as_str())
            .bind(JobState::Failed.as_str())
            .bind(JobState::Cancelled.as_str())
            .bind(horizon)
            .execute(&self.pool)
            .await
            .map_err(|error| faulted("purging the queue", error))?;
        Ok(gone.rows_affected() as usize)
    }

    async fn defragment(&self) -> Result<usize, Error> {
        let now = (self.ticker)();
        let statement = format!(
            "update {} set payload = null, updated_ms = $1 \
             where payload is not null and state in ($2, $3, $4)",
            self.table()
        );
        let done = sqlx::query(&statement)
            .bind(now)
            .bind(JobState::Completed.as_str())
            .bind(JobState::Failed.as_str())
            .bind(JobState::Cancelled.as_str())
            .execute(&self.pool)
            .await
            .map_err(|error| faulted("compacting the queue", error))?;
        Ok(done.rows_affected() as usize)
    }

    async fn list(&self, filter: &JobFilter) -> Result<Vec<JobRecord>, Error> {
        let statement = format!(
            "select {COLUMNS} from {} order by created_ms desc, job_id desc",
            self.table()
        );
        let rows = sqlx::query(&statement)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| faulted("listing jobs", error))?;
        let found: Vec<JobRecord> = rows.iter().map(record_of).collect::<Result<_, _>>()?;
        Ok(found
            .into_iter()
            .filter(|record| filter.admits(record))
            .collect())
    }

    async fn health(&self) -> Result<(), Error> {
        let statement = format!("select count(*) from {}", self.table());
        sqlx::query(&statement)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| faulted("reading the queue", error))
            .map(|_| ())
    }
}
