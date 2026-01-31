use fhir_store::Namespace;
use fhir_core::Error;
use sqlx::{PgPool, Row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub required: bool,
    pub statements: &'static str,
}

const RESOURCE_TABLE: &str = "
create table if not exists resource (
    surrogate_id bigint generated always as identity primary key,
    resource_type text not null,
    resource_id text not null,
    version_number bigint not null,
    spec_version text not null,
    last_updated text not null,
    updated_secs bigint not null,
    updated_nanos integer not null,
    is_deleted boolean not null,
    is_current boolean not null,
    body bytea not null
);
";

const RESOURCE_KEYS: &str = "
create unique index if not exists resource_version_key
    on resource (resource_id, version_number);
create unique index if not exists resource_current_key
    on resource (resource_id) where is_current;
create index if not exists resource_chronology
    on resource (updated_secs, updated_nanos, resource_id, version_number);
create index if not exists resource_live_type_key
    on resource (resource_type) where is_current and not is_deleted;
";

const INDEX_TABLES: &str = "
create table if not exists index_token (
    surrogate_id bigint not null references resource(surrogate_id) on delete cascade,
    param text not null,
    slot text not null,
    ordinal integer not null,
    system text,
    code text not null,
    code_tail text
);
create index if not exists index_token_key on index_token (param, slot, code);
create index if not exists index_token_owner on index_token (surrogate_id);

create table if not exists index_text (
    surrogate_id bigint not null references resource(surrogate_id) on delete cascade,
    param text not null,
    slot text not null,
    ordinal integer not null,
    value text not null,
    folded text not null
);
create index if not exists index_text_key on index_text (param, slot, folded text_pattern_ops);
create index if not exists index_text_owner on index_text (surrogate_id);

create table if not exists index_number (
    surrogate_id bigint not null references resource(surrogate_id) on delete cascade,
    param text not null,
    slot text not null,
    ordinal integer not null,
    value double precision not null
);
create index if not exists index_number_key on index_number (param, slot, value);
create index if not exists index_number_owner on index_number (surrogate_id);

create table if not exists index_date (
    surrogate_id bigint not null references resource(surrogate_id) on delete cascade,
    param text not null,
    slot text not null,
    ordinal integer not null,
    low_secs bigint not null,
    low_nanos integer not null,
    high_secs bigint not null,
    high_nanos integer not null
);
create index if not exists index_date_key on index_date (param, slot, low_secs, high_secs);
create index if not exists index_date_owner on index_date (surrogate_id);

create table if not exists index_quantity (
    surrogate_id bigint not null references resource(surrogate_id) on delete cascade,
    param text not null,
    slot text not null,
    ordinal integer not null,
    value double precision not null,
    system text,
    code text,
    structured boolean not null
);
create index if not exists index_quantity_key on index_quantity (param, slot, value);
create index if not exists index_quantity_owner on index_quantity (surrogate_id);

create table if not exists index_reference (
    surrogate_id bigint not null references resource(surrogate_id) on delete cascade,
    param text not null,
    slot text not null,
    ordinal integer not null,
    ref_full text not null,
    ref_id text not null,
    ref_type text
);
create index if not exists index_reference_key on index_reference (param, slot, ref_id);
create index if not exists index_reference_full on index_reference (param, slot, ref_full);
create index if not exists index_reference_owner on index_reference (surrogate_id);

create table if not exists index_uri (
    surrogate_id bigint not null references resource(surrogate_id) on delete cascade,
    param text not null,
    slot text not null,
    ordinal integer not null,
    value text not null
);
create index if not exists index_uri_key on index_uri (param, slot, value);
create index if not exists index_uri_owner on index_uri (surrogate_id);

create table if not exists index_sort (
    surrogate_id bigint not null references resource(surrogate_id) on delete cascade,
    param text not null,
    sort_text text
);
create unique index if not exists index_sort_key on index_sort (surrogate_id, param);
";

const BODY_ENCODING: &str = "
alter table resource add column if not exists body_encoding text not null default 'plain';
";

const TUNING: &str = "
create index if not exists index_reference_link
    on index_reference (ref_id, param, surrogate_id);
create index if not exists resource_live_key
    on resource (resource_type, resource_id) where is_current and not is_deleted;
";

const JOB_TABLE: &str = "
create table if not exists job (
    job_id text primary key,
    kind text not null,
    state text not null,
    payload text,
    progress_done bigint not null,
    progress_total bigint,
    progress_detail text,
    attempt integer not null,
    attempts integer not null,
    outcome text,
    created_ms bigint not null,
    updated_ms bigint not null,
    available_ms bigint not null,
    lease_ms bigint,
    worker text,
    started_ms bigint,
    cancelled boolean not null
);
create index if not exists job_ready on job (state, available_ms, created_ms, job_id);
";

const OUTPUT_TABLE: &str = "
create table if not exists job_output (
    job_id text not null,
    name text not null,
    kind text not null,
    row_count bigint not null,
    body bytea not null,
    primary key (job_id, name)
);
";

