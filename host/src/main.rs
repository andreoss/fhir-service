use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service};
use fhir_core::Error;
use fhir_store::ResourceStore;
use std::sync::Arc;

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
    let store: Arc<dyn ResourceStore> = match config.backend {
        fhir_host::Backend::Memory => Arc::new(MemoryStore::default()),
        other => {
            return Err(Error::Config(format!(
                "backend {other} is not implemented yet; set FHIR_BACKEND=memory"
            )))
        }
    };
    let dependencies = vec![Dependency {
        name: "store",
        check: Arc::new(|| Ok(())),
    }];
    let service = Service::new(store, config.version, dependencies);
    eprintln!("serving {config}");
    service.serve(config.bind).await
}