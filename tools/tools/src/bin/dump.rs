use fhir_core::Error;
use fhir_jobs::{ExportJob, Orchestrator};
use fhir_store::JobKind;
use fhir_tools::{perform, DirectoryOutputs, Session};
use serde_json::json;
use std::sync::Arc;

const USAGE: &str = "usage: dump <directory> [resource type ...]";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(directory) = args.first() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    match run(directory, &args[1..]).await {
        Ok(report) => println!("{report}"),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

async fn run(directory: &str, types: &[String]) -> Result<String, Error> {
    let config = fhir_host::Config::from_env()?;
    let session = Session::open(&config).await?;
    let sink = Arc::new(DirectoryOutputs::open(directory)?);
    let orchestrator = Orchestrator::new().with(Arc::new(ExportJob::new(session.store(), sink)));
    let payload = json!({ "_type": types }).to_string();
    perform(&orchestrator, JobKind::Export, &payload).await
}
