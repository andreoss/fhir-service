use fhir_adapter_memory::MemoryJobStore;
use fhir_store::StepTicker;

fn store(ticker: &StepTicker) -> MemoryJobStore {
    MemoryJobStore::new(ticker.ticker())
}

#[tokio::test]
async fn a_submitted_job_reads_back() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::submission(&store(&ticker)).await;
}

#[tokio::test]
async fn a_claim_holds_one_job_under_a_lease() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::claiming(&store(&ticker)).await;
}

#[tokio::test]
async fn a_heartbeat_extends_the_lease() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::heartbeats(&store(&ticker)).await;
}

#[tokio::test]
async fn a_finished_attempt_keeps_its_result() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::completion(&store(&ticker)).await;
}

#[tokio::test]
async fn a_listing_filters_by_kind_and_state() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::listing(&store(&ticker)).await;
}

#[tokio::test]
async fn a_stop_reaches_a_queued_and_a_running_job() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::cancellation(&store(&ticker)).await;
}

#[tokio::test]
async fn a_failed_attempt_waits_and_runs_again() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::retries(&store(&ticker), &ticker).await;
}

#[tokio::test]
async fn a_job_a_stopped_worker_held_is_claimed_again() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::recovery(&store(&ticker), &ticker).await;
}

#[tokio::test]
async fn a_job_with_no_attempt_left_fails_on_reclaim() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::exhaustion(&store(&ticker), &ticker).await;
}

#[tokio::test]
async fn a_cancelling_job_whose_worker_stopped_ends_cancelled() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::stopped_while_cancelling(&store(&ticker), &ticker).await;
}

#[tokio::test]
async fn work_that_cannot_succeed_fails_at_once() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::rejection(&store(&ticker)).await;
}
