mod support;

use fhir_adapter_document::range::{partition, FeedRange};
use fhir_store::{ResourceStore, SearchQuery};
use fhir_store_contract::fixture::{id, observation, patient};
use mongodb::bson::{doc, Document};
use mongodb::Client;

async fn documents(client: &Client, namespace: &str, filter: Document) -> u64 {
    client
        .database(namespace)
        .collection::<Document>("resource")
        .count_documents(filter)
        .await
        .expect("the collection is readable")
}

async fn one(client: &Client, namespace: &str, filter: Document) -> Document {
    client
        .database(namespace)
        .collection::<Document>("resource")
        .find_one(filter)
        .await
        .expect("the collection is readable")
        .expect("the version is held")
}

fn kind(name: &str) -> fhir_core::ResourceType {
    name.parse().expect("a known type")
}

#[tokio::test]
async fn preparing_a_namespace_twice_leaves_it_as_it_was() {
    let Some((store, client, namespace)) = support::fresh("prepared").await else {
        return;
    };
    let again = store
        .initialise()
        .await
        .expect("the namespace prepares again");
    assert!(again > 0);
    store.create(patient("i1", "Stone", true)).await.unwrap();
    store.initialise().await.expect("preparing keeps the data");
    assert_eq!(
        store
            .read(&fhir_core::ResourceKey::new(
                "Patient".parse().unwrap(),
                id("i1")
            ))
            .await
            .unwrap()
            .version_id()
            .as_str(),
        "1"
    );
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn a_written_resource_carries_its_values_into_its_index_arrays() {
    let Some((store, client, namespace)) = support::fresh("indexed").await else {
        return;
    };
    store
        .create(observation("o1", "code-1", 4.5, "Patient/p1"))
        .await
        .unwrap();
    let held = one(&client, namespace.as_str(), doc! {"resource_id": "o1"});
    let held = held.await;
    assert!(!held.get_array("token").unwrap().is_empty());
    assert!(!held.get_array("quantity").unwrap().is_empty());
    assert!(!held.get_array("reference").unwrap().is_empty());
    assert!(!held.get_array("sort").unwrap().is_empty());
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn only_the_current_version_answers_a_search() {
    let Some((store, client, namespace)) = support::fresh("current").await else {
        return;
    };
    store
        .create(observation("m1", "code-1", 1.0, "Patient/p1"))
        .await
        .unwrap();
    store
        .update(observation("m1", "code-2", 2.0, "Patient/p2"), None)
        .await
        .unwrap();
    let name = namespace.as_str();
    assert_eq!(
        documents(&client, name, doc! {"resource_id": "m1"}).await,
        2
    );
    assert_eq!(
        documents(
            &client,
            name,
            doc! {"resource_id": "m1", "is_current": true}
        )
        .await,
        1
    );
    let stale = store
        .search(&SearchQuery {
            filters: vec![fhir_store_contract::search::filter(
                "Observation",
                "code",
                "code-1",
            )],
            ..SearchQuery::of_type(kind("Observation"))
        })
        .await
        .unwrap();
    assert!(stale.entries.is_empty(), "{:?}", stale.total);
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn a_delete_marker_carries_no_indexed_value() {
    let Some((store, client, namespace)) = support::fresh("marker").await else {
        return;
    };
    store.create(patient("k1", "Stone", true)).await.unwrap();
    store
        .delete(&fhir_core::ResourceKey::new(
            "Patient".parse().unwrap(),
            id("k1"),
        ))
        .await
        .unwrap();
    let name = namespace.as_str();
    let held = one(
        &client,
        name,
        doc! {"resource_id": "k1", "is_current": true},
    )
    .await;
    assert!(held.get_array("text").unwrap().is_empty());
    assert_eq!(
        documents(&client, name, doc! {"resource_id": "k1"}).await,
        2
    );
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn removing_a_resource_takes_every_version_with_it() {
    let Some((store, client, namespace)) = support::fresh("removed").await else {
        return;
    };
    store
        .create(observation("r1", "code-1", 1.0, "Patient/p1"))
        .await
        .unwrap();
    store
        .update(observation("r1", "code-2", 1.0, "Patient/p1"), None)
        .await
        .unwrap();
    store
        .hard_delete(&fhir_core::ResourceKey::new(
            "Observation".parse().unwrap(),
            id("r1"),
        ))
        .await
        .unwrap();
    assert_eq!(
        documents(&client, namespace.as_str(), doc! {"resource_id": "r1"}).await,
        0
    );
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn two_namespaces_hold_separate_records() {
    let Some((first, client, one)) = support::fresh("split_one").await else {
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
    assert_eq!(documents(&client, one.as_str(), Document::new()).await, 1);
    assert_eq!(documents(&client, two.as_str(), Document::new()).await, 0);
    support::drop_namespace(&client, &one).await;
    support::drop_namespace(&client, &two).await;
}

#[tokio::test]
async fn a_search_never_reads_a_body_to_decide_a_match() {
    let Some((store, client, namespace)) = support::fresh("no_sift").await else {
        return;
    };
    store.create(patient("b1", "Stone", true)).await.unwrap();
    client
        .database(namespace.as_str())
        .collection::<Document>("resource")
        .update_many(
            doc! {"resource_id": "b1"},
            doc! {"$set": {"body": mongodb::bson::Binary {
                subtype: mongodb::bson::spec::BinarySubtype::Generic,
                bytes: b"{}".to_vec(),
            }, "body_encoding": "plain"}},
        )
        .await
        .expect("the body is replaceable");
    let found = store
        .search(&SearchQuery {
            filters: vec![fhir_store_contract::search::filter(
                "Patient", "family", "Stone",
            )],
            count: 0,
            ..SearchQuery::of_type(kind("Patient"))
        })
        .await
        .unwrap();
    assert_eq!(found.total, Some(1));
    assert!(found.entries.is_empty());
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn a_page_is_taken_from_the_same_order_every_time() {
    let Some((store, client, namespace)) = support::fresh("paged").await else {
        return;
    };
    for name in ["a", "b", "c", "d", "e"] {
        store
            .create(patient(&format!("q{name}"), name, true))
            .await
            .unwrap();
    }
    let page = |offset: usize| SearchQuery {
        offset,
        count: 2,
        ..SearchQuery::of_type(kind("Patient"))
    };
    let ids = |found: fhir_store::SearchPage| -> Vec<String> {
        found
            .entries
            .iter()
            .map(|entry| entry.id().as_str().to_owned())
            .collect()
    };
    let first = ids(store.search(&page(0)).await.unwrap());
    let second = ids(store.search(&page(2)).await.unwrap());
    let third = ids(store.search(&page(4)).await.unwrap());
    assert_eq!(first, vec!["qa", "qb"]);
    assert_eq!(second, vec!["qc", "qd"]);
    assert_eq!(third, vec!["qe"]);
    assert_eq!(ids(store.search(&page(0)).await.unwrap()), first);
    assert_eq!(store.search(&page(2)).await.unwrap().offset, 2);
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn a_plan_is_measured_by_the_candidates_it_draws() {
    let Some((store, client, namespace)) = support::fresh("planned").await else {
        return;
    };
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", false)).await.unwrap();
    let indexed = SearchQuery {
        filters: vec![fhir_store_contract::search::filter(
            "Patient", "family", "Stone",
        )],
        ..SearchQuery::of_type(kind("Patient"))
    };
    assert_eq!(store.search(&indexed).await.unwrap().total, Some(1));
    let plans = store.plans();
    assert_eq!(plans.len(), 1);
    assert!(plans[0].indexed, "{plans:?}");
    assert_eq!(plans[0].baseline, 2);

    store
        .search(&SearchQuery::of_type(kind("Patient")))
        .await
        .unwrap();
    let scans: Vec<_> = store
        .plans()
        .into_iter()
        .filter(|plan| !plan.indexed)
        .collect();
    assert_eq!(scans.len(), 1);
    assert_eq!(scans[0].baseline, 2);
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn every_written_resource_lands_in_the_range_that_holds_it() {
    let Some((store, client, namespace)) = support::fresh("ranged").await else {
        return;
    };
    let whole = FeedRange::whole();
    let (left, right) = whole.split().expect("the whole range splits");
    let mut seen = 0;
    for name in ["f1", "f2", "f3", "f4", "f5", "f6"] {
        store.create(patient(name, "Stone", true)).await.unwrap();
        assert!(whole.holds(name));
        assert_ne!(left.holds(name), right.holds(name));
        let held = one(&client, namespace.as_str(), doc! {"resource_id": name}).await;
        assert_eq!(held.get_i32("partition"), Ok(partition(name)));
        seen += 1;
    }
    assert_eq!(seen, 6);
    support::drop_namespace(&client, &namespace).await;
}

#[tokio::test]
async fn an_index_an_operator_named_is_added_after_the_namespace_and_only_once() {
    let Some((store, client, namespace)) = support::fresh("tuned").await else {
        return;
    };
    let extras = vec![
        fhir_store::tuning::Extra {
            backend: "document".to_owned(),
            kind: "token".to_owned(),
            param: "code".to_owned(),
        },
        fhir_store::tuning::Extra {
            backend: "document".to_owned(),
            kind: "reference".to_owned(),
            param: "subject".to_owned(),
        },
    ];
    let applied = store.tune(&extras).await.expect("the indexes are added");
    assert_eq!(applied, vec!["tune_token_code", "tune_reference_subject"]);

    let found = client
        .database(namespace.as_str())
        .collection::<mongodb::bson::Document>("resource")
        .list_index_names()
        .await
        .expect("the indexes are listed");
    let tuned: Vec<&String> = found
        .iter()
        .filter(|name| name.starts_with("tune_"))
        .collect();
    assert_eq!(tuned.len(), 2, "{found:?}");

    store.tune(&extras).await.expect("asking twice is allowed");
    support::drop_namespace(&client, &namespace).await;
}
