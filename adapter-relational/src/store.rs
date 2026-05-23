use crate::compile::Bind;
use crate::extract::{rows_of, Rows};
use crate::fault::{self, Policy};
use crate::migration::Migrator;
use crate::row::{envelope_of, Record, COLUMNS};
use crate::throttle::{Admission, Throttle};
use async_trait::async_trait;
use fhir_core::search::{for_type, ParamDef, ParameterSpec};
use fhir_core::{Error, ResourceEnvelope, ResourceKey, VersionId};
use fhir_store::tuning::Extra;
use fhir_store::Namespace;
use fhir_store::{
    system_clock, Clock, HistoryOrder, HistoryPage, HistoryQuery, HistoryScope, IndexReport,
    PlanCache, PlanStat, ResourceStore, SearchPage, SearchQuery, StoreScope,
};
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::{Mutex, OwnedMutexGuard};

const POOL_SIZE: u32 = 16;
const PROBE: std::time::Duration = std::time::Duration::from_secs(2);
const DEFAULT_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

const RESERVED_SHARE: u32 = 4;

fn admitting(connections: u32, wait: std::time::Duration) -> Throttle {
    let reserved = (connections / RESERVED_SHARE).max(1);
    Throttle::new(connections as usize)
        .reserving(reserved as usize)
        .waiting(wait)
}

const INDEX_TABLES: [&str; 8] = [
    "index_token",
    "index_text",
    "index_number",
    "index_date",
    "index_quantity",
    "index_reference",
    "index_uri",
    "index_sort",
];

pub fn faulted(context: &str, error: sqlx::Error) -> Error {
    fault::classified(context, error)
}

fn body_of(envelope: &ResourceEnvelope) -> Result<Value, Error> {
    serde_json::from_slice(envelope.raw()).map_err(|error| Error::InvalidJson(error.to_string()))
}

fn version_number(version: &VersionId) -> Result<i64, Error> {
    version
        .as_str()
        .parse::<i64>()
        .map_err(|_| Error::Internal(format!("non-numeric version {:?}", version.as_str())))
}

fn next_version(current: &VersionId) -> Result<VersionId, Error> {
    let number = version_number(current)?;
    VersionId::parse(&(number + 1).to_string())
}

fn repeated(envelope: &ResourceEnvelope) -> Error {
    Error::Duplicate(format!(
        "version {} of {:?} is restored twice with a different body",
        envelope.version_id().as_str(),
        envelope.id().as_str()
    ))
}

type Current = Record;

pub struct RelationalStore {
    pool: PgPool,
    namespace: Namespace,
    clock: Clock,
    plans: Arc<PlanCache>,
    custom: Arc<RwLock<HashMap<String, (ParameterSpec, IndexReport)>>>,
    policy: Policy,
    throttle: Arc<Throttle>,
    source: Source,
}

#[derive(Clone)]
enum Source {
    Pool,
    Held(Held),
}

type Held = Arc<Mutex<Option<Transaction<'static, Postgres>>>>;

