use fhir_api::{DiscoveredKeys, Dependency, HeldKeys, Keys, Service, StoredTrail};
use fhir_core::Error;
use fhir_store::ResourceStore;
use std::sync::Arc;

const LEASE_MILLIS: i64 = 30_000;
const POLL_MILLIS: u64 = 50;
const SWEEP_MILLIS: u64 = 1_000;
const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[tokio::main]
async fn main() {
    match run().await {
        Ok(()) => {}
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<(), Error> {
    let config = fhir_host::Config::from_env()?;
    let (store, jobs, outputs) = fhir_host::stores::open(&config).await?;
    let store_dependency = {
        let store = Arc::clone(&store);
        Arc::new(move || store.health().map_err(|error| error.to_string()))
    };
    let dependencies = vec![Dependency {
        name: "store",
        check: store_dependency,
    }];
    let mut service = Service::started(Arc::clone(&store), config.version, dependencies)
        .await?
        .with_entries(config.entries());
    if let Some(authorization) = config.authorization.clone() {
        service = service.with_authorization(authorization);
        let keys: Arc<dyn Keys> = match config.keys.clone() {
            Some(set) => Arc::new(HeldKeys::new(set)),
            None => Arc::new(DiscoveredKeys::new(DISCOVERY_TIMEOUT)),
        };
        service = service.enforcing(keys)?.recording(Arc::new(StoredTrail::new(
            Arc::clone(&store),
            config.version,
        )));
    }
    if let Some(outputs) = &outputs {
        service = service.with_outputs(Arc::clone(outputs));
    }
    service = service.scraped(config.scrape.clone());
    if let Some(jobs) = &jobs {
        service = service.with_jobs(Arc::clone(jobs));
        spawn_worker(
            Arc::clone(jobs),
            store,
            outputs,
            config.version,
            service.telemetry(),
        );
        spawn_watchdog(Arc::clone(jobs));
    }
    eprintln!("serving {config}");
    let bound = service.bind(config.bind).await?;
    println!("listening on {}", bound.local_addr()?);
    bound.serve().await
}

fn spawn_worker(
    jobs: Arc<dyn fhir_store::JobStore>,
    store: Arc<dyn ResourceStore>,
    outputs: Option<Arc<dyn fhir_store::BulkStore>>,
    version: fhir_core::FhirVersion,
    telemetry: Arc<fhir_telemetry::Telemetry>,
) {
    let mut registry = fhir_jobs::Orchestrator::new()
        .with(Arc::new(fhir_jobs::ImportJob::new(Arc::clone(&store), version)));
    if let Some(sink) = outputs {
        registry = registry
            .with(Arc::new(fhir_jobs::ReindexJob::new(
                Arc::clone(&store),
                Arc::clone(&sink),
            )))
            .with(Arc::new(fhir_jobs::BulkDeleteJob::new(
                Arc::clone(&store),
                Arc::clone(&sink),
            )))
            .with(Arc::new(fhir_jobs::BulkUpdateJob::new(
                Arc::clone(&store),
                Arc::clone(&sink),
            )))
            .with(Arc::new(fhir_jobs::ExportJob::new(store, sink)));
    }
    let orchestrator = Arc::new(registry.reporting(telemetry));
    let worker = fhir_jobs::Worker::new(jobs, orchestrator, "host", LEASE_MILLIS);
    tokio::spawn(async move {
        loop {
            let _ = worker.poll().await;
            tokio::time::sleep(std::time::Duration::from_millis(POLL_MILLIS)).await;
        }
    });
}

fn spawn_watchdog(jobs: Arc<dyn fhir_store::JobStore>) {
    let watchdog = fhir_jobs::Watchdog::new(jobs);
    tokio::spawn(async move {
        loop {
            let _ = watchdog.sweep().await;
            tokio::time::sleep(std::time::Duration::from_millis(SWEEP_MILLIS)).await;
        }
    });
}
