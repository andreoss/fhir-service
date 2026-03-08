use fhir_api::{Dependency, DiscoveredKeys, HeldKeys, Keys, Service, StoredTrail};
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
    let mut service = assembled(
        &config,
        config.version,
        Arc::clone(&store),
        outputs.clone(),
        dependencies,
    )
    .await?;
    let mut worker = None;
    if let Some(jobs) = &jobs {
        service = service.with_jobs(Arc::clone(jobs));
        worker = Some(spawn_worker(
            Arc::clone(jobs),
            Arc::clone(&store),
            outputs.clone(),
            config.version,
            service.telemetry(),
            service.interactions(),
        ));
        spawn_watchdog(Arc::clone(jobs));
        spawn_retention(Arc::clone(jobs), config.retention.clone());
    }
    
    
    if let Some(directory) = &config.preload {
        let held = fhir_host::preload::read(directory, config.version)?;
        let loaded = fhir_host::preload::load(store.as_ref(), config.version, &held).await?;
        eprintln!(
            "preloaded {} resources: {} written, {} unchanged",
            loaded.files, loaded.written, loaded.unchanged
        );
    }
    eprintln!("serving {config}");
    let bound = match config.versions.as_slice() {
        [_] => service.bind(config.bind).await?,
        held => {
            let mut services = vec![(config.version, service)];
            for version in held.iter().skip(1) {
                let store = fhir_host::stores::resources_for(&config, *version).await?;
                let dependencies = vec![Dependency::of_store("store", Arc::clone(&store))];
                services.push((
                    *version,
                    assembled(&config, *version, store, outputs.clone(), dependencies).await?,
                ));
            }
            let endpoints = fhir_api::Endpoints::new(config.version, services)?;
            eprintln!(
                "serving {} releases, {} by default",
                endpoints.served().len(),
                endpoints.default_version()
            );
            fhir_api::Bound::holding(config.bind, endpoints.router()).await?
        }
    };
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



async fn assembled(
    config: &fhir_host::Config,
    version: fhir_core::FhirVersion,
    store: Arc<dyn ResourceStore>,
    outputs: Option<Arc<dyn fhir_store::BulkStore>>,
    dependencies: Vec<Dependency>,
) -> Result<Service, Error> {
    let mut service = Service::started(Arc::clone(&store), version, dependencies)
        .await?
        .with_entries(config.entries())
        .with_versioning(config.versioning.clone())
        .with_profile_validation(config.profiles)
        .with_roles(config.roles.clone())
        .with_throttle(config.throttle.clone())
        .with_capabilities(config.capabilities)
        .accepting_profiles(config.allowed_profiles.clone())
        .with_administration(config.administration.clone())
        .with_tenancy(config.tenancy.clone())
        .with_policies(config.policies)
        .with_security_headers(config.security_headers.clone())
        .resettable(config.reset.clone())
        .skipping_unchanged(config.unchanged.clone())
        .answering(config.default_format)
        .paging(config.paging.clone())
        .bounded_by(config.limits)
        .behind_proxy(config.forwarding)
        .normalising(config.references.clone())
        .serving(config.restricted.clone());
    if let Some(alert) = config.alert.clone() {
        service = service.alarming(Arc::new(alert));
    }
    if let Some(collector) = config.collector.clone() {
        service = service.tracing(Arc::new(collector.clone()));
        fhir_host::otlp::spawn_metrics(collector, service.telemetry());
    }
    if let Some(authorization) = config.authorization.clone() {
        let issuer = authorization.issuer.clone();
        service = service.with_authorization(authorization);
        let keys: Arc<dyn Keys> = match config.keys.clone() {
            Some(set) => Arc::new(HeldKeys::new(set)),
            None => Arc::new(
                DiscoveredKeys::new(DISCOVERY_TIMEOUT).pinning(&issuer, config.pins.clone()),
            ),
        };
        let trail =
            StoredTrail::resumed(Arc::clone(&store), version, fhir_api::configured_seal()).await?;
        service = service.enforcing(keys)?.recording(Arc::new(trail));
    }
    if config.terminology_dir.is_some() {
        let catalogue = Arc::new(fhir_host::terminology::loaded(config)?);
        service = service.with_terminology(Arc::new(
            fhir_api::StoredTerminology::new(Arc::clone(&store), version).with_catalogue(catalogue),
        ));
    }
    if let Some(outputs) = &outputs {
        service = service.with_outputs(Arc::clone(outputs));
    }
    Ok(service.scraped(config.scrape.clone()))
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
    interactions: Arc<dyn fhir_store::Interactions>,
) -> Arc<fhir_jobs::Worker> {
    let importing = fhir_jobs::ImportJob::new(Arc::clone(&store), version);
    let importing = match &outputs {
        Some(sink) => importing.reporting(Arc::clone(sink)),
        None => importing,
    };
    let mut registry = fhir_jobs::Orchestrator::new()
        .with(Arc::new(importing))
        .with(Arc::new(fhir_jobs::InteractionJob::new(interactions)));
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




fn spawn_retention(jobs: Arc<dyn fhir_store::JobStore>, retention: fhir_jobs::Retention) {
    if retention.is_empty() {
        return;
    }
    let worker = fhir_jobs::RetentionWorker::new(jobs, retention);
    tokio::spawn(async move {
        loop {
            match worker.sweep().await {
                Ok(swept) if swept.is_empty() => {}
                Ok(swept) => {
                    for (resource_type, cutoff) in swept.submitted {
                        eprintln!("retention: {resource_type} written before {cutoff} submitted");
                    }
                }
                Err(error) => eprintln!("retention: {error}"),
            }
            tokio::time::sleep(std::time::Duration::from_millis(SWEEP_MILLIS)).await;
        }
    });
}