pub(crate) enum Work {
    Owned(Transaction<'static, Postgres>),
    Held(OwnedMutexGuard<Option<Transaction<'static, Postgres>>>),
}

impl Work {
    pub(crate) fn conn(&mut self) -> Result<&mut PgConnection, Error> {
        match self {
            Work::Owned(transaction) => Ok(transaction),
            Work::Held(guard) => match guard.as_mut() {
                Some(transaction) => Ok(transaction),
                None => Err(Error::Internal("the scope is closed".to_owned())),
            },
        }
    }

    pub(crate) async fn done(self) -> Result<(), Error> {
        match self {
            Work::Owned(transaction) => transaction
                .commit()
                .await
                .map_err(|error| faulted("committing a write", error)),
            Work::Held(_) => Ok(()),
        }
    }
}

impl RelationalStore {
    pub fn new(pool: PgPool, namespace: Namespace) -> RelationalStore {
        RelationalStore {
            pool,
            namespace,
            clock: system_clock(),
            plans: Arc::new(PlanCache::new()),
            custom: Arc::new(RwLock::new(HashMap::new())),
            policy: Policy::default(),
            throttle: Arc::new(Throttle::default()),
            source: Source::Pool,
        }
    }

    pub async fn connect(url: &str, namespace: Namespace) -> Result<RelationalStore, Error> {
        RelationalStore::connect_holding(url, namespace, POOL_SIZE).await
    }

    pub async fn connect_holding(
        url: &str,
        namespace: Namespace,
        connections: u32,
    ) -> Result<RelationalStore, Error> {
        RelationalStore::connect_waiting(url, namespace, connections, DEFAULT_WAIT).await
    }

    pub async fn connect_waiting(
        url: &str,
        namespace: Namespace,
        connections: u32,
        wait: std::time::Duration,
    ) -> Result<RelationalStore, Error> {
        let held = connections.max(1);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(held)
            .acquire_timeout(wait)
            .connect(url)
            .await
            .map_err(|error| Error::Config(format!("the store is not reachable: {error}")))?;
        Ok(RelationalStore::new(pool, namespace).with_throttle(admitting(held, wait)))
    }

    pub fn connect_later(url: &str, namespace: Namespace) -> RelationalStore {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(POOL_SIZE)
            .connect_lazy(url)
            .unwrap_or_else(|_| sqlx::PgPool::connect_lazy("postgres://").expect("a lazy pool"));
        RelationalStore::new(pool, namespace)
    }

    pub fn jobs(&self) -> crate::jobs::RelationalJobStore {
        crate::jobs::RelationalJobStore::new(self.pool.clone(), self.namespace.clone())
    }

    pub fn outputs(&self) -> crate::bulk::RelationalBulkStore {
        crate::bulk::RelationalBulkStore::new(self.pool.clone(), self.namespace.clone())
    }

    pub fn with_clock(self, clock: Clock) -> RelationalStore {
        RelationalStore { clock, ..self }
    }

    pub fn with_policy(self, policy: Policy) -> RelationalStore {
        RelationalStore { policy, ..self }
    }

    pub fn with_throttle(self, throttle: Throttle) -> RelationalStore {
        RelationalStore {
            throttle: Arc::new(throttle),
            ..self
        }
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn throttle(&self) -> &Throttle {
        &self.throttle
    }

    pub(crate) async fn admit(&self) -> Result<Admission, Error> {
        self.throttle.admit().await
    }

    pub(crate) async fn admit_cheap(&self) -> Result<Admission, Error> {
        self.throttle.admit_cheap().await
    }

    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn migrator(&self) -> Migrator {
        Migrator::new(self.pool.clone(), self.namespace.clone())
    }

    pub async fn migrate(&self) -> Result<usize, Error> {
        self.migrator().latest().await
    }

    pub async fn tune(&self, extras: &[Extra]) -> Result<Vec<String>, Error> {
        let mut applied = Vec::new();
        for extra in extras {
            let column = match extra.kind.as_str() {
                "token" => "code",
                "text" => "folded",
                "reference" => "ref_full",
                "date" => "low_secs",
                _ => "value",
            };
            let name = extra.name();
            let statement = format!(
                "create index if not exists {name} on {} (slot, {column}) where param = '{}'",
                self.table(&format!("index_{}", extra.kind)),
                extra.param.replace('\'', "''")
            );
            sqlx::raw_sql(&statement)
                .execute(&self.pool)
                .await
                .map_err(|error| fault::classified("adding an index an operator named", error))?;
            applied.push(name);
        }
        Ok(applied)
    }

    pub fn plans(&self) -> Vec<PlanStat> {
        self.plans.stats()
    }

    pub(crate) fn cache(&self) -> &PlanCache {
        &self.plans
    }

    pub(crate) fn table(&self, name: &str) -> String {
        format!("{}.{name}", self.namespace.as_str())
    }

    pub(crate) fn defs(&self, envelope: &ResourceEnvelope) -> Vec<Arc<ParamDef>> {
        let mut defs = for_type(envelope.resource_type());
        if let Ok(custom) = self.custom.read() {
            for (spec, _) in custom.values() {
                if spec.base.contains(&envelope.resource_type()) {
                    defs.push(Arc::clone(&spec.def));
                }
            }
        }
        defs
    }

    pub(crate) fn remember(&self, spec: ParameterSpec, report: IndexReport) {
        if let Ok(mut custom) = self.custom.write() {
            custom.insert(spec.url.clone(), (spec, report));
        }
    }

    pub(crate) fn forget(&self, url: &str) {
        if let Ok(mut custom) = self.custom.write() {
            custom.remove(url);
        }
    }

    pub(crate) async fn record(&self, report: &IndexReport) -> Result<(), Error> {
        let statement = format!(
            "insert into {} (url, backfilled, indexed, value_count, overflow, failures)
             values ($1, $2::boolean, $3, $4, $5, $6)
             on conflict (url) do update set backfilled = excluded.backfilled,
             indexed = excluded.indexed, value_count = excluded.value_count,
             overflow = excluded.overflow, failures = excluded.failures",
            self.table("parameter_index")
        );
        let binds = [
            Bind::Text(report.url.clone()),
            Bind::Text(report.backfilled.to_string()),
            Bind::Big(report.indexed as i64),
            Bind::Big(report.values as i64),
            Bind::Big(report.overflow as i64),
            Bind::Text(failures_text(&report.failures)),
        ];
        self.ran(&statement, &binds, "recording an index state")
            .await?;
        Ok(())
    }

    pub(crate) async fn recorded(&self, url: &str) -> Result<Option<IndexReport>, Error> {
        let statement = format!(
            "select backfilled, indexed, value_count, overflow, failures from {}
             where url = $1",
            self.table("parameter_index")
        );
        let binds = [Bind::Text(url.to_owned())];
        let Some(row) = self
            .perhaps(&statement, &binds, "reading an index state")
            .await?
        else {
            return Ok(None);
        };
        let read = |name: &str| -> Result<i64, Error> {
            row.try_get(name)
                .map_err(|error| faulted("reading an index state", error))
        };
        let failures: String = row
            .try_get("failures")
            .map_err(|error| faulted("reading an index state", error))?;
        Ok(Some(IndexReport {
            url: url.to_owned(),
            backfilled: row
                .try_get("backfilled")
                .map_err(|error| faulted("reading an index state", error))?,
            indexed: read("indexed")? as usize,
            values: read("value_count")? as usize,
            overflow: read("overflow")? as usize,
            failures: failures_of(&failures),
        }))
    }

    pub(crate) async fn forget_record(&self, url: &str) -> Result<(), Error> {
        let statement = format!(
            "delete from {} where url = $1",
            self.table("parameter_index")
        );
        let binds = [Bind::Text(url.to_owned())];
        self.ran(&statement, &binds, "dropping an index state")
            .await?;
        Ok(())
    }

    pub(crate) async fn work(&self) -> Result<Work, Error> {
        match &self.source {
            Source::Pool => Ok(Work::Owned(
                self.pool
                    .begin()
                    .await
                    .map_err(|error| faulted("starting a write", error))?,
            )),
            Source::Held(held) => Ok(Work::Held(Arc::clone(held).lock_owned().await)),
        }
    }

    fn held(&self) -> bool {
        matches!(self.source, Source::Held(_))
    }

    fn sharing(&self, source: Source) -> RelationalStore {
        RelationalStore {
            pool: self.pool.clone(),
            namespace: self.namespace.clone(),
            clock: Arc::clone(&self.clock),
            plans: Arc::clone(&self.plans),
            custom: Arc::clone(&self.custom),
            policy: self.policy,
            throttle: Arc::clone(&self.throttle),
            source,
        }
    }

    pub(crate) async fn listed(
        &self,
        statement: &str,
        binds: &[Bind],
        context: &str,
    ) -> Result<Vec<PgRow>, Error> {
        match self.held() {
            false => {
                fault::retried(&self.policy, context, || {
                    crate::query::apply(statement, binds).fetch_all(&self.pool)
                })
                .await
            }
            true => {
                let mut work = self.work().await?;
                crate::query::apply(statement, binds)
                    .fetch_all(work.conn()?)
                    .await
                    .map_err(|error| faulted(context, error))
            }
        }
    }

    pub(crate) async fn only(
        &self,
        statement: &str,
        binds: &[Bind],
        context: &str,
    ) -> Result<PgRow, Error> {
        match self.held() {
            false => {
                fault::retried(&self.policy, context, || {
                    crate::query::apply(statement, binds).fetch_one(&self.pool)
                })
                .await
            }
            true => {
                let mut work = self.work().await?;
                crate::query::apply(statement, binds)
                    .fetch_one(work.conn()?)
                    .await
                    .map_err(|error| faulted(context, error))
            }
        }
    }

    pub(crate) async fn perhaps(
        &self,
        statement: &str,
        binds: &[Bind],
        context: &str,
    ) -> Result<Option<PgRow>, Error> {
        match self.held() {
            false => {
                fault::retried(&self.policy, context, || {
                    crate::query::apply(statement, binds).fetch_optional(&self.pool)
                })
                .await
            }
            true => {
                let mut work = self.work().await?;
                crate::query::apply(statement, binds)
                    .fetch_optional(work.conn()?)
                    .await
                    .map_err(|error| faulted(context, error))
            }
        }
    }

    pub(crate) async fn ran(
        &self,
        statement: &str,
        binds: &[Bind],
        context: &str,
    ) -> Result<u64, Error> {
        match self.held() {
            false => {
                let done = fault::retried(&self.policy, context, || {
                    crate::query::apply(statement, binds).execute(&self.pool)
                })
                .await?;
                Ok(done.rows_affected())
            }
            true => {
                let mut work = self.work().await?;
                let done = crate::query::apply(statement, binds)
                    .execute(work.conn()?)
                    .await
                    .map_err(|error| faulted(context, error))?;
                Ok(done.rows_affected())
            }
        }
    }

    async fn current_in(
        &self,
        transaction: &mut PgConnection,
        key: &ResourceKey,
        lock: bool,
    ) -> Result<Option<Current>, Error> {
        let statement = format!(
            "select {COLUMNS} from {} where resource_type = $1 and resource_id = $2 \
             and is_current{}",
            self.table("resource"),
            if lock { " for update" } else { "" }
        );
        let found = sqlx::query(&statement)
            .bind(key.resource_type().as_str())
            .bind(key.id().as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| faulted("reading the current version", error))?;
        found.map(|row| Record::of(&row)).transpose()
    }

    async fn version_in(
        &self,
        transaction: &mut PgConnection,
        key: &ResourceKey,
        version: &VersionId,
    ) -> Result<Option<Record>, Error> {
        let statement = format!(
            "select {COLUMNS} from {} where resource_type = $1 and resource_id = $2 \
             and version_number = $3",
            self.table("resource")
        );
        let found = sqlx::query(&statement)
            .bind(key.resource_type().as_str())
            .bind(key.id().as_str())
            .bind(version_number(version)?)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| faulted("reading a version", error))?;
        found.map(|row| Record::of(&row)).transpose()
    }

    async fn clear_index(
        &self,
        transaction: &mut PgConnection,
        surrogate: i64,
    ) -> Result<(), Error> {
        for table in INDEX_TABLES {
            let statement = format!("delete from {} where surrogate_id = $1", self.table(table));
            sqlx::query(&statement)
                .bind(surrogate)
                .execute(&mut *transaction)
                .await
                .map_err(|error| faulted("clearing index values", error))?;
        }
        Ok(())
    }

    async fn append(
        &self,
        transaction: &mut PgConnection,
        previous: Option<&Current>,
        envelope: &ResourceEnvelope,
    ) -> Result<i64, Error> {
        if let Some(previous) = previous {
            let statement = format!(
                "update {} set is_current = false where surrogate_id = $1",
                self.table("resource")
            );
            sqlx::query(&statement)
                .bind(previous.surrogate)
                .execute(&mut *transaction)
                .await
                .map_err(|error| faulted("superseding a version", error))?;
            self.clear_index(transaction, previous.surrogate).await?;
        }
        let key = envelope.last_updated().key();
        let (packed, encoding) = crate::body::encoded(envelope.raw());
        let statement = format!(
            "insert into {} (resource_type, resource_id, version_number, spec_version,
                             last_updated, updated_secs, updated_nanos, is_deleted, is_current, body, body_encoding)
             values ($1, $2, $3, $4, $5, $6, $7, $8, true, $9, $10)
             returning surrogate_id",
            self.table("resource")
        );
        let row = sqlx::query(&statement)
            .bind(envelope.resource_type().as_str())
            .bind(envelope.id().as_str())
            .bind(version_number(envelope.version_id())?)
            .bind(envelope.version().as_str())
            .bind(envelope.last_updated().as_str())
            .bind(key.seconds())
            .bind(key.nanos() as i32)
            .bind(envelope.is_deleted())
            .bind(packed)
            .bind(encoding.as_str())
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| faulted("writing a version", error))?;
        let surrogate: i64 = row
            .try_get("surrogate_id")
            .map_err(|error| faulted("writing a version", error))?;
        if !envelope.is_deleted() {
            let body = body_of(envelope)?;
            let rows = rows_of(envelope, &body, &self.defs(envelope));
            self.index(transaction, surrogate, &rows).await?;
        }
        Ok(surrogate)
    }

