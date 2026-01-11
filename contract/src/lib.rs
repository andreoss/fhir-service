
pub mod fixture;
pub mod job;
pub mod search;

use fhir_core::Error;
use fhir_store::{HistoryOrder, HistoryQuery, HistoryScope, ResourceStore};

use fixture::{id, observation, patient, version};

fn assert_not_found(result: Result<impl std::fmt::Debug, Error>) {
    assert!(matches!(result, Err(Error::NotFound)), "{result:?}");
}

pub async fn lifecycle(store: &dyn ResourceStore) {
    let created = store.create(patient("a1", "Stone", true)).await.unwrap();
    assert_eq!(created.version_id().as_str(), "1");
    assert_eq!(created.id().as_str(), "a1");
    assert_eq!(created.resource_type().as_str(), "Patient");
    assert!(!created.is_deleted());

    let read = store.read(&id("a1")).await.unwrap();
    assert_eq!(read.version_id(), created.version_id());
    assert_eq!(read.raw(), created.raw());
    assert_eq!(read.last_updated(), created.last_updated());

    let duplicate = store.create(patient("a1", "Stone", false)).await;
    assert!(matches!(duplicate, Err(Error::Duplicate(_))), "{duplicate:?}");

    assert_not_found(store.read(&id("nobody")).await);
    assert_not_found(store.vread(&id("nobody"), &version("1")).await);
}

pub async fn versioning(store: &dyn ResourceStore) {
    store.create(patient("b1", "Stone", true)).await.unwrap();

    let updated = store.update(patient("b1", "Stone", false), None).await.unwrap();
    assert_eq!(updated.version_id().as_str(), "2");

    let first = store.vread(&id("b1"), &version("1")).await.unwrap();
    assert_eq!(first.version_id().as_str(), "1");
    let second = store.vread(&id("b1"), &version("2")).await.unwrap();
    assert_eq!(second.raw(), updated.raw());
    assert_not_found(store.vread(&id("b1"), &version("3")).await);

    let repeated = store.update(patient("b1", "Stone", false), None).await.unwrap();
    assert_eq!(repeated.version_id().as_str(), "2");

    let expected = store
        .update(patient("b1", "Rivers", true), Some(&version("2")))
        .await
        .unwrap();
    assert_eq!(expected.version_id().as_str(), "3");

    let stale = store.update(patient("b1", "Fields", true), Some(&version("1"))).await;
    assert!(matches!(stale, Err(Error::VersionConflict)), "{stale:?}");
    assert_eq!(store.read(&id("b1")).await.unwrap().version_id().as_str(), "3");

    assert_not_found(store.update(patient("absent", "Stone", true), None).await);
}

pub async fn removal(store: &dyn ResourceStore) {
    store.create(patient("c1", "Stone", true)).await.unwrap();
    store.update(patient("c1", "Rivers", true), None).await.unwrap();

    let marker = store.delete(&id("c1")).await.unwrap();
    assert!(marker.is_deleted());
    assert_eq!(marker.version_id().as_str(), "3");
    assert!(store.read(&id("c1")).await.unwrap().is_deleted());
    assert!(!store.vread(&id("c1"), &version("1")).await.unwrap().is_deleted());

    let again = store.delete(&id("c1")).await;
    assert!(matches!(again, Err(Error::Deleted)), "{again:?}");

    let restored = store.update(patient("c1", "Stone", true), None).await.unwrap();
    assert_eq!(restored.version_id().as_str(), "4");
    assert!(!restored.is_deleted());

    assert_eq!(store.purge_history(&id("c1")).await.unwrap(), 3);
    assert_eq!(store.read(&id("c1")).await.unwrap().version_id().as_str(), "4");
    assert_not_found(store.vread(&id("c1"), &version("1")).await);

    store.hard_delete(&id("c1")).await.unwrap();
    assert_not_found(store.read(&id("c1")).await);
    assert_not_found(store.hard_delete(&id("c1")).await);
    assert_not_found(store.purge_history(&id("absent")).await);
}

