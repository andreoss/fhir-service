use crate::extract::{rows_of, Rows};
use crate::fault::{self, Policy};
use crate::migration::Migrator;
use crate::namespace::Namespace;
use crate::row::{envelope_of, Record, COLUMNS};
use async_trait::async_trait;
use fhir_core::search::{for_type, ParamDef, ParameterSpec};
use fhir_core::{Error, ResourceEnvelope, ResourceId, VersionId};
use fhir_store::{
    system_clock, Clock, HistoryOrder, HistoryPage, HistoryQuery, HistoryScope, IndexReport,
    PlanCache, PlanStat, ResourceStore, SearchPage, SearchQuery,
};
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::HashMap;
use crate::throttle::{Admission, Throttle};
use std::sync::{Arc, RwLock};

const POOL_SIZE: u32 = 16;

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

type Current = Record;

pub struct RelationalStore {
    pool: PgPool,
    namespace: Namespace,
    clock: Clock,
    plans: PlanCache,
    custom: RwLock<HashMap<String, (ParameterSpec, IndexReport)>>,
    policy: Policy,
    throttle: Throttle,
}

impl RelationalStore {
    pub fn new(pool: PgPool, namespace: Namespace) -> RelationalStore {
        RelationalStore {
            pool,
            namespace,
            clock: system_clock(),
            plans: PlanCache::new(),
            custom: RwLock::new(HashMap::new()),
            policy: Policy::default(),
            throttle: Throttle::default(),
        }
    }

