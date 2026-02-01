mod support;

use fhir_store::{
    ChangeFeed, ChangeKind, ChangeRecord, Continuation, FeedRange, ResourceStore, StoreScope,
};
use fhir_store_contract::fixture::{id, patient};

fn kinds(records: &[ChangeRecord]) -> Vec<ChangeKind> {
    records.iter().map(|record| record.kind).collect()
}

fn ids(records: &[ChangeRecord]) -> Vec<String> {
    records
        .iter()
        .map(|record| record.id.as_str().to_owned())
        .collect()
}

async fn every(store: &fhir_adapter_document::DocumentStore) -> Vec<ChangeRecord> {
    store
        .changes(FeedRange::whole(), &Continuation::start(), usize::MAX)
        .await
        .expect("the feed is readable")
        .records
}

#[tokio::test]
async fn one_record_is_written_per_create_update_and_delete() {
    let Some((store, client, namespace)) = support::fresh("recorded").await else {
        return;
    };
    store.create(patient("c1", "Stone", true)).await.unwrap();
    store.update(patient("c1", "Rivers", true), None).await.unwrap();
    store.delete(&id("c1")).await.unwrap();

    let records = every(&store).await;
    assert_eq!(
        kinds(&records),
        vec![ChangeKind::Created, ChangeKind::Updated, ChangeKind::Deleted]
    );
    assert_eq!(ids(&records), vec!["c1", "c1", "c1"]);
    let versions: Vec<&str> = records
        .iter()
        .map(|record| record.version.as_str())
        .collect();
    assert_eq!(versions, vec!["1", "2", "3"]);
    let order: Vec<i64> = records.iter().map(|record| record.sequence).collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn an_update_that_changes_nothing_records_nothing() {
    let Some((store, client, namespace)) = support::fresh("unchanged").await else {
        return;
    };
    store.create(patient("u1", "Stone", true)).await.unwrap();
    store.update(patient("u1", "Stone", true), None).await.unwrap();
    assert_eq!(every(&store).await.len(), 1);
    store.update(patient("u1", "Rivers", true), None).await.unwrap();
    assert_eq!(every(&store).await.len(), 2);
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn a_delete_is_recorded_once_and_never_twice() {
    let Some((store, client, namespace)) = support::fresh("deleted_once").await else {
        return;
    };
    store.create(patient("d1", "Stone", true)).await.unwrap();
    store.delete(&id("d1")).await.unwrap();
    assert!(store.delete(&id("d1")).await.is_err());
    let records = every(&store).await;
    let deletions = records
        .iter()
        .filter(|record| record.kind == ChangeKind::Deleted)
        .count();
    assert_eq!(deletions, 1, "{:?}", kinds(&records));
    assert_eq!(records.len(), 2);
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn a_reader_resumes_where_it_left_off() {
    let Some((store, client, namespace)) = support::fresh("resumed").await else {
        return;
    };
    for name in ["r1", "r2", "r3", "r4", "r5"] {
        store.create(patient(name, "Stone", true)).await.unwrap();
    }
    let mut position = Continuation::start();
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..3 {
        let page = store
            .changes(FeedRange::whole(), &position, 2)
            .await
            .expect("the feed is readable");
        seen.extend(ids(&page.records));
        position = page.next;
    }
    assert_eq!(seen, vec!["r1", "r2", "r3", "r4", "r5"]);
    let spent = store
        .changes(FeedRange::whole(), &position, 2)
        .await
        .unwrap();
    assert!(spent.records.is_empty());
    assert_eq!(spent.next, position);

    let held = position.as_str();
    let read_back = Continuation::parse(&held).expect("a written position reads back");
    assert_eq!(read_back, position);
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn the_halves_of_a_range_carry_every_record_once() {
    let Some((store, client, namespace)) = support::fresh("ranged_feed").await else {
        return;
    };
    for name in ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"] {
        store.create(patient(name, "Stone", true)).await.unwrap();
    }
    let (left, right) = FeedRange::whole().split().expect("the range splits");
    let start = Continuation::start();
    let below = store.changes(left, &start, usize::MAX).await.unwrap();
    let above = store.changes(right, &start, usize::MAX).await.unwrap();
    let whole = every(&store).await;
    assert_eq!(below.records.len() + above.records.len(), whole.len());
    for record in &below.records {
        assert!(left.holds(record.id.as_str()));
        assert!(!right.holds(record.id.as_str()));
    }
    for record in &above.records {
        assert!(right.holds(record.id.as_str()));
    }
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn a_discarded_scope_records_nothing() {
    let Some((store, client, namespace)) = support::fresh("discarded").await else {
        return;
    };
    let scope = store.begin().await.expect("the store opens a scope");
    scope.store().create(patient("s1", "Stone", true)).await.unwrap();
    scope.rollback().await.unwrap();
    assert!(every(&store).await.is_empty());

    let scope = store.begin().await.unwrap();
    scope.store().create(patient("s2", "Rivers", true)).await.unwrap();
    scope.store().update(patient("s2", "Fields", true), None).await.unwrap();
    scope.commit().await.unwrap();
    assert_eq!(
        kinds(&every(&store).await),
        vec![ChangeKind::Created, ChangeKind::Updated]
    );
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn removing_a_resource_leaves_the_record_of_the_writes_it_took() {
    let Some((store, client, namespace)) = support::fresh("kept").await else {
        return;
    };
    store.create(patient("h1", "Stone", true)).await.unwrap();
    store.update(patient("h1", "Rivers", true), None).await.unwrap();
    assert_eq!(store.purge_history(&id("h1")).await.unwrap(), 1);
    store.hard_delete(&id("h1")).await.unwrap();
    assert!(store.read(&id("h1")).await.is_err());
    assert_eq!(
        kinds(&every(&store).await),
        vec![ChangeKind::Created, ChangeKind::Updated]
    );
    support::drop_namespace(&client, &namespace).await;
}
