use fhir_api::{Dependency, Service};
use fhir_core::Error;
use fhir_store::ResourceStore;
use std::sync::Arc;

const LEASE_MILLIS: i64 = 30_000;
const POLL_MILLIS: u64 = 50;

#[cfg(feature = "backend-memory")]
use fhir_adapter_memory::MemoryStore;

#[cfg(feature = "backend-relational")]
use fhir_adapter_relational::{Namespace, RelationalStore};

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
    let store = build_store(&config).await?;
    let store_dependency = {
        let store = Arc::clone(&store);
        Arc::new(move || store.health().map_err(|error| error.to_string()))
    };
    let dependencies = vec![Dependency {
        name: "store",
        check: store_dependency,
    }];
    let jobs = build_jobs(&config);
    let mut service = Service::started(Arc::clone(&store), config.version, dependencies).await?;
    if let Some(jobs) = &jobs {
        service = service.with_jobs(Arc::clone(jobs));
        spawn_worker(Arc::clone(jobs), store, config.version);
    }
    eprintln!("serving {config}");
    let bound = service.bind(config.bind).await?;
    println!("listening on {}", bound.local_addr()?);
    bound.serve().await
}

async fn build_store(config: &fhir_host::Config) -> Result<Arc<dyn ResourceStore>, Error> {
    match config.backend {
        #[cfg(feature = "backend-memory")]
        fhir_host::Backend::Memory => Ok(Arc::new(MemoryStore::default())),
        #[cfg(not(feature = "backend-memory"))]
        fhir_host::Backend::Memory => Err(Error::Config(
            "memory backend is not enabled in this build; rebuild with --features backend-memory".to_owned(),
        )),
        #[cfg(feature = "backend-relational")]
        fhir_host::Backend::Relational => {
            let namespace = match std::env::var(fhir_adapter_relational::ENV_NAMESPACE) {
                Ok(name) => Namespace::parse(&name)?,
                Err(_) => Namespace::default(),
            };
            let store = RelationalStore::connect(&config.database_url, namespace).await?;
            store.migrate().await?;
            Ok(Arc::new(store))
        }
        #[cfg(not(feature = "backend-relational"))]
        fhir_host::Backend::Relational => Err(Error::Config(
            "relational backend is not enabled in this build; rebuild with --features backend-relational or set FHIR_BACKEND=memory".to_owned(),
        )),
        #[cfg(feature = "backend-document")]
        fhir_host::Backend::Document => Err(Error::Config(
            "document backend is not implemented yet".to_owned(),
        )),
        #[cfg(not(feature = "backend-document"))]
        fhir_host::Backend::Document => Err(Error::Config(
            "document backend is not enabled in this build; rebuild with --features backend-document or set FHIR_BACKEND=memory".to_owned(),
        )),
    }
}

fn build_jobs(config: &fhir_host::Config) -> Option<Arc<dyn fhir_store::JobStore>> {
    match config.backend {
        #[cfg(feature = "backend-memory")]
        fhir_host::Backend::Memory => Some(Arc::new(fhir_adapter_memory::MemoryJobStore::default())),
        #[cfg(not(feature = "backend-memory"))]
        fhir_host::Backend::Memory => None,
        fhir_host::Backend::Relational => None,
        fhir_host::Backend::Document => None,
    }
}

fn spawn_worker(
    jobs: Arc<dyn fhir_store::JobStore>,
    store: Arc<dyn ResourceStore>,
    version: fhir_core::FhirVersion,
) {
    let orchestrator = Arc::new(
        fhir_jobs::Orchestrator::new()
            .with(Arc::new(fhir_jobs::ExportJob::new(Arc::clone(&store))))
            .with(Arc::new(fhir_jobs::ImportJob::new(Arc::clone(&store), version)))
            .with(Arc::new(fhir_jobs::BulkDeleteJob::new(Arc::clone(&store))))
            .with(Arc::new(fhir_jobs::BulkUpdateJob::new(Arc::clone(&store))))
            .with(Arc::new(fhir_jobs::ReindexJob::new(store))),
    );
    let worker = fhir_jobs::Worker::new(jobs, orchestrator, "host", LEASE_MILLIS);
    tokio::spawn(async move {
        loop {
            let _ = worker.poll().await;
            tokio::time::sleep(std::time::Duration::from_millis(POLL_MILLIS)).await;
        }
    });
}
