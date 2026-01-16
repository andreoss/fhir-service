mod support;

use fhir_adapter_relational::{Namespace, RelationalJobStore};
use fhir_store::StepTicker;
use sqlx::PgPool;

async fn queue(name: &str) -> Option<(RelationalJobStore, StepTicker, PgPool, Namespace)> {
    let (store, pool, namespace) = support::fresh(name).await?;
    let ticker = StepTicker::starting_at(1_000);
    let jobs = RelationalJobStore::new(pool.clone(), namespace.clone()).with_ticker(ticker.ticker());
    drop(store);
    Some((jobs, ticker, pool, namespace))
}

macro_rules! suite {
    ($name:ident, $group:ident, $slug:expr) => {
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            let Some((jobs, ticker, pool, namespace)) = queue($slug).await else {
                return;
            };
            let _ = &ticker;
            fhir_store_contract::job::$group(&jobs).await;
            support::drop_namespace(&pool, &namespace).await;
        }
    };
    ($name:ident, $group:ident, $slug:expr, timed) => {
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            let Some((jobs, ticker, pool, namespace)) = queue($slug).await else {
                return;
            };
            fhir_store_contract::job::$group(&jobs, &ticker).await;
            support::drop_namespace(&pool, &namespace).await;
        }
    };
}

suite!(a_submitted_job_reads_back, submission, "jsub");
suite!(a_claim_holds_one_job_under_a_lease, claiming, "jclaim");
suite!(a_heartbeat_extends_the_lease, heartbeats, "jbeat");
suite!(a_finished_attempt_keeps_its_result, completion, "jdone");
suite!(a_listing_filters_by_kind_and_state, listing, "jlist");
suite!(a_stop_reaches_a_queued_and_a_running_job, cancellation, "jstop");
suite!(work_that_cannot_succeed_fails_at_once, rejection, "jreject");
suite!(a_failed_attempt_waits_and_runs_again, retries, "jretry", timed);
suite!(a_job_a_stopped_worker_held_is_claimed_again, recovery, "jresume", timed);
suite!(a_job_with_no_attempt_left_fails_on_reclaim, exhaustion, "jspent", timed);
suite!(
    a_cancelling_job_whose_worker_stopped_ends_cancelled,
    stopped_while_cancelling,
    "jgone",
    timed
);