const OUTPUT_TUNING: &str = "
create index if not exists job_output_kind on job_output (job_id, kind);
";

const JOB_TUNING: &str = "
create index if not exists job_lease on job (state, lease_ms);
create index if not exists job_ended on job (state, updated_ms);
create index if not exists job_started on job (kind, started_ms);
";

const JOB_OWNER: &str = "
alter table job add column if not exists owner text;
";

const JOB_OWNER_TUNING: &str = "
create index if not exists job_owner on job (owner, created_ms);
";

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "resource table",
        required: true,
        statements: RESOURCE_TABLE,
    },
    Migration {
        version: 2,
        name: "resource keys",
        required: true,
        statements: RESOURCE_KEYS,
    },
    Migration {
        version: 3,
        name: "index tables",
        required: true,
        statements: INDEX_TABLES,
    },
    Migration {
        version: 4,
        name: "packed bodies",
        required: true,
        statements: BODY_ENCODING,
    },
    Migration {
        version: 5,
        name: "index tuning",
        required: false,
        statements: TUNING,
    },
    Migration {
        version: 6,
        name: "job queue",
        required: true,
        statements: JOB_TABLE,
    },
    Migration {
        version: 7,
        name: "job index tuning",
        required: false,
        statements: JOB_TUNING,
    },
    Migration {
        version: 8,
        name: "job output files",
        required: true,
        statements: OUTPUT_TABLE,
    },
    Migration {
        version: 9,
        name: "output index tuning",
        required: false,
        statements: OUTPUT_TUNING,
    },
    Migration {
        version: 10,
        name: "job owner",
        required: true,
        statements: JOB_OWNER,
    },
    Migration {
        version: 11,
        name: "job owner index",
        required: false,
        statements: JOB_OWNER_TUNING,
    },
];

pub fn latest() -> u32 {
    highest_of(MIGRATIONS)
}

pub fn lowest_compatible() -> u32 {
    lowest_of(MIGRATIONS)
}

fn highest_of(migrations: &[Migration]) -> u32 {
    migrations.iter().map(|step| step.version).max().unwrap_or_default()
}

fn lowest_of(migrations: &[Migration]) -> u32 {
    migrations
        .iter()
        .filter(|step| step.required)
        .map(|step| step.version)
        .max()
        .unwrap_or(1)
}

fn step(version: u32) -> Option<&'static Migration> {
    MIGRATIONS.iter().find(|step| step.version == version)
}

