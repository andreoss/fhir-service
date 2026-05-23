use fhir_adapter_document::DocumentStore;
use fhir_adapter_memory::MemoryStore;
use fhir_adapter_relational::RelationalStore;
use fhir_store::{Namespace, ResourceStore};
use std::sync::Arc;

const FIXED: &str = "2026-09-06T04:00:00.000Z";

fn clock() -> Arc<dyn Fn() -> fhir_core::FhirInstant + Send + Sync> {
    Arc::new(|| fhir_core::FhirInstant::parse(FIXED).expect("a fixed instant"))
}

fn stamped(name: &str) -> Namespace {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    let trimmed: String = name.chars().take(12).collect();
    Namespace::parse(&format!("t_{trimmed}_{stamp}")).expect("a generated namespace is valid")
}

async fn relational_store(name: &str) -> Option<(Arc<dyn ResourceStore>, sqlx::PgPool, Namespace)> {
    let url = std::env::var(fhir_adapter_relational::ENV_URL)
        .unwrap_or_else(|_| fhir_adapter_relational::DEFAULT_URL.to_owned());
    let pool = match sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&url)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            return fhir_store_contract::engine::absent("relational", &url, &error.to_string())
        }
    };
    let namespace = stamped(name);
    let store = RelationalStore::new(pool.clone(), namespace.clone()).with_clock(clock());
    store.migrate().await.expect("the schema applies");
    Some((Arc::new(store), pool, namespace))
}

async fn document_store(
    name: &str,
) -> Option<(Arc<dyn ResourceStore>, mongodb::Client, Namespace)> {
    let url = std::env::var(fhir_adapter_document::ENV_URL)
        .unwrap_or_else(|_| fhir_adapter_document::DEFAULT_URL.to_owned());
    let refused = |error: String| fhir_store_contract::engine::absent("document", &url, &error);
    let mut options = match mongodb::options::ClientOptions::parse(&url).await {
        Ok(options) => options,
        Err(error) => return refused(error.to_string()),
    };
    options.server_selection_timeout = Some(std::time::Duration::from_secs(3));
    let client = match mongodb::Client::with_options(options) {
        Ok(client) => client,
        Err(error) => return refused(error.to_string()),
    };
    if let Err(error) = client
        .database("admin")
        .run_command(mongodb::bson::doc! {"hello": 1})
        .await
    {
        return refused(error.to_string());
    }
    let namespace = stamped(name);
    let store = DocumentStore::new(client.clone(), namespace.clone()).with_clock(clock());
    store.initialise().await.expect("the namespace prepares");
    Some((Arc::new(store), client, namespace))
}

macro_rules! backend_group {
    ($name:ident, $group:path) => {
        mod $name {
            use super::*;

            #[tokio::test]
            async fn memory() {
                let store = MemoryStore::with_clock(clock());
                $group(&store).await;
            }

            #[tokio::test]
            async fn relational() {
                let Some((store, pool, namespace)) = relational_store(stringify!($name)).await
                else {
                    return;
                };
                $group(store.as_ref()).await;
                let statement = format!("drop schema if exists {} cascade", namespace.as_str());
                let _ = sqlx::raw_sql(&statement).execute(&pool).await;
            }

            #[tokio::test]
            async fn document() {
                let Some((store, client, namespace)) = document_store(stringify!($name)).await
                else {
                    return;
                };
                $group(store.as_ref()).await;
                let _ = client.database(namespace.as_str()).drop().await;
            }
        }
    };
}

backend_group!(lifecycle, fhir_store_contract::lifecycle);
backend_group!(versioning, fhir_store_contract::versioning);
backend_group!(removal, fhir_store_contract::removal);
backend_group!(record, fhir_store_contract::record);
backend_group!(readiness, fhir_store_contract::readiness);

#[test]
fn an_engine_that_cannot_be_reached_fails_the_run() {
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(|| {
        fhir_store_contract::engine::absent::<()>(
            "relational",
            "postgres://127.0.0.1:1/none",
            "connection refused",
        )
    });
    std::panic::set_hook(quiet);
    let told = outcome.expect_err("an absent engine fails the run");
    let told = told
        .downcast_ref::<String>()
        .expect("the refusal carries its message");
    assert!(told.contains("postgres://127.0.0.1:1/none"), "{told}");
    assert!(
        told.contains(fhir_store_contract::engine::ENV_SKIP),
        "{told}"
    );
}