    pub(crate) async fn index(
        &self,
        transaction: &mut PgConnection,
        surrogate: i64,
        rows: &Rows,
    ) -> Result<(), Error> {
        if !rows.tokens.is_empty() {
            let statement = format!(
                "insert into {} (surrogate_id, param, slot, ordinal, system, code, code_tail)
                 select $1, p, s, o, y, c, t
                 from unnest($2::text[], $3::text[], $4::int[], $5::text[], $6::text[], $7::text[])
                 as u(p, s, o, y, c, t)",
                self.table("index_token")
            );
            sqlx::query(&statement)
                .bind(surrogate)
                .bind(
                    rows.tokens
                        .iter()
                        .map(|row| row.param.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.tokens
                        .iter()
                        .map(|row| row.slot.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.tokens
                        .iter()
                        .map(|row| row.ordinal)
                        .collect::<Vec<i32>>(),
                )
                .bind(
                    rows.tokens
                        .iter()
                        .map(|row| row.system.clone())
                        .collect::<Vec<Option<String>>>(),
                )
                .bind(
                    rows.tokens
                        .iter()
                        .map(|row| row.code.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.tokens
                        .iter()
                        .map(|row| row.code_tail.clone())
                        .collect::<Vec<Option<String>>>(),
                )
                .execute(&mut *transaction)
                .await
                .map_err(|error| faulted("indexing coded values", error))?;
        }
        if !rows.texts.is_empty() {
            let statement = format!(
                "insert into {} (surrogate_id, param, slot, ordinal, value, folded)
                 select $1, p, s, o, v, f
                 from unnest($2::text[], $3::text[], $4::int[], $5::text[], $6::text[])
                 as u(p, s, o, v, f)",
                self.table("index_text")
            );
            sqlx::query(&statement)
                .bind(surrogate)
                .bind(
                    rows.texts
                        .iter()
                        .map(|row| row.param.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.texts
                        .iter()
                        .map(|row| row.slot.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.texts
                        .iter()
                        .map(|row| row.ordinal)
                        .collect::<Vec<i32>>(),
                )
                .bind(
                    rows.texts
                        .iter()
                        .map(|row| row.value.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.texts
                        .iter()
                        .map(|row| row.folded.clone())
                        .collect::<Vec<String>>(),
                )
                .execute(&mut *transaction)
                .await
                .map_err(|error| faulted("indexing textual values", error))?;
        }
        if !rows.numbers.is_empty() {
            let statement = format!(
                "insert into {} (surrogate_id, param, slot, ordinal, value)
                 select $1, p, s, o, v
                 from unnest($2::text[], $3::text[], $4::int[], $5::float8[]) as u(p, s, o, v)",
                self.table("index_number")
            );
            sqlx::query(&statement)
                .bind(surrogate)
                .bind(
                    rows.numbers
                        .iter()
                        .map(|row| row.param.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.numbers
                        .iter()
                        .map(|row| row.slot.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.numbers
                        .iter()
                        .map(|row| row.ordinal)
                        .collect::<Vec<i32>>(),
                )
                .bind(
                    rows.numbers
                        .iter()
                        .map(|row| row.value)
                        .collect::<Vec<f64>>(),
                )
                .execute(&mut *transaction)
                .await
                .map_err(|error| faulted("indexing decimal values", error))?;
        }
        if !rows.dates.is_empty() {
            let statement = format!(
                "insert into {} (surrogate_id, param, slot, ordinal, low_secs, low_nanos, high_secs, high_nanos)
                 select $1, p, s, o, a, b, c, d
                 from unnest($2::text[], $3::text[], $4::int[], $5::bigint[], $6::int[], $7::bigint[], $8::int[])
                 as u(p, s, o, a, b, c, d)",
                self.table("index_date")
            );
            sqlx::query(&statement)
                .bind(surrogate)
                .bind(
                    rows.dates
                        .iter()
                        .map(|row| row.param.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.dates
                        .iter()
                        .map(|row| row.slot.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.dates
                        .iter()
                        .map(|row| row.ordinal)
                        .collect::<Vec<i32>>(),
                )
                .bind(
                    rows.dates
                        .iter()
                        .map(|row| row.low_secs)
                        .collect::<Vec<i64>>(),
                )
                .bind(
                    rows.dates
                        .iter()
                        .map(|row| row.low_nanos)
                        .collect::<Vec<i32>>(),
                )
                .bind(
                    rows.dates
                        .iter()
                        .map(|row| row.high_secs)
                        .collect::<Vec<i64>>(),
                )
                .bind(
                    rows.dates
                        .iter()
                        .map(|row| row.high_nanos)
                        .collect::<Vec<i32>>(),
                )
                .execute(&mut *transaction)
                .await
                .map_err(|error| faulted("indexing spans of time", error))?;
        }
        if !rows.quantities.is_empty() {
            let statement = format!(
                "insert into {} (surrogate_id, param, slot, ordinal, value, system, code, structured)
                 select $1, p, s, o, v, y, c, t
                 from unnest($2::text[], $3::text[], $4::int[], $5::float8[], $6::text[], $7::text[], $8::bool[])
                 as u(p, s, o, v, y, c, t)",
                self.table("index_quantity")
            );
            sqlx::query(&statement)
                .bind(surrogate)
                .bind(
                    rows.quantities
                        .iter()
                        .map(|row| row.param.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.quantities
                        .iter()
                        .map(|row| row.slot.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.quantities
                        .iter()
                        .map(|row| row.ordinal)
                        .collect::<Vec<i32>>(),
                )
                .bind(
                    rows.quantities
                        .iter()
                        .map(|row| row.value)
                        .collect::<Vec<f64>>(),
                )
                .bind(
                    rows.quantities
                        .iter()
                        .map(|row| row.system.clone())
                        .collect::<Vec<Option<String>>>(),
                )
                .bind(
                    rows.quantities
                        .iter()
                        .map(|row| row.code.clone())
                        .collect::<Vec<Option<String>>>(),
                )
                .bind(
                    rows.quantities
                        .iter()
                        .map(|row| row.structured)
                        .collect::<Vec<bool>>(),
                )
                .execute(&mut *transaction)
                .await
                .map_err(|error| faulted("indexing measured values", error))?;
        }
        if !rows.references.is_empty() {
            let statement = format!(
                "insert into {} (surrogate_id, param, slot, ordinal, ref_full, ref_id, ref_type)
                 select $1, p, s, o, f, i, t
                 from unnest($2::text[], $3::text[], $4::int[], $5::text[], $6::text[], $7::text[])
                 as u(p, s, o, f, i, t)",
                self.table("index_reference")
            );
            sqlx::query(&statement)
                .bind(surrogate)
                .bind(
                    rows.references
                        .iter()
                        .map(|row| row.param.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.references
                        .iter()
                        .map(|row| row.slot.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.references
                        .iter()
                        .map(|row| row.ordinal)
                        .collect::<Vec<i32>>(),
                )
                .bind(
                    rows.references
                        .iter()
                        .map(|row| row.ref_full.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.references
                        .iter()
                        .map(|row| row.ref_id.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.references
                        .iter()
                        .map(|row| row.ref_type.clone())
                        .collect::<Vec<Option<String>>>(),
                )
                .execute(&mut *transaction)
                .await
                .map_err(|error| faulted("indexing pointers", error))?;
        }
        if !rows.uris.is_empty() {
            let statement = format!(
                "insert into {} (surrogate_id, param, slot, ordinal, value)
                 select $1, p, s, o, v
                 from unnest($2::text[], $3::text[], $4::int[], $5::text[]) as u(p, s, o, v)",
                self.table("index_uri")
            );
            sqlx::query(&statement)
                .bind(surrogate)
                .bind(
                    rows.uris
                        .iter()
                        .map(|row| row.param.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.uris
                        .iter()
                        .map(|row| row.slot.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.uris
                        .iter()
                        .map(|row| row.ordinal)
                        .collect::<Vec<i32>>(),
                )
                .bind(
                    rows.uris
                        .iter()
                        .map(|row| row.value.clone())
                        .collect::<Vec<String>>(),
                )
                .execute(&mut *transaction)
                .await
                .map_err(|error| faulted("indexing identifiers", error))?;
        }
        if !rows.sorts.is_empty() {
            let statement = format!(
                "insert into {} (surrogate_id, param, sort_text)
                 select $1, p, v from unnest($2::text[], $3::text[]) as u(p, v)
                 on conflict (surrogate_id, param) do update set sort_text = excluded.sort_text",
                self.table("index_sort")
            );
            sqlx::query(&statement)
                .bind(surrogate)
                .bind(
                    rows.sorts
                        .iter()
                        .map(|row| row.param.clone())
                        .collect::<Vec<String>>(),
                )
                .bind(
                    rows.sorts
                        .iter()
                        .map(|row| row.sort_text.clone())
                        .collect::<Vec<Option<String>>>(),
                )
                .execute(&mut *transaction)
                .await
                .map_err(|error| faulted("indexing ordering keys", error))?;
        }
        Ok(())
    }

    async fn scoped_rows(
        &self,
        scope: &HistoryScope,
        query: &HistoryQuery,
    ) -> Result<(Vec<PgRow>, i64), Error> {
        let mut conditions: Vec<String> = Vec::new();
        let mut binding = 0;
        let mut resource_type: Option<String> = None;
        let mut resource_id: Option<String> = None;
        match scope {
            HistoryScope::System => {}
            HistoryScope::Type(kind) => {
                binding += 1;
                conditions.push(format!("resource_type = ${binding}"));
                resource_type = Some(kind.as_str().to_owned());
            }
            HistoryScope::Instance(_, id) => {
                binding += 1;
                conditions.push(format!("resource_id = ${binding}"));
                resource_id = Some(id.as_str().to_owned());
            }
        }
        let mut bounds: Vec<(i64, i32)> = Vec::new();
        if let Some(period) = query.since {
            conditions.push(format!(
                "(updated_secs, updated_nanos) >= (${}, ${})",
                binding + 1,
                binding + 2
            ));
            bounds.push((period.low().seconds(), period.low().nanos() as i32));
            binding += 2;
        }
        if let Some(period) = query.before {
            conditions.push(format!(
                "(updated_secs, updated_nanos) < (${}, ${})",
                binding + 1,
                binding + 2
            ));
            bounds.push((period.low().seconds(), period.low().nanos() as i32));
            binding += 2;
        }
        if let Some(period) = query.at {
            conditions.push(format!(
                "(updated_secs, updated_nanos) between (${}, ${}) and (${}, ${})",
                binding + 1,
                binding + 2,
                binding + 3,
                binding + 4
            ));
            bounds.push((period.low().seconds(), period.low().nanos() as i32));
            bounds.push((period.high().seconds(), period.high().nanos() as i32));
            binding += 4;
        }
        let _ = binding;
        let where_clause = match conditions.is_empty() {
            true => String::new(),
            false => format!(" where {}", conditions.join(" and ")),
        };
        let direction = match query.order {
            HistoryOrder::Newest => "desc",
            HistoryOrder::Oldest => "asc",
        };
        let table = self.table("resource");
        let counting = format!("select count(*) as total from {table}{where_clause}");
        let listing = format!(
            "select {COLUMNS} from {table}{where_clause}
             order by updated_secs {direction}, updated_nanos {direction},
                      resource_id {direction}, version_number {direction}
             limit $%L offset $%O"
        );
        let limit = query.count.min(i64::MAX as usize) as i64;
        let offset = query.offset.min(i64::MAX as usize) as i64;
        let listing = listing
            .replace(
                "$%L",
                &format!("${}", bind_count(&resource_type, &resource_id, &bounds) + 1),
            )
            .replace(
                "$%O",
                &format!("${}", bind_count(&resource_type, &resource_id, &bounds) + 2),
            );

        let mut counter = sqlx::query(&counting);
        if let Some(kind) = &resource_type {
            counter = counter.bind(kind.clone());
        }
        if let Some(id) = &resource_id {
            counter = counter.bind(id.clone());
        }
        for (seconds, nanos) in &bounds {
            counter = counter.bind(*seconds).bind(*nanos);
        }
        let mut work = self.work().await?;
        let total: i64 = counter
            .fetch_one(work.conn()?)
            .await
            .map_err(|error| faulted("counting history", error))?
            .try_get("total")
            .map_err(|error| faulted("counting history", error))?;

        let mut lister = sqlx::query(&listing);
        if let Some(kind) = &resource_type {
            lister = lister.bind(kind.clone());
        }
        if let Some(id) = &resource_id {
            lister = lister.bind(id.clone());
        }
        for (seconds, nanos) in &bounds {
            lister = lister.bind(*seconds).bind(*nanos);
        }
        let rows = lister
            .bind(limit)
            .bind(offset)
            .fetch_all(work.conn()?)
            .await
            .map_err(|error| faulted("reading history", error))?;
        Ok((rows, total))
    }
}

fn bind_count(
    resource_type: &Option<String>,
    resource_id: &Option<String>,
    bounds: &[(i64, i32)],
) -> usize {
    usize::from(resource_type.is_some()) + usize::from(resource_id.is_some()) + bounds.len() * 2
}

#[async_trait]
impl ResourceStore for RelationalStore {
    async fn create(&self, envelope: ResourceEnvelope) -> Result<ResourceEnvelope, Error> {
        let _place = self.admit().await?;
        let mut work = self.work().await?;
        let key = ResourceKey::of(&envelope);
        if self.current_in(work.conn()?, &key, true).await?.is_some() {
            return Err(Error::Duplicate(format!("{key} already exists")));
        }
        let first: VersionId = "1".parse()?;
        let stored = envelope.stored_with(first, (self.clock)())?;
        self.append(work.conn()?, None, &stored).await?;
        work.done().await?;
        Ok(stored)
    }

    async fn read(&self, key: &ResourceKey) -> Result<ResourceEnvelope, Error> {
        let _place = self.admit_cheap().await?;
        let statement = format!(
            "select {COLUMNS} from {} where resource_type = $1 and resource_id = $2 \
             and is_current",
            self.table("resource")
        );
        let binds = [
            Bind::Text(key.resource_type().as_str().to_owned()),
            Bind::Text(key.id().as_str().to_owned()),
        ];
        let row = self
            .perhaps(&statement, &binds, "reading a resource")
            .await?;
        match row {
            Some(row) => envelope_of(&row),
            None => Err(Error::NotFound),
        }
    }

    async fn vread(
        &self,
        key: &ResourceKey,
        version: &VersionId,
    ) -> Result<ResourceEnvelope, Error> {
        let _place = self.admit_cheap().await?;
        let statement = format!(
            "select {COLUMNS} from {} where resource_type = $1 and resource_id = $2 \
             and version_number = $3",
            self.table("resource")
        );
        let number = version_number(version)?;
        let binds = [
            Bind::Text(key.resource_type().as_str().to_owned()),
            Bind::Text(key.id().as_str().to_owned()),
            Bind::Big(number),
        ];
        let row = self
            .perhaps(&statement, &binds, "reading a version")
            .await?;
        match row {
            Some(row) => envelope_of(&row),
            None => Err(Error::NotFound),
        }
    }

    async fn update(
        &self,
        envelope: ResourceEnvelope,
        expected_version: Option<&VersionId>,
    ) -> Result<ResourceEnvelope, Error> {
        let _place = self.admit().await?;
        let mut work = self.work().await?;
        let Some(current) = self
            .current_in(work.conn()?, &ResourceKey::of(&envelope), true)
            .await?
        else {
            return Err(Error::NotFound);
        };
        if let Some(expected) = expected_version {
            if &current.version != expected {
                return Err(Error::VersionConflict);
            }
        }
        if current.resource_type != envelope.resource_type() {
            return Err(Error::InvalidEnvelope(format!(
                "resource type mismatch: expected {:?} found {:?}",
                current.resource_type.as_str(),
                envelope.resource_type().as_str()
            )));
        }
        if !current.deleted {
            let held = current.envelope()?;
            if envelope.content_eq(&held) {
                return Ok(held);
            }
        }
        let stored = envelope.stored_with(next_version(&current.version)?, (self.clock)())?;
        self.append(work.conn()?, Some(&current), &stored).await?;
        work.done().await?;
        Ok(stored)
    }

    async fn restore_version(&self, envelope: ResourceEnvelope) -> Result<bool, Error> {
        let _place = self.admit().await?;
        let mut work = self.work().await?;
        if let Some(held) = self
            .version_in(
                work.conn()?,
                &ResourceKey::of(&envelope),
                envelope.version_id(),
            )
            .await?
        {
            let exact = held.last_updated == *envelope.last_updated()
                && held.deleted == envelope.is_deleted()
                && held.envelope()?.content_eq(&envelope);
            if exact {
                return Ok(false);
            }
            return Err(repeated(&envelope));
        }
        let current = self
            .current_in(work.conn()?, &ResourceKey::of(&envelope), true)
            .await?;
        self.append(work.conn()?, current.as_ref(), &envelope)
            .await?;
        work.done().await?;
        Ok(true)
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage, Error> {
        let _place = self.admit().await?;
        crate::query::run(self, query).await
    }

    async fn delete(&self, key: &ResourceKey) -> Result<ResourceEnvelope, Error> {
        let _place = self.admit().await?;
        let mut work = self.work().await?;
        let current = self
            .current_in(work.conn()?, key, true)
            .await?
            .ok_or(Error::NotFound)?;
        if current.deleted {
            return Err(Error::Deleted);
        }
        let marker = ResourceEnvelope::deleted_marker(
            current.spec,
            current.resource_type,
            key.id().clone(),
            next_version(&current.version)?,
            (self.clock)(),
        );
        self.append(work.conn()?, Some(&current), &marker).await?;
        work.done().await?;
        Ok(marker)
    }

    async fn hard_delete(&self, key: &ResourceKey) -> Result<(), Error> {
        let _place = self.admit().await?;
        let statement = format!(
            "delete from {} where resource_type = $1 and resource_id = $2",
            self.table("resource")
        );
        let binds = [
            Bind::Text(key.resource_type().as_str().to_owned()),
            Bind::Text(key.id().as_str().to_owned()),
        ];
        match self.ran(&statement, &binds, "removing a resource").await? {
            0 => Err(Error::NotFound),
            _ => Ok(()),
        }
    }

    async fn erase_versions(&self, key: &ResourceKey, through: &VersionId) -> Result<usize, Error> {
        let _place = self.admit().await?;
        let mut work = self.work().await?;
        let number = version_number(through)?;
        if self.version_in(work.conn()?, key, through).await?.is_none() {
            return Err(Error::NotFound);
        }
        let statement = format!(
            "delete from {} where resource_type = $1 and resource_id = $2 \
             and version_number <= $3",
            self.table("resource")
        );
        let removed = sqlx::query(&statement)
            .bind(key.resource_type().as_str())
            .bind(key.id().as_str())
            .bind(number)
            .execute(work.conn()?)
            .await
            .map_err(|error| faulted("erasing a version", error))?;
        work.done().await?;
        Ok(removed.rows_affected() as usize)
    }

    async fn empty(&self) -> Result<usize, Error> {
        let _place = self.admit().await?;
        let mut work = self.work().await?;

        let statement = format!("delete from {}", self.table("resource"));
        let removed = sqlx::query(&statement)
            .execute(work.conn()?)
            .await
            .map_err(|error| faulted("emptying the store", error))?;
        work.done().await?;
        Ok(removed.rows_affected() as usize)
    }

    async fn purge_history(&self, key: &ResourceKey) -> Result<usize, Error> {
        let _place = self.admit().await?;
        let mut work = self.work().await?;
        if self.current_in(work.conn()?, key, true).await?.is_none() {
            return Err(Error::NotFound);
        }
        let statement = format!(
            "delete from {} where resource_type = $1 and resource_id = $2 and not is_current",
            self.table("resource")
        );
        let removed = sqlx::query(&statement)
            .bind(key.resource_type().as_str())
            .bind(key.id().as_str())
            .execute(work.conn()?)
            .await
            .map_err(|error| faulted("purging history", error))?;
        work.done().await?;
        Ok(removed.rows_affected() as usize)
    }

    async fn history(
        &self,
        scope: &HistoryScope,
        query: &HistoryQuery,
    ) -> Result<HistoryPage, Error> {
        let _place = self.admit().await?;
        if let HistoryScope::Instance(resource_type, id) = scope {
            let statement = format!(
                "select resource_type from {} where resource_type = $1 and resource_id = $2
                 order by version_number limit 1",
                self.table("resource")
            );
            let binds = [
                Bind::Text(resource_type.as_str().to_owned()),
                Bind::Text(id.as_str().to_owned()),
            ];
            self.perhaps(&statement, &binds, "reading a resource")
                .await?
                .ok_or(Error::NotFound)?;
        }
        let (rows, total) = self.scoped_rows(scope, query).await?;
        let entries = rows
            .iter()
            .map(envelope_of)
            .collect::<Result<Vec<ResourceEnvelope>, Error>>()?;
        Ok(HistoryPage {
            entries,
            total: total.max(0) as usize,
            offset: query.offset,
        })
    }

    async fn index_parameter(&self, spec: &ParameterSpec) -> Result<IndexReport, Error> {
        let report = IndexReport::empty(&spec.url);
        self.record(&report).await?;
        self.remember(spec.clone(), report.clone());
        Ok(report)
    }

    async fn drop_parameter(&self, url: &str) -> Result<(), Error> {
        crate::query::drop_index(self, url).await?;
        self.forget_record(url).await?;
        self.forget(url);
        Ok(())
    }

    async fn adopt_parameter(&self, spec: &ParameterSpec) -> Result<(), Error> {
        let report = self
            .recorded(&spec.url)
            .await?
            .unwrap_or_else(|| IndexReport::empty(&spec.url));
        self.remember(spec.clone(), report);
        Ok(())
    }

    async fn reindex(&self, specs: &[ParameterSpec]) -> Result<Vec<IndexReport>, Error> {
        crate::query::reindex(self, specs).await
    }

    async fn reindex_resource(
        &self,
        specs: &[ParameterSpec],
        key: &ResourceKey,
    ) -> Result<Vec<IndexReport>, Error> {
        crate::query::reindex_resource(self, specs, key).await
    }

    async fn index_report(&self, url: &str) -> Result<Option<IndexReport>, Error> {
        self.recorded(url).await
    }

    async fn begin(&self) -> Result<Arc<dyn StoreScope>, Error> {
        if self.held() {
            return Err(Error::Internal("a scope is already open".to_owned()));
        }
        let transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| faulted("starting a scope", error))?;
        let held: Held = Arc::new(Mutex::new(Some(transaction)));
        Ok(Arc::new(RelationalScope {
            store: Arc::new(self.sharing(Source::Held(Arc::clone(&held)))),
            held,
        }))
    }

    async fn health(&self) -> Result<(), Error> {
        if self.pool.is_closed() {
            return Err(Error::Internal("the store is not connected".to_owned()));
        }
        let asked = tokio::time::timeout(PROBE, async {
            sqlx::query("select 1").execute(&self.pool).await
        })
        .await;
        match asked {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(Error::Internal(format!(
                "the store engine did not answer: {error}"
            ))),
            Err(_) => Err(Error::Internal(
                "the store engine did not answer in time".to_owned(),
            )),
        }
    }
}

const RECLAIMED: [&str; 9] = [
    "resource",
    "index_token",
    "index_text",
    "index_number",
    "index_date",
    "index_quantity",
    "index_reference",
    "index_uri",
    "index_sort",
];

impl RelationalStore {
    pub async fn defragment(&self) -> Result<usize, Error> {
        let _place = self.admit().await?;
        for table in RECLAIMED {
            let statement = format!("vacuum (analyze) {}", self.table(table));
            sqlx::raw_sql(&statement)
                .execute(&self.pool)
                .await
                .map_err(|error| faulted("reclaiming space", error))?;
        }
        Ok(RECLAIMED.len())
    }
}

pub struct RelationalScope {
    store: Arc<RelationalStore>,
    held: Held,
}

impl RelationalScope {
    async fn settle(&self, keep: bool) -> Result<(), Error> {
        let Some(transaction) = self.held.lock().await.take() else {
            return Ok(());
        };
        match keep {
            true => transaction
                .commit()
                .await
                .map_err(|error| faulted("committing a scope", error)),
            false => transaction
                .rollback()
                .await
                .map_err(|error| faulted("rolling a scope back", error)),
        }
    }
}

#[async_trait]
impl StoreScope for RelationalScope {
    fn store(&self) -> Arc<dyn ResourceStore> {
        Arc::clone(&self.store) as Arc<dyn ResourceStore>
    }

    async fn commit(&self) -> Result<(), Error> {
        self.settle(true).await
    }

    async fn rollback(&self) -> Result<(), Error> {
        self.settle(false).await
    }
}

fn failures_text(failures: &[fhir_store::IndexFailure]) -> String {
    let held: Vec<Value> = failures
        .iter()
        .map(
            |failure| serde_json::json!({ "resource": failure.resource, "reason": failure.reason }),
        )
        .collect();
    Value::Array(held).to_string()
}

fn failures_of(raw: &str) -> Vec<fhir_store::IndexFailure> {
    let Ok(Value::Array(held)) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    held.iter()
        .filter_map(|item| {
            Some(fhir_store::IndexFailure {
                resource: item.get("resource")?.as_str()?.to_owned(),
                reason: item.get("reason")?.as_str()?.to_owned(),
            })
        })
        .collect()
}
