mod support;

use fhir_adapter_relational::migration::{Migrator, State};
use fhir_adapter_relational::MIGRATIONS;

#[tokio::test]
async fn latest_applies_every_migration_once() {
    let Some(pool) = support::engine().await else {
        return;
    };
    let namespace = support::namespace("latest");
    let migrator = Migrator::new(pool.clone(), namespace.clone());
    assert_eq!(migrator.version().await.unwrap(), None);
    let applied = migrator.latest().await.unwrap();
    assert_eq!(applied, MIGRATIONS.len());
    assert_eq!(
        migrator.version().await.unwrap(),
        Some(fhir_adapter_relational::migration::latest())
    );
    assert_eq!(migrator.latest().await.unwrap(), 0);
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn next_applies_exactly_one_migration() {
    let Some(pool) = support::engine().await else {
        return;
    };
    let namespace = support::namespace("next");
    let migrator = Migrator::new(pool.clone(), namespace.clone());
    assert_eq!(migrator.next().await.unwrap(), Some(1));
    assert_eq!(migrator.version().await.unwrap(), Some(1));
    assert_eq!(migrator.next().await.unwrap(), Some(2));
    assert_eq!(migrator.version().await.unwrap(), Some(2));
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn applied_migrations_are_recorded_in_order() {
    let Some(pool) = support::engine().await else {
        return;
    };
    let namespace = support::namespace("recorded");
    let migrator = Migrator::new(pool.clone(), namespace.clone());
    migrator.latest().await.unwrap();
    let applied = migrator.applied().await.unwrap();
    let versions: Vec<u32> = applied.iter().map(|entry| entry.version).collect();
    let expected: Vec<u32> = MIGRATIONS.iter().map(|entry| entry.version).collect();
    assert_eq!(versions, expected);
    assert!(applied.iter().all(|entry| !entry.applied_at.is_empty()));
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn force_records_a_version_without_replaying_the_earlier_ones() {
    let Some(pool) = support::engine().await else {
        return;
    };
    let namespace = support::namespace("force");
    let migrator = Migrator::new(pool.clone(), namespace.clone());
    migrator.force(1).await.unwrap();
    assert_eq!(migrator.version().await.unwrap(), Some(1));
    migrator.force(1).await.unwrap();
    assert_eq!(migrator.applied().await.unwrap().len(), 1);
    assert!(migrator
        .force(fhir_adapter_relational::migration::latest() + 1)
        .await
        .is_err());
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn an_instance_reports_the_schema_it_can_serve() {
    let Some(pool) = support::engine().await else {
        return;
    };
    let namespace = support::namespace("compat");
    let migrator = Migrator::new(pool.clone(), namespace.clone());
    let behind = migrator.compatibility().await.unwrap();
    assert_eq!(behind.state, State::Behind);
    assert!(!behind.is_compatible());
    migrator.latest().await.unwrap();
    let ready = migrator.compatibility().await.unwrap();
    assert_eq!(ready.state, State::Compatible);
    assert!(ready.is_compatible());
    assert_eq!(ready.instance, fhir_adapter_relational::migration::latest());
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_schema_newer_than_the_instance_is_reported_ahead() {
    let Some(pool) = support::engine().await else {
        return;
    };
    let namespace = support::namespace("ahead");
    let migrator = Migrator::new(pool.clone(), namespace.clone());
    migrator.latest().await.unwrap();
    migrator
        .record(fhir_adapter_relational::migration::latest() + 1)
        .await
        .unwrap();
    let ahead = migrator.compatibility().await.unwrap();
    assert_eq!(ahead.state, State::Ahead);
    assert!(!ahead.is_compatible());
    support::drop_namespace(&pool, &namespace).await;
}
