mod support;

use fhir_store::ResourceStore;
use fhir_store_contract::fixture::{id, observation, patient};
use sqlx::{PgPool, Row};

async fn count(pool: &PgPool, namespace: &str, table: &str) -> i64 {
    let statement = format!("select count(*) as total from {namespace}.{table}");
    sqlx::query(&statement)
        .fetch_one(pool)
        .await
        .expect("the index table is readable")
        .get::<i64, _>("total")
}

async fn surrogates(pool: &PgPool, namespace: &str) -> Vec<i64> {
    let statement = format!("select surrogate_id from {namespace}.resource order by surrogate_id");
    sqlx::query(&statement)
        .fetch_all(pool)
        .await
        .expect("the resource table is readable")
        .iter()
        .map(|row| row.get::<i64, _>("surrogate_id"))
        .collect()
}

#[tokio::test]
async fn a_written_resource_carries_its_values_into_the_index_tables() {
    let Some((store, pool, namespace)) = support::fresh("indexed").await else {
        return;
    };
    let name = namespace.as_str();
    store
        .create(observation("o1", "code-1", 4.5, "Patient/p1"))
        .await
        .unwrap();
    assert!(count(&pool, name, "index_token").await > 0);
    assert!(count(&pool, name, "index_quantity").await > 0);
    assert!(count(&pool, name, "index_reference").await > 0);
    assert!(count(&pool, name, "index_text").await > 0);
    assert!(count(&pool, name, "index_sort").await > 0);
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn every_version_takes_a_surrogate_key_of_its_own() {
    let Some((store, pool, namespace)) = support::fresh("surrogate").await else {
        return;
    };
    store.create(patient("s1", "Stone", true)).await.unwrap();
    store
        .update(patient("s1", "Rivers", true), None)
        .await
        .unwrap();
    let keys = surrogates(&pool, namespace.as_str()).await;
    assert_eq!(keys.len(), 2);
    assert!(keys[0] < keys[1]);
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_new_version_replaces_the_index_values_of_the_one_it_supersedes() {
    let Some((store, pool, namespace)) = support::fresh("merged").await else {
        return;
    };
    let name = namespace.as_str();
    store
        .create(observation("m1", "code-1", 1.0, "Patient/p1"))
        .await
        .unwrap();
    let first = count(&pool, name, "index_token").await;
    store
        .update(observation("m1", "code-2", 2.0, "Patient/p2"), None)
        .await
        .unwrap();
    assert_eq!(count(&pool, name, "index_token").await, first);
    let statement =
        format!("select count(*) as total from {name}.index_token where code = 'code-1'");
    let stale: i64 = sqlx::query(&statement)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("total");
    assert_eq!(stale, 0);
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_delete_marker_carries_no_indexed_value() {
    let Some((store, pool, namespace)) = support::fresh("marker").await else {
        return;
    };
    let name = namespace.as_str();
    store.create(patient("k1", "Stone", true)).await.unwrap();
    store
        .delete(&fhir_core::ResourceKey::new(
            "Patient".parse().unwrap(),
            id("k1"),
        ))
        .await
        .unwrap();
    assert_eq!(count(&pool, name, "index_text").await, 0);
    assert_eq!(count(&pool, name, "resource").await, 2);
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn removing_a_resource_takes_its_index_values_with_it() {
    let Some((store, pool, namespace)) = support::fresh("removed").await else {
        return;
    };
    let name = namespace.as_str();
    store
        .create(observation("r1", "code-1", 1.0, "Patient/p1"))
        .await
        .unwrap();
    store
        .hard_delete(&fhir_core::ResourceKey::new(
            "Observation".parse().unwrap(),
            id("r1"),
        ))
        .await
        .unwrap();
    assert_eq!(count(&pool, name, "resource").await, 0);
    assert_eq!(count(&pool, name, "index_token").await, 0);
    assert_eq!(count(&pool, name, "index_sort").await, 0);
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn purging_history_keeps_the_current_version_alone() {
    let Some((store, pool, namespace)) = support::fresh("purged").await else {
        return;
    };
    let name = namespace.as_str();
    store.create(patient("g1", "Stone", true)).await.unwrap();
    store
        .update(patient("g1", "Rivers", true), None)
        .await
        .unwrap();
    store
        .update(patient("g1", "Fields", true), None)
        .await
        .unwrap();
    assert_eq!(
        store
            .purge_history(&fhir_core::ResourceKey::new(
                "Patient".parse().unwrap(),
                id("g1")
            ))
            .await
            .unwrap(),
        2
    );
    assert_eq!(count(&pool, name, "resource").await, 1);
    assert!(count(&pool, name, "index_text").await > 0);
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn two_namespaces_hold_separate_records() {
    let Some((first, pool, one)) = support::fresh("split_one").await else {
        return;
    };
    let Some((second, _, two)) = support::fresh("split_two").await else {
        return;
    };
    first.create(patient("n1", "Stone", true)).await.unwrap();
    assert!(second
        .read(&fhir_core::ResourceKey::new(
            "Patient".parse().unwrap(),
            id("n1")
        ))
        .await
        .is_err());
    assert_eq!(count(&pool, one.as_str(), "resource").await, 1);
    assert_eq!(count(&pool, two.as_str(), "resource").await, 0);
    support::drop_namespace(&pool, &one).await;
    support::drop_namespace(&pool, &two).await;
}