    pub async fn connect(url: &str, namespace: Namespace) -> Result<RelationalStore, Error> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(POOL_SIZE)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect(url)
            .await
            .map_err(|error| Error::Config(format!("the store is not reachable: {error}")))?;
        Ok(RelationalStore::new(pool, namespace))
    }

    pub fn connect_later(url: &str, namespace: Namespace) -> RelationalStore {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(POOL_SIZE)
            .connect_lazy(url)
            .unwrap_or_else(|_| sqlx::PgPool::connect_lazy("postgres://").expect("a lazy pool"));
        RelationalStore::new(pool, namespace)
    }

    pub fn with_clock(self, clock: Clock) -> RelationalStore {
        RelationalStore { clock, ..self }
    }

    pub fn with_policy(self, policy: Policy) -> RelationalStore {
        RelationalStore { policy, ..self }
    }

    pub fn with_throttle(self, throttle: Throttle) -> RelationalStore {
        RelationalStore { throttle, ..self }
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

    pub(crate) fn reported(&self, url: &str) -> Option<IndexReport> {
        self.custom
            .read()
            .ok()
            .and_then(|custom| custom.get(url).map(|(_, report)| report.clone()))
    }

    async fn current_in(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        id: &ResourceId,
        lock: bool,
    ) -> Result<Option<Current>, Error> {
        let statement = format!(
            "select {COLUMNS} from {} where resource_id = $1 and is_current{}",
            self.table("resource"),
            if lock { " for update" } else { "" }
        );
        let found = sqlx::query(&statement)
            .bind(id.as_str())
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|error| faulted("reading the current version", error))?;
        found.map(|row| Record::of(&row)).transpose()
    }

    async fn clear_index(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        surrogate: i64,
    ) -> Result<(), Error> {
        for table in INDEX_TABLES {
            let statement = format!("delete from {} where surrogate_id = $1", self.table(table));
            sqlx::query(&statement)
                .bind(surrogate)
                .execute(&mut **transaction)
                .await
                .map_err(|error| faulted("clearing index values", error))?;
        }
        Ok(())
    }

    async fn append(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
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
                .execute(&mut **transaction)
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
            .bind(packed).bind(encoding.as_str())
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| faulted("writing a version", error))?;
        let surrogate: i64 = row.try_get("surrogate_id").map_err(|error| faulted("writing a version", error))?;
        if !envelope.is_deleted() {
            let body = body_of(envelope)?;
            let rows = rows_of(envelope, &body, &self.defs(envelope));
            self.index(transaction, surrogate, &rows).await?;
        }
        Ok(surrogate)
    }

    pub(crate) async fn index(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
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
                .bind(rows.tokens.iter().map(|row| row.param.clone()).collect::<Vec<String>>())
                .bind(rows.tokens.iter().map(|row| row.slot.clone()).collect::<Vec<String>>())
                .bind(rows.tokens.iter().map(|row| row.ordinal).collect::<Vec<i32>>())
                .bind(rows.tokens.iter().map(|row| row.system.clone()).collect::<Vec<Option<String>>>())
                .bind(rows.tokens.iter().map(|row| row.code.clone()).collect::<Vec<String>>())
                .bind(rows.tokens.iter().map(|row| row.code_tail.clone()).collect::<Vec<Option<String>>>())
                .execute(&mut **transaction)
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
                .bind(rows.texts.iter().map(|row| row.param.clone()).collect::<Vec<String>>())
                .bind(rows.texts.iter().map(|row| row.slot.clone()).collect::<Vec<String>>())
                .bind(rows.texts.iter().map(|row| row.ordinal).collect::<Vec<i32>>())
                .bind(rows.texts.iter().map(|row| row.value.clone()).collect::<Vec<String>>())
                .bind(rows.texts.iter().map(|row| row.folded.clone()).collect::<Vec<String>>())
                .execute(&mut **transaction)
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
                .bind(rows.numbers.iter().map(|row| row.param.clone()).collect::<Vec<String>>())
                .bind(rows.numbers.iter().map(|row| row.slot.clone()).collect::<Vec<String>>())
                .bind(rows.numbers.iter().map(|row| row.ordinal).collect::<Vec<i32>>())
                .bind(rows.numbers.iter().map(|row| row.value).collect::<Vec<f64>>())
                .execute(&mut **transaction)
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
                .bind(rows.dates.iter().map(|row| row.param.clone()).collect::<Vec<String>>())
                .bind(rows.dates.iter().map(|row| row.slot.clone()).collect::<Vec<String>>())
                .bind(rows.dates.iter().map(|row| row.ordinal).collect::<Vec<i32>>())
                .bind(rows.dates.iter().map(|row| row.low_secs).collect::<Vec<i64>>())
                .bind(rows.dates.iter().map(|row| row.low_nanos).collect::<Vec<i32>>())
                .bind(rows.dates.iter().map(|row| row.high_secs).collect::<Vec<i64>>())
                .bind(rows.dates.iter().map(|row| row.high_nanos).collect::<Vec<i32>>())
                .execute(&mut **transaction)
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
                .bind(rows.quantities.iter().map(|row| row.param.clone()).collect::<Vec<String>>())
                .bind(rows.quantities.iter().map(|row| row.slot.clone()).collect::<Vec<String>>())
                .bind(rows.quantities.iter().map(|row| row.ordinal).collect::<Vec<i32>>())
                .bind(rows.quantities.iter().map(|row| row.value).collect::<Vec<f64>>())
                .bind(rows.quantities.iter().map(|row| row.system.clone()).collect::<Vec<Option<String>>>())
                .bind(rows.quantities.iter().map(|row| row.code.clone()).collect::<Vec<Option<String>>>())
                .bind(rows.quantities.iter().map(|row| row.structured).collect::<Vec<bool>>())
                .execute(&mut **transaction)
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
                .bind(rows.references.iter().map(|row| row.param.clone()).collect::<Vec<String>>())
                .bind(rows.references.iter().map(|row| row.slot.clone()).collect::<Vec<String>>())
                .bind(rows.references.iter().map(|row| row.ordinal).collect::<Vec<i32>>())
                .bind(rows.references.iter().map(|row| row.ref_full.clone()).collect::<Vec<String>>())
                .bind(rows.references.iter().map(|row| row.ref_id.clone()).collect::<Vec<String>>())
                .bind(rows.references.iter().map(|row| row.ref_type.clone()).collect::<Vec<Option<String>>>())
                .execute(&mut **transaction)
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
                .bind(rows.uris.iter().map(|row| row.param.clone()).collect::<Vec<String>>())
                .bind(rows.uris.iter().map(|row| row.slot.clone()).collect::<Vec<String>>())
                .bind(rows.uris.iter().map(|row| row.ordinal).collect::<Vec<i32>>())
                .bind(rows.uris.iter().map(|row| row.value.clone()).collect::<Vec<String>>())
                .execute(&mut **transaction)
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
                .bind(rows.sorts.iter().map(|row| row.param.clone()).collect::<Vec<String>>())
                .bind(rows.sorts.iter().map(|row| row.sort_text.clone()).collect::<Vec<Option<String>>>())
                .execute(&mut **transaction)
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
            .replace("$%L", &format!("${}", bind_count(&resource_type, &resource_id, &bounds) + 1))
            .replace("$%O", &format!("${}", bind_count(&resource_type, &resource_id, &bounds) + 2));

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
        let total: i64 = counter
            .fetch_one(&self.pool)
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
            .fetch_all(&self.pool)
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
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| faulted("starting a write", error))?;
        if self.current_in(&mut transaction, envelope.id(), true).await?.is_some() {
            return Err(Error::Duplicate(format!(
                "id {:?} already exists",
                envelope.id().as_str()
            )));
        }
        let first: VersionId = "1".parse()?;
        let stored = envelope.stored_with(first, (self.clock)())?;
        self.append(&mut transaction, None, &stored).await?;
        transaction
            .commit()
            .await
            .map_err(|error| faulted("committing a write", error))?;
        Ok(stored)
    }

    async fn read(&self, id: &ResourceId) -> Result<ResourceEnvelope, Error> {
        let _place = self.admit().await?;
        let statement = format!(
            "select {COLUMNS} from {} where resource_id = $1 and is_current",
            self.table("resource")
        );
        let row = fault::retried(&self.policy, "reading a resource", || {
            sqlx::query(&statement).bind(id.as_str()).fetch_optional(&self.pool)
        })
        .await?;
        match row {
            Some(row) => envelope_of(&row),
            None => Err(Error::NotFound),
        }
    }

    async fn vread(&self, id: &ResourceId, version: &VersionId) -> Result<ResourceEnvelope, Error> {
        let _place = self.admit().await?;
        let statement = format!(
            "select {COLUMNS} from {} where resource_id = $1 and version_number = $2",
            self.table("resource")
        );
        let number = version_number(version)?;
        let row = fault::retried(&self.policy, "reading a version", || {
            sqlx::query(&statement).bind(id.as_str()).bind(number).fetch_optional(&self.pool)
        })
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
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| faulted("starting a write", error))?;
        let Some(current) = self.current_in(&mut transaction, envelope.id(), true).await? else {
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
        let stored = envelope.stored_with(
            next_version(&current.version)?,
            (self.clock)(),
        )?;
        self.append(&mut transaction, Some(&current), &stored).await?;
        transaction
            .commit()
            .await
            .map_err(|error| faulted("committing a write", error))?;
        Ok(stored)
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage, Error> {
        let _place = self.admit().await?;
        crate::query::run(self, query).await
    }

    async fn delete(&self, id: &ResourceId) -> Result<ResourceEnvelope, Error> {
        let _place = self.admit().await?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| faulted("starting a write", error))?;
        let current = self
            .current_in(&mut transaction, id, true)
            .await?
            .ok_or(Error::NotFound)?;
        if current.deleted {
            return Err(Error::Deleted);
        }
        let marker = ResourceEnvelope::deleted_marker(
            current.spec,
            current.resource_type,
            id.clone(),
            next_version(&current.version)?,
            (self.clock)(),
        );
        self.append(&mut transaction, Some(&current), &marker).await?;
        transaction
            .commit()
            .await
            .map_err(|error| faulted("committing a write", error))?;
        Ok(marker)
    }

    async fn hard_delete(&self, id: &ResourceId) -> Result<(), Error> {
        let _place = self.admit().await?;
        let statement = format!(
            "delete from {} where resource_id = $1",
            self.table("resource")
        );
        let removed = fault::retried(&self.policy, "removing a resource", || {
            sqlx::query(&statement).bind(id.as_str()).execute(&self.pool)
        })
        .await?;
        match removed.rows_affected() {
            0 => Err(Error::NotFound),
            _ => Ok(()),
        }
    }

    async fn purge_history(&self, id: &ResourceId) -> Result<usize, Error> {
        let _place = self.admit().await?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| faulted("starting a purge", error))?;
        if self.current_in(&mut transaction, id, true).await?.is_none() {
            return Err(Error::NotFound);
        }
        let statement = format!(
            "delete from {} where resource_id = $1 and not is_current",
            self.table("resource")
        );
        let removed = sqlx::query(&statement)
            .bind(id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(|error| faulted("purging history", error))?;
        transaction
            .commit()
            .await
            .map_err(|error| faulted("committing a purge", error))?;
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
                "select resource_type from {} where resource_id = $1
                 order by version_number limit 1",
                self.table("resource")
            );
            let row = sqlx::query(&statement)
                .bind(id.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(|error| faulted("reading a resource", error))?
                .ok_or(Error::NotFound)?;
            let found: String = row
                .try_get("resource_type")
                .map_err(|error| faulted("reading a resource", error))?;
            if found != resource_type.as_str() {
                return Err(Error::NotFound);
            }
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
        self.remember(spec.clone(), report.clone());
        Ok(report)
    }

    async fn drop_parameter(&self, url: &str) -> Result<(), Error> {
        crate::query::drop_index(self, url).await?;
        self.forget(url);
        Ok(())
    }

    async fn reindex(&self, specs: &[ParameterSpec]) -> Result<Vec<IndexReport>, Error> {
        crate::query::reindex(self, specs).await
    }

    fn index_report(&self, url: &str) -> Option<IndexReport> {
        self.reported(url)
    }

    fn health(&self) -> Result<(), Error> {
        match self.pool.is_closed() {
            false => Ok(()),
            true => Err(Error::Internal("the store is not connected".to_owned())),
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
