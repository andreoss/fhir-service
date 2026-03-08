use fhir_core::Error;
use fhir_jobs::{ImportJob, Orchestrator};
use fhir_store::JobKind;
use fhir_tools::{perform, Session};
use std::sync::Arc;

const USAGE: &str = "usage: load <supply.ndjson>";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(supply) = args.first() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    match run(supply).await {
        Ok(report) => println!("{report}"),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

async fn run(supply: &str) -> Result<String, Error> {
    let rows = std::fs::read_to_string(supply)
        .map_err(|error| Error::Config(format!("the supply cannot be read: {error}")))?;
    let config = fhir_host::Config::from_env()?;
    let session = Session::open(&config).await?;
    let orchestrator =
        Orchestrator::new().with(Arc::new(ImportJob::new(session.store(), session.version())));
    perform(&orchestrator, JobKind::Import, &rows).await
}
