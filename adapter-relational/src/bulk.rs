use crate::namespace::Namespace;
use crate::store::faulted;
use async_trait::async_trait;
use fhir_core::Error;
use fhir_store::{BulkStore, JobId, Output};
use sqlx::{PgPool, Row};

pub struct RelationalBulkStore {
    pool: PgPool,
    namespace: Namespace,
}

impl RelationalBulkStore {
    pub fn new(pool: PgPool, namespace: Namespace) -> RelationalBulkStore {
        RelationalBulkStore { pool, namespace }
    }

    fn table(&self) -> String {
        format!("{}.job_output", self.namespace.as_str())
    }
}

#[async_trait]
impl BulkStore for RelationalBulkStore {
    async fn write(&self, job: &JobId, output: &Output, body: &[u8]) -> Result<(), Error> {
        let statement = format!(
            "insert into {} (job_id, name, kind, row_count, body) values ($1, $2, $3, $4, $5) \
             on conflict (job_id, name) do update set kind = excluded.kind, \
             row_count = excluded.row_count, body = excluded.body",
            self.table()
        );
        sqlx::query(&statement)
            .bind(job.as_str())
            .bind(&output.name)
            .bind(&output.kind)
            .bind(output.count as i64)
            .bind(body)
            .execute(&self.pool)
            .await
            .map_err(|error| faulted("writing an output file", error))?;
        Ok(())
    }

    async fn read(&self, job: &JobId, name: &str) -> Result<Vec<u8>, Error> {
        let statement = format!(
            "select body from {} where job_id = $1 and name = $2",
            self.table()
        );
        let row = sqlx::query(&statement)
            .bind(job.as_str())
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| faulted("reading an output file", error))?;
        let row = row.ok_or(Error::NotFound)?;
        row.try_get::<Vec<u8>, _>("body")
            .map_err(|error| faulted("reading an output column", error))
    }

    async fn list(&self, job: &JobId) -> Result<Vec<Output>, Error> {
        let statement = format!(
            "select name, kind, row_count, length(body) as size from {} \
             where job_id = $1 order by name",
            self.table()
        );
        let rows = sqlx::query(&statement)
            .bind(job.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| faulted("listing output files", error))?;
        rows.iter()
            .map(|row| {
                let count = row
                    .try_get::<i64, _>("row_count")
                    .map_err(|error| faulted("reading an output column", error))?;
                let size = row
                    .try_get::<i32, _>("size")
                    .map_err(|error| faulted("reading an output column", error))?;
                Ok(Output {
                    name: row
                        .try_get::<String, _>("name")
                        .map_err(|error| faulted("reading an output column", error))?,
                    kind: row
                        .try_get::<String, _>("kind")
                        .map_err(|error| faulted("reading an output column", error))?,
                    count: count.max(0) as u64,
                    size: size.max(0) as usize,
                })
            })
            .collect()
    }

    async fn purge(&self, job: &JobId) -> Result<usize, Error> {
        let statement = format!("delete from {} where job_id = $1", self.table());
        let done = sqlx::query(&statement)
            .bind(job.as_str())
            .execute(&self.pool)
            .await
            .map_err(|error| faulted("releasing output files", error))?;
        Ok(done.rows_affected() as usize)
    }

    fn health(&self) -> Result<(), Error> {
        match self.pool.is_closed() {
            true => Err(Error::Internal("the output sink is closed".to_owned())),
            false => Ok(()),
        }
    }
}