pub async fn record(store: &dyn ResourceStore) {
    store.create(patient("d1", "Stone", true)).await.unwrap();
    store.update(patient("d1", "Rivers", true), None).await.unwrap();
    store.delete(&id("d1")).await.unwrap();
    store.create(observation("d2", "code-1", 3.0, "Patient/d1")).await.unwrap();

    let instance = HistoryScope::Instance("Patient".parse().unwrap(), id("d1"));
    let newest = store.history(&instance, &HistoryQuery::default()).await.unwrap();
    assert_eq!(newest.total, 3);
    assert_eq!(newest.offset, 0);
    let versions: Vec<&str> = newest.entries.iter().map(|entry| entry.version_id().as_str()).collect();
    assert_eq!(versions, vec!["3", "2", "1"]);
    assert!(newest.entries[0].is_deleted());

    let oldest = HistoryQuery {
        order: HistoryOrder::Oldest,
        ..HistoryQuery::default()
    };
    let earliest = store.history(&instance, &oldest).await.unwrap();
    let versions: Vec<&str> = earliest.entries.iter().map(|entry| entry.version_id().as_str()).collect();
    assert_eq!(versions, vec!["1", "2", "3"]);

    let paged = HistoryQuery {
        offset: 1,
        count: 1,
        ..HistoryQuery::default()
    };
    let page = store.history(&instance, &paged).await.unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(page.offset, 1);
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].version_id().as_str(), "2");

    let by_type = HistoryScope::Type("Patient".parse().unwrap());
    assert_eq!(store.history(&by_type, &HistoryQuery::default()).await.unwrap().total, 3);
    let other = HistoryScope::Type("Observation".parse().unwrap());
    assert_eq!(store.history(&other, &HistoryQuery::default()).await.unwrap().total, 1);
    let all = store.history(&HistoryScope::System, &HistoryQuery::default()).await.unwrap();
    assert_eq!(all.total, 4);

    let future = HistoryQuery {
        since: Some(fhir_core::InstantPeriod::parse("2099").unwrap()),
        ..HistoryQuery::default()
    };
    assert_eq!(store.history(&instance, &future).await.unwrap().total, 0);

    let unknown = HistoryScope::Instance("Patient".parse().unwrap(), id("absent"));
    assert_not_found(store.history(&unknown, &HistoryQuery::default()).await);
    let mistyped = HistoryScope::Instance("Observation".parse().unwrap(), id("d1"));
    assert_not_found(store.history(&mistyped, &HistoryQuery::default()).await);
}

pub async fn readiness(store: &dyn ResourceStore) {
    store.health().expect("a fresh store is healthy");
}

pub async fn atomicity(store: &dyn ResourceStore) {
    let scope = store.begin().await.expect("the store opens a scope");
    let scoped = scope.store();
    scoped.create(patient("f1", "Stone", true)).await.unwrap();
    scoped.create(patient("f2", "Rivers", true)).await.unwrap();
    assert_eq!(scoped.read(&id("f1")).await.unwrap().version_id().as_str(), "1");
    scope.rollback().await.unwrap();
    assert_not_found(store.read(&id("f1")).await);
    assert_not_found(store.read(&id("f2")).await);

    let scope = store.begin().await.unwrap();
    let scoped = scope.store();
    scoped.create(patient("f3", "Stone", true)).await.unwrap();
    scoped.update(patient("f3", "Rivers", true), None).await.unwrap();
    scope.commit().await.unwrap();
    assert_eq!(store.read(&id("f3")).await.unwrap().version_id().as_str(), "2");

    let scope = store.begin().await.unwrap();
    let scoped = scope.store();
    scoped.delete(&id("f3")).await.unwrap();
    assert!(scoped.read(&id("f3")).await.unwrap().is_deleted());
    scope.rollback().await.unwrap();
    assert!(!store.read(&id("f3")).await.unwrap().is_deleted());
    assert_eq!(
        store
            .history(&HistoryScope::Instance("Patient".parse().unwrap(), id("f3")), &HistoryQuery::default())
            .await
            .unwrap()
            .total,
        2
    );
    store.hard_delete(&id("f3")).await.unwrap();
}

pub async fn scoped_search(store: &dyn ResourceStore) {
    let by_name = |value: &str| fhir_store::SearchQuery {
        filters: vec![crate::search::filter("Patient", "family", value)],
        ..fhir_store::SearchQuery::of_type("Patient".parse().expect("a known type"))
    };

    let scope = store.begin().await.expect("the store opens a scope");
    let scoped = scope.store();
    scoped.create(patient("g1", "Stone", true)).await.unwrap();
    let inside = scoped.search(&by_name("Stone")).await.unwrap();
    assert_eq!(inside.entries.len(), 1);
    scope.rollback().await.unwrap();
    assert!(store.search(&by_name("Stone")).await.unwrap().entries.is_empty());
}
