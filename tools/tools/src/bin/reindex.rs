use fhir_core::Error;
use fhir_jobs::{Orchestrator, ReindexJob};
use fhir_store::JobKind;
use fhir_tools::{perform, DirectoryOutputs, Session};
use serde_json::json;
use std::sync::Arc;

const USAGE: &str = "usage: reindex <resource type ...> [--reports <directory>]";
const REPORTS: &str = "scratch/reindex";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    match run(&args).await {
        Ok(report) => println!("{report}"),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

fn split(args: &[String]) -> (Vec<String>, String) {
    match args.iter().position(|arg| arg == "--reports") {
        Some(at) => (
            args[..at].to_vec(),
            args.get(at + 1)
                .cloned()
                .unwrap_or_else(|| REPORTS.to_owned()),
        ),
        None => (args.to_vec(), REPORTS.to_owned()),
    }
}

async fn run(args: &[String]) -> Result<String, Error> {
    let (types, reports) = split(args);
    let config = fhir_host::Config::from_env()?;
    let session = Session::open(&config).await?;
    let sink = Arc::new(DirectoryOutputs::open(reports)?);
    let orchestrator = Orchestrator::new().with(Arc::new(ReindexJob::new(session.store(), sink)));
    let payload = json!({ "_type": types }).to_string();
    perform(&orchestrator, JobKind::Reindex, &payload).await
}
