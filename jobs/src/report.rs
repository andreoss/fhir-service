use fhir_core::{Error, IssueCode, OperationOutcome};
use fhir_store::{BulkStore, JobId, Output};

pub const OUTCOME: &str = "OperationOutcome";

pub fn itemised(failures: &[String]) -> Vec<u8> {
    let mut body = Vec::new();
    for failure in failures {
        let outcome = OperationOutcome::error(IssueCode::Processing, failure.as_str());
        body.extend_from_slice(&outcome.to_fhir_json());
        body.push(b'\n');
    }
    body
}

pub async fn record_failures(
    sink: &dyn BulkStore,
    job: &JobId,
    name: &str,
    failures: &[String],
) -> Result<(), Error> {
    if failures.is_empty() {
        return Ok(());
    }
    let output = Output::new(name, OUTCOME, failures.len() as u64);
    sink.write(job, &output, &itemised(failures)).await
}

pub fn failure_file(container: &str, label: &str) -> String {
    match container.is_empty() {
        true => format!("{label}-failures.ndjson"),
        false => format!("{container}/{label}-failures.ndjson"),
    }
}
