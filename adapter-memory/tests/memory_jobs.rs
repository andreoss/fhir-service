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

#[tokio::test]
async fn a_running_job_learns_of_a_stop_at_its_next_heartbeat() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::cancel_signal(&store(&ticker)).await;
}

#[tokio::test]
async fn an_ended_job_releases_its_description() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::defragmentation(&store(&ticker)).await;
}

#[tokio::test]
async fn a_kind_runs_no_more_jobs_at_once_than_its_limit() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::concurrency(&store(&ticker)).await;
}

#[tokio::test]
async fn a_kind_starts_no_more_often_than_its_throttle() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::throttling(&store(&ticker), &ticker).await;
}

#[tokio::test]
async fn one_kind_never_holds_another_back() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::limits_are_per_kind(&store(&ticker)).await;
}

#[tokio::test]
async fn an_ended_job_is_kept_for_its_retention_and_then_removed() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::retention(&store(&ticker), &ticker).await;
}

#[tokio::test]
async fn the_sink_satisfies_the_output_contract() {
    fhir_store_contract::bulk::outputs(&fhir_adapter_memory::MemoryBulkStore::new()).await;
}

#[tokio::test]
async fn a_resumed_job_keeps_the_identifier_it_started_with() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::correlation(&store(&ticker), &ticker).await;
}

#[tokio::test]
async fn a_stopping_worker_hands_its_work_back_at_once() {
    let ticker = StepTicker::starting_at(1_000);
    fhir_store_contract::job::handover(&store(&ticker)).await;
}
