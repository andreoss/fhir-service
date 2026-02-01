#![allow(dead_code)]

use fhir_adapter_document::{DocumentStore, DEFAULT_URL, ENV_URL};
use fhir_store::Namespace;
use mongodb::Client;

pub const SKIPPED: &str = "skipped: the document engine is not available";

pub fn url() -> String {
    std::env::var(ENV_URL).unwrap_or_else(|_| DEFAULT_URL.to_owned())
}

pub async fn engine() -> Option<Client> {
    let client = match Client::with_uri_str(url()).await {
        Ok(client) => client,
        Err(_) => {
            eprintln!("{SKIPPED}");
            return None;
        }
    };
    let answered = client
        .database("admin")
        .run_command(mongodb::bson::doc! {"hello": 1})
        .await;
    match answered {
        Ok(_) => Some(client),
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
    let trimmed: String = name.chars().take(16).collect();
    Namespace::parse(&format!("t_{trimmed}_{stamp}")).expect("generated namespace is valid")
}

pub async fn drop_namespace(client: &Client, namespace: &Namespace) {
    let _ = client.database(namespace.as_str()).drop().await;
}

pub async fn fresh(name: &str) -> Option<(DocumentStore, Client, Namespace)> {
    let client = engine().await?;
    let namespace = namespace(name);
    let store = DocumentStore::new(client.clone(), namespace.clone()).with_clock(std::sync::Arc::new(
        || fhir_core::FhirInstant::parse("2026-09-06T04:00:00.000Z").expect("fixed instant"),
    ));
    store.initialise().await.expect("the namespace prepares");
    Some((store, client, namespace))
}