fn state_of(current: Option<u32>, lowest: u32, instance: u32) -> State {
    match current {
        None => State::Behind,
        Some(version) if version < lowest => State::Behind,
        Some(version) if version > instance => State::Ahead,
        Some(_) => State::Compatible,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub version: u32,
    pub name: String,
    pub applied_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Compatible,
    Behind,
    Ahead,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Compatible => "compatible",
            State::Behind => "behind",
            State::Ahead => "ahead",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compatibility {
    pub instance: u32,
    pub lowest: u32,
    pub current: Option<u32>,
    pub state: State,
}

impl Compatibility {
    pub fn is_compatible(&self) -> bool {
        self.state == State::Compatible
    }
}

fn failed(context: &str, error: sqlx::Error) -> Error {
    Error::Internal(format!("{context}: {error}"))
}

pub struct Migrator {
    pool: PgPool,
    namespace: Namespace,
}

impl Migrator {
    pub fn new(pool: PgPool, namespace: Namespace) -> Migrator {
        Migrator { pool, namespace }
    }

    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    async fn prepare(&self) -> Result<(), Error> {
        let name = self.namespace.as_str();
        let statements = format!(
            "create schema if not exists {name};
             create table if not exists {name}.schema_version (
                 version integer primary key,
                 name text not null,
                 applied_at timestamptz not null default now()
             );"
        );
        sqlx::raw_sql(&statements)
            .execute(&self.pool)
            .await
            .map_err(|error| failed("preparing the version table", error))?;
        Ok(())
    }

    async fn has_table(&self) -> Result<bool, Error> {
        let address = format!("{}.schema_version", self.namespace.as_str());
        let row = sqlx::query("select to_regclass($1) is not null as present")
            .bind(&address)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| failed("reading the version table", error))?;
        row.try_get::<bool, _>("present")
            .map_err(|error| failed("reading the version table", error))
    }

    pub async fn version(&self) -> Result<Option<u32>, Error> {
        if !self.has_table().await? {
            return Ok(None);
        }
        let statement = format!(
            "select max(version) as version from {}.schema_version",
            self.namespace.as_str()
        );
        let row = sqlx::query(&statement)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| failed("reading the schema version", error))?;
        let found: Option<i32> = row
            .try_get("version")
            .map_err(|error| failed("reading the schema version", error))?;
        Ok(found.map(|version| version.max(0) as u32))
    }

    pub async fn applied(&self) -> Result<Vec<Applied>, Error> {
        if !self.has_table().await? {
            return Ok(Vec::new());
        }
        let statement = format!(
            "select version, name, applied_at::text as applied_at
             from {}.schema_version order by version",
            self.namespace.as_str()
        );
        let rows = sqlx::query(&statement)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| failed("reading applied migrations", error))?;
        rows.into_iter()
            .map(|row| {
                Ok(Applied {
                    version: row
                        .try_get::<i32, _>("version")
                        .map_err(|error| failed("reading applied migrations", error))?
                        .max(0) as u32,
                    name: row
                        .try_get::<String, _>("name")
                        .map_err(|error| failed("reading applied migrations", error))?,
                    applied_at: row
                        .try_get::<String, _>("applied_at")
                        .map_err(|error| failed("reading applied migrations", error))?,
                })
            })
            .collect()
    }

    async fn run(&self, step: &Migration) -> Result<(), Error> {
        self.prepare().await?;
        let name = self.namespace.as_str();
        let statements = format!("set local search_path to {name};\n{}", step.statements);
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| failed("starting a migration", error))?;
        sqlx::raw_sql(&statements)
            .execute(&mut *transaction)
            .await
            .map_err(|error| failed(&format!("applying schema version {}", step.version), error))?;
        let record = format!(
            "insert into {name}.schema_version (version, name) values ($1, $2)
             on conflict (version) do update set name = excluded.name"
        );
        sqlx::query(&record)
            .bind(step.version as i32)
            .bind(step.name)
            .execute(&mut *transaction)
            .await
            .map_err(|error| failed("recording a migration", error))?;
        transaction
            .commit()
            .await
            .map_err(|error| failed("committing a migration", error))
    }

    pub async fn next(&self) -> Result<Option<u32>, Error> {
        let current = self.version().await?.unwrap_or_default();
        let Some(pending) = MIGRATIONS.iter().find(|step| step.version > current) else {
            return Ok(None);
        };
        self.run(pending).await?;
        Ok(Some(pending.version))
    }

    pub async fn latest(&self) -> Result<usize, Error> {
        let current = self.version().await?.unwrap_or_default();
        let pending: Vec<&Migration> =
            MIGRATIONS.iter().filter(|step| step.version > current).collect();
        for step in &pending {
            self.run(step).await?;
        }
        Ok(pending.len())
    }

    pub async fn force(&self, version: u32) -> Result<u32, Error> {
        let step = step(version).ok_or_else(|| {
            Error::Config(format!("schema version {version} is unknown to this build"))
        })?;
        self.run(step).await?;
        Ok(version)
    }

    pub async fn record(&self, version: u32) -> Result<(), Error> {
        self.prepare().await?;
        let name = step(version).map(|step| step.name).unwrap_or("unknown");
        let statement = format!(
            "insert into {}.schema_version (version, name) values ($1, $2)
             on conflict (version) do update set name = excluded.name",
            self.namespace.as_str()
        );
        sqlx::query(&statement)
            .bind(version as i32)
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|error| failed("recording a version", error))?;
        Ok(())
    }

    pub async fn compatibility(&self) -> Result<Compatibility, Error> {
        let current = self.version().await?;
        let lowest = lowest_compatible();
        let instance = latest();
        Ok(Compatibility {
            instance,
            lowest,
            current,
            state: state_of(current, lowest, instance),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic() -> Vec<Migration> {
        vec![
            Migration { version: 1, name: "one", required: true, statements: "" },
            Migration { version: 2, name: "two", required: true, statements: "" },
            Migration { version: 3, name: "three", required: false, statements: "" },
        ]
    }

    #[test]
    fn versions_run_forward_without_gaps_or_repeats() {
        for (offset, step) in MIGRATIONS.iter().enumerate() {
            assert_eq!(step.version, offset as u32 + 1);
            assert!(!step.name.is_empty());
            assert!(!step.statements.trim().is_empty());
        }
        assert_eq!(latest(), MIGRATIONS.len() as u32);
    }

    #[test]
    fn an_optional_step_widens_the_range_it_sits_above() {
        let steps = synthetic();
        assert_eq!(highest_of(&steps), 3);
        assert_eq!(lowest_of(&steps), 2);
    }

    #[test]
    fn a_build_without_steps_still_reports_a_range() {
        assert_eq!(highest_of(&[]), 0);
        assert_eq!(lowest_of(&[]), 1);
    }

    #[test]
    fn a_schema_outside_the_range_is_not_served() {
        assert_eq!(state_of(None, 2, 3), State::Behind);
        assert_eq!(state_of(Some(1), 2, 3), State::Behind);
        assert_eq!(state_of(Some(2), 2, 3), State::Compatible);
        assert_eq!(state_of(Some(3), 2, 3), State::Compatible);
        assert_eq!(state_of(Some(4), 2, 3), State::Ahead);
    }

    #[test]
    fn every_state_is_named() {
        assert_eq!(State::Compatible.as_str(), "compatible");
        assert_eq!(State::Behind.as_str(), "behind");
        assert_eq!(State::Ahead.as_str(), "ahead");
    }

    #[test]
    fn only_a_known_version_is_a_step() {
        assert_eq!(step(1).map(|step| step.name), Some("resource table"));
        assert!(step(latest() + 1).is_none());
    }
}
