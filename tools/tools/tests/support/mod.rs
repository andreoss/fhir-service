#![allow(dead_code)]

use fhir_adapter_relational::migration::Migrator;
use fhir_adapter_relational::{Namespace, DEFAULT_URL, ENV_URL};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::path::PathBuf;
use std::process::Command;

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

pub fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default()
}

pub fn namespace(name: &str) -> Namespace {
    Namespace::parse(&format!("t_{name}_{}", stamp())).expect("generated namespace is valid")
}

pub async fn prepared(pool: &PgPool, namespace: &Namespace) {
    Migrator::new(pool.clone(), namespace.clone())
        .latest()
        .await
        .expect("the schema applies");
}

pub async fn drop_namespace(pool: &PgPool, namespace: &Namespace) {
    let statement = format!("drop schema if exists {} cascade", namespace.as_str());
    let _ = sqlx::raw_sql(&statement).execute(pool).await;
}

pub fn scratch(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scratch")
        .join(format!("{name}-{}", stamp()));
    std::fs::create_dir_all(&root).expect("the scratch directory is writable");
    root
}

pub fn service_binary() -> Option<PathBuf> {
    let mut here = std::env::current_exe().ok()?;
    here.pop();
    here.pop();
    let binary = here.join("fhir-host");
    match binary.exists() {
        true => Some(binary),
        false => {
            eprintln!("skipped: the service binary is not built");
            None
        }
    }
}

pub fn tool(binary: &str, args: &[&str], namespace: &str) -> (i32, String) {
    tool_with(binary, args, namespace, &[])
}

pub fn tool_with(
    binary: &str,
    args: &[&str],
    namespace: &str,
    extra: &[(&str, &str)],
) -> (i32, String) {
    let mut command = Command::new(binary);
    command
        .args(args)
        .env("FHIR_BACKEND", "relational")
        .env("FHIR_DATABASE_URL", url())
        .env(fhir_adapter_relational::ENV_URL, url())
        .env(fhir_adapter_relational::ENV_NAMESPACE, namespace)
        .env_remove("FHIR_METRICS_CREDENTIAL");
    for (name, value) in extra {
        command.env(name, value);
    }
    let output = command.output().expect("the tool runs");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code().unwrap_or(-1), text)
}
