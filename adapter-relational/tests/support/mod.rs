use fhir_adapter_relational::{Namespace, DEFAULT_URL, ENV_URL};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub const SKIPPED: &str = "skipped: the relational engine is not available";

pub fn url() -> String {
    std::env::var(ENV_URL).unwrap_or_else(|_| DEFAULT_URL.to_owned())
}

pub async fn engine() -> Option<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&url())
        .await;
    match pool {
        Ok(pool) => Some(pool),
        Err(_) => {
            eprintln!("{SKIPPED}");
            None
        }
    }
}

pub fn namespace(name: &str) -> Namespace {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    Namespace::parse(&format!("t_{name}_{stamp}")).expect("generated namespace is valid")
}

pub async fn drop_namespace(pool: &PgPool, namespace: &Namespace) {
    let statement = format!("drop schema if exists {} cascade", namespace.as_str());
    let _ = sqlx::raw_sql(&statement).execute(pool).await;
}
