mod support;

use fhir_adapter_relational::fault::{classify, Fault, Policy};
use fhir_adapter_relational::migration::{latest, lowest_compatible, Migrator, State};
use fhir_adapter_relational::Throttle;
use fhir_store::{ResourceStore, SearchQuery};
use fhir_store_contract::fixture::{id, patient};
use std::sync::Arc;

#[tokio::test]
async fn a_violated_constraint_is_permanent_and_is_not_repeated() {
    let Some((store, pool, namespace)) = support::fresh("permanent").await else { return };
    let statement = format!(
        "insert into {}.schema_version (version, name) values (1, 'again')",
        namespace.as_str()
    );
    let failure = sqlx::query(&statement).execute(&pool).await.unwrap_err();
    assert_eq!(classify(&failure), Fault::Permanent);
    assert!(!classify(&failure).is_retriable());
    let _ = store.health();
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_malformed_statement_is_permanent() {
    let Some(pool) = support::engine().await else { return };
    let failure = sqlx::query("select sideways from nowhere")
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(classify(&failure), Fault::Permanent);
}

#[tokio::test]
async fn a_store_that_lost_its_engine_fails_fast() {
    let Some((store, pool, namespace)) = support::fresh("closed").await else { return };
    store.create(patient("f1", "Stone", true)).await.unwrap();
    let store = fhir_adapter_relational::RelationalStore::new(pool.clone(), namespace.clone())
        .with_policy(Policy::once());
    assert_eq!(store.policy().attempts, 1);
    assert!(store.read(&id("f1")).await.is_ok());
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn no_more_work_reaches_the_engine_at_once_than_the_limit_allows() {
    let Some((_store, pool, namespace)) = support::fresh("throttled").await else { return };
    let store = Arc::new(
        fhir_adapter_relational::RelationalStore::new(pool.clone(), namespace.clone())
            .with_throttle(Throttle::new(2)),
    );
    let mut running = Vec::new();
    for index in 0..8 {
        let store = Arc::clone(&store);
        running.push(tokio::spawn(async move {
            store
                .create(patient(&format!("t{index}"), "Stone", true))
                .await
                .map(|_| ())
        }));
    }
    for task in running {
        task.await.unwrap().unwrap();
    }
    assert_eq!(store.throttle().limit(), 2);
    assert!(store.throttle().peak() <= 2, "{}", store.throttle().peak());
    assert_eq!(store.throttle().running(), 0);
    assert_eq!(
        store.search(&SearchQuery::default()).await.unwrap().total,
        Some(8)
    );
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn reclaiming_space_leaves_the_records_readable() {
    let Some((store, pool, namespace)) = support::fresh("reclaimed").await else { return };
    for index in 0..20 {
        store
            .create(patient(&format!("r{index}"), "Stone", true))
            .await
            .unwrap();
    }
    for index in 0..10 {
        store.hard_delete(&id(&format!("r{index}"))).await.unwrap();
    }
    assert_eq!(store.defragment().await.unwrap(), 9);
    assert_eq!(
        store.search(&SearchQuery::default()).await.unwrap().total,
        Some(10)
    );
    assert!(store.read(&id("r19")).await.is_ok());
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_schema_that_lacks_only_tuning_is_still_served() {
    let Some(pool) = support::engine().await else { return };
    let namespace = support::namespace("tuned");
    let migrator = Migrator::new(pool.clone(), namespace.clone());
    assert!(lowest_compatible() < latest());
    while migrator.version().await.unwrap().unwrap_or_default() < lowest_compatible() {
        migrator.next().await.unwrap();
    }
    let report = migrator.compatibility().await.unwrap();
    assert_eq!(report.state, State::Compatible);
    assert!(report.current.unwrap() < report.instance);
    migrator.latest().await.unwrap();
    assert_eq!(migrator.version().await.unwrap(), Some(latest()));
    support::drop_namespace(&pool, &namespace).await;
}
