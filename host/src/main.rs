use fhir_api::{Dependency, Service};
use fhir_core::Error;
use fhir_store::ResourceStore;
use std::sync::Arc;

#[cfg(feature = "backend-memory")]
use fhir_adapter_memory::MemoryStore;

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
    let store = build_store(&config)?;
    let store_dependency = {
        let store = Arc::clone(&store);
        Arc::new(move || store.health().map_err(|error| error.to_string()))
    };
    let dependencies = vec![Dependency {
        name: "store",
        check: store_dependency,
    }];
    let service = Service::started(store, config.version, dependencies).await?;
    eprintln!("serving {config}");
    let bound = service.bind(config.bind).await?;
    println!("listening on {}", bound.local_addr()?);
    bound.serve().await
}

fn build_store(config: &fhir_host::Config) -> Result<Arc<dyn ResourceStore>, Error> {
    match config.backend {
        #[cfg(feature = "backend-memory")]
        fhir_host::Backend::Memory => Ok(Arc::new(MemoryStore::default())),
        #[cfg(not(feature = "backend-memory"))]
        fhir_host::Backend::Memory => Err(Error::Config(
            "memory backend is not enabled in this build; rebuild with --features backend-memory".to_owned(),
        )),
        #[cfg(feature = "backend-relational")]
        fhir_host::Backend::Relational => Err(Error::Config(
            "relational backend is not implemented yet".to_owned(),
        )),
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