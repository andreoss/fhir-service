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
    let mut dependencies = vec![Dependency::of_store("store", Arc::clone(&store))];
    if let Some(jobs) = &jobs {
        dependencies.push(Dependency::of_queue("queue", Arc::clone(jobs)));
    }
    if let Some(outputs) = &outputs {
        dependencies.push(Dependency::of_outputs("outputs", Arc::clone(outputs)));
    }
    let mut service = Service::started(Arc::clone(&store), config.version, dependencies)
        .await?
        .with_entries(config.entries());
    if let Some(authorization) = config.authorization.clone() {
        let issuer = authorization.issuer.clone();
        service = service.with_authorization(authorization);
        let keys: Arc<dyn Keys> = match config.keys.clone() {
            Some(set) => Arc::new(HeldKeys::new(set)),
            None => Arc::new(
                DiscoveredKeys::new(DISCOVERY_TIMEOUT)
                    .pinning(&issuer, config.pins.clone()),
            ),
        };
        let trail = StoredTrail::resumed(
            Arc::clone(&store),
            config.version,
            fhir_api::configured_seal(),
        )
        .await?;
        service = service.enforcing(keys)?.recording(Arc::new(trail));
    }
    if config.terminology_dir.is_some() {
        let catalogue = Arc::new(fhir_host::terminology::loaded(&config)?);
        service = service.with_terminology(Arc::new(
            fhir_api::StoredTerminology::new(Arc::clone(&store), config.version)
                .with_catalogue(catalogue),
        ));
    }
    if let Some(outputs) = &outputs {
        service = service.with_outputs(Arc::clone(outputs));
    }
    service = service.scraped(config.scrape.clone());
    let mut worker = None;
    if let Some(jobs) = &jobs {
        service = service.with_jobs(Arc::clone(jobs));
        worker = Some(spawn_worker(
            Arc::clone(jobs),
            store,
            outputs,
            config.version,
            service.telemetry(),
        ));
        spawn_watchdog(Arc::clone(jobs));
    }
    eprintln!("serving {config}");
    let bound = service.bind(config.bind).await?;
    println!("listening on {}", bound.local_addr()?);
    let served = bound.serve_until(asked_to_stop()).await;
    if let Some(worker) = worker {
        match worker.stopping().await {
            Ok(handed) => eprintln!("handed back {handed}"),
            Err(error) => eprintln!("handing back: {error}"),
        }
    }
    served
}

async fn asked_to_stop() {
    let interrupted = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminated = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut asked) => {
                asked.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminated = std::future::pending::<()>();
    tokio::select! {
        _ = interrupted => {}
        _ = terminated => {}
    }
}

fn spawn_worker(
    jobs: Arc<dyn fhir_store::JobStore>,
    store: Arc<dyn ResourceStore>,
    outputs: Option<Arc<dyn fhir_store::BulkStore>>,
    version: fhir_core::FhirVersion,
    telemetry: Arc<fhir_telemetry::Telemetry>,
) -> Arc<fhir_jobs::Worker> {
    let importing = fhir_jobs::ImportJob::new(Arc::clone(&store), version);
    let importing = match &outputs {
        Some(sink) => importing.reporting(Arc::clone(sink)),
        None => importing,
    };
    let mut registry = fhir_jobs::Orchestrator::new().with(Arc::new(importing));
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
    let worker = Arc::new(fhir_jobs::Worker::per_instance(
        jobs,
        orchestrator,
        LEASE_MILLIS,
    ));
    let polling = Arc::clone(&worker);
    tokio::spawn(async move {
        loop {
            let _ = polling.poll().await;
            tokio::time::sleep(std::time::Duration::from_millis(POLL_MILLIS)).await;
        }
    });
    worker
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
