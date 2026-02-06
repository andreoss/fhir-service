mod support;

use fhir_adapter_relational::{Namespace, RelationalJobStore};
use fhir_store::StepTicker;
use fhir_store::JobStore;
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
suite!(an_ended_job_releases_its_description, defragmentation, "jdefrag");
suite!(
    an_ended_job_is_kept_for_its_retention_and_then_removed,
    retention,
    "jkeep",
    timed
);
suite!(a_stop_reaches_a_queued_and_a_running_job, cancellation, "jstop");
suite!(a_kind_runs_no_more_jobs_at_once_than_its_limit, concurrency, "jcap");
suite!(one_kind_never_holds_another_back, limits_are_per_kind, "jkinds");
suite!(
    a_kind_starts_no_more_often_than_its_throttle,
    throttling,
    "jgap",
    timed
);
suite!(work_that_cannot_succeed_fails_at_once, rejection, "jreject");
suite!(
    a_running_job_learns_of_a_stop_at_its_next_heartbeat,
    cancel_signal,
    "jsignal"
);
suite!(a_failed_attempt_waits_and_runs_again, retries, "jretry", timed);
suite!(a_job_a_stopped_worker_held_is_claimed_again, recovery, "jresume", timed);
suite!(a_job_with_no_attempt_left_fails_on_reclaim, exhaustion, "jspent", timed);
suite!(
    a_resumed_job_keeps_the_identifier_it_started_with,
    correlation,
    "jtie",
    timed
);
suite!(
    a_cancelling_job_whose_worker_stopped_ends_cancelled,
    stopped_while_cancelling,
    "jgone",
    timed
);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_limit_holds_when_many_workers_claim_at_once() {
    let Some((jobs, _ticker, pool, namespace)) = queue("jload").await else {
        return;
    };
    let jobs = std::sync::Arc::new(jobs);
    for slot in 0..12 {
        let id = fhir_store::JobId::parse(&format!("load-{slot}")).unwrap();
        jobs.submit(fhir_store::JobRequest::new(
            id,
            fhir_store::JobKind::Export,
            "{}",
        ))
        .await
        .unwrap();
    }
    let limits = fhir_store::JobLimits::unlimited().running(fhir_store::JobKind::Export, 3);

    let mut claiming = Vec::new();
    for worker in 0..8 {
        let jobs = std::sync::Arc::clone(&jobs);
        claiming.push(tokio::spawn(async move {
            let lease = fhir_store::Lease::new(format!("w{worker}"), 60_000)
                .with_limit(12)
                .with_limits(limits);
            jobs.claim(&lease).await.unwrap().len()
        }));
    }
    let mut taken = 0;
    for task in claiming {
        taken += task.await.unwrap();
    }

    let running = jobs
        .list(&fhir_store::JobFilter::in_state(fhir_store::JobState::Running))
        .await
        .unwrap();
    assert_eq!(taken, 3, "the limit did not hold under load");
    assert_eq!(running.len(), 3);
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sink_satisfies_the_output_contract() {
    let Some((store, pool, namespace)) = support::fresh("jout").await else {
        return;
    };
    fhir_store_contract::bulk::outputs(&store.outputs()).await;
    support::drop_namespace(&pool, &namespace).await;
}
