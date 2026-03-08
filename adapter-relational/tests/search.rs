mod support;

use fhir_core::search::{Filter, ParameterSpec, SearchValue, Target};
use fhir_store::{ResourceStore, SearchQuery, SortDirection, SortKey};
use fhir_store_contract::fixture::{envelope, id, observation, patient};
use fhir_store_contract::search::filter;
use serde_json::json;
use sqlx::{PgPool, Row};

fn kind(name: &str) -> fhir_core::ResourceType {
    name.parse().unwrap()
}

fn spec() -> ParameterSpec {
    ParameterSpec::parse(&json!({
        "resourceType": "SearchParameter",
        "url": "urn:p:band",
        "status": "active",
        "code": "band",
        "base": ["Patient"],
        "type": "token",
        "expression": "Patient.extension.valueCode"
    }))
    .unwrap()
}

fn banded(id: &str, band: &str) -> fhir_core::ResourceEnvelope {
    envelope(
        "Patient",
        id,
        &format!(r#""active":true,"extension":[{{"valueCode":"{band}"}}]"#),
    )
}

fn ids(page: &fhir_store::SearchPage) -> Vec<String> {
    page.entries
        .iter()
        .map(|entry| entry.id().as_str().to_owned())
        .collect()
}

#[tokio::test]
async fn a_query_over_an_indexed_parameter_draws_from_the_index() {
    let Some((store, pool, namespace)) = support::fresh("planned").await else {
        return;
    };
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", false)).await.unwrap();
    let indexed = SearchQuery {
        filters: vec![filter("Patient", "active", "true")],
        ..SearchQuery::of_type(kind("Patient"))
    };
    assert_eq!(ids(&store.search(&indexed).await.unwrap()), vec!["p1"]);
    let plans = store.plans();
    assert_eq!(plans.len(), 1);
    assert!(plans[0].indexed, "{plans:?}");
    assert_eq!(plans[0].baseline, 1);

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
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_plan_that_stops_paying_for_itself_is_withdrawn() {
    let Some((store, pool, namespace)) = support::fresh("withdrawn").await else {
        return;
    };
    store.create(patient("w1", "Stone", true)).await.unwrap();
    let query = SearchQuery {
        filters: vec![filter("Patient", "active", "true")],
        ..SearchQuery::of_type(kind("Patient"))
    };
    store.search(&query).await.unwrap();
    for index in 0..40 {
        store
            .create(patient(&format!("w{}", index + 2), "Stone", true))
            .await
            .unwrap();
    }
    store.search(&query).await.unwrap();
    let plans = store.plans();
    assert!(plans.iter().any(|plan| plan.disabled), "{plans:?}");
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_custom_parameter_answers_only_once_its_index_is_backfilled() {
    let Some((store, pool, namespace)) = support::fresh("custom").await else {
        return;
    };
    store.create(banded("b1", "high")).await.unwrap();
    store.create(banded("b2", "low")).await.unwrap();
    let spec = spec();
    let report = store.index_parameter(&spec).await.unwrap();
    assert!(!report.backfilled);
    assert_eq!(
        store
            .index_report("urn:p:band")
            .await
            .unwrap()
            .unwrap()
            .indexed,
        0
    );

    let custom = |code: &str| SearchQuery {
        filters: vec![Filter {
            index: Some("urn:p:band".to_owned()),
            ..Filter::new(
                "band",
                Target::path(["extension.valueCode"]),
                vec![SearchValue::parse(fhir_core::search::ValueType::Token, code).unwrap()],
            )
        }],
        ..SearchQuery::of_type(kind("Patient"))
    };
    assert!(store
        .search(&custom("high"))
        .await
        .unwrap()
        .entries
        .is_empty());

    let reports = store.reindex(std::slice::from_ref(&spec)).await.unwrap();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].backfilled);
    assert_eq!(reports[0].indexed, 2);
    assert_eq!(
        ids(&store.search(&custom("high")).await.unwrap()),
        vec!["b1"]
    );

    store.drop_parameter("urn:p:band").await.unwrap();
    assert!(store.index_report("urn:p:band").await.unwrap().is_none());
    assert!(store
        .search(&custom("high"))
        .await
        .unwrap()
        .entries
        .is_empty());
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_resource_carrying_a_malformed_value_is_reported_and_skipped() {
    let Some((store, pool, namespace)) = support::fresh("failed").await else {
        return;
    };
    store.create(banded("f1", "high")).await.unwrap();
    let dated = ParameterSpec::parse(&json!({
        "resourceType": "SearchParameter",
        "url": "urn:p:when",
        "status": "active",
        "code": "when",
        "base": ["Patient"],
        "type": "date",
        "expression": "Patient.extension.valueCode"
    }))
    .unwrap();
    let reports = store.reindex(&[dated]).await.unwrap();
    assert_eq!(reports[0].failures.len(), 1);
    assert_eq!(reports[0].failures[0].resource, "Patient/f1");
    assert_eq!(reports[0].indexed, 0);
    assert!(reports[0].backfilled);
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn many_alternatives_are_bound_in_one_batch() {
    let Some((store, pool, namespace)) = support::fresh("batched").await else {
        return;
    };
    for index in 0..30 {
        store
            .create(patient(&format!("m{index}"), "Stone", true))
            .await
            .unwrap();
    }
    let wanted: Vec<String> = (0..30).map(|index| format!("m{index}")).collect();
    let query = SearchQuery {
        filters: vec![filter("Patient", "_id", &wanted.join(","))],
        ..SearchQuery::of_type(kind("Patient"))
    };
    assert_eq!(store.search(&query).await.unwrap().total, Some(30));
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_resource_without_an_ordering_value_sorts_last_ascending() {
    let Some((store, pool, namespace)) = support::fresh("ordered").await else {
        return;
    };
    store.create(patient("o1", "Stone", true)).await.unwrap();
    store
        .create(envelope("Patient", "o2", r#""active":true"#))
        .await
        .unwrap();
    let sorted = |direction| SearchQuery {
        sort: vec![SortKey {
            name: "name".to_owned(),
            target: fhir_core::search::lookup(Some(kind("Patient")), "name")
                .unwrap()
                .target
                .clone(),
            direction,
        }],
        ..SearchQuery::of_type(kind("Patient"))
    };
    assert_eq!(
        ids(&store
            .search(&sorted(SortDirection::Ascending))
            .await
            .unwrap()),
        vec!["o1", "o2"]
    );
    assert_eq!(
        ids(&store
            .search(&sorted(SortDirection::Descending))
            .await
            .unwrap()),
        vec!["o2", "o1"]
    );
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_search_reads_no_body_to_decide_what_matches() {
    let Some((store, pool, namespace)) = support::fresh("indexonly").await else {
        return;
    };
    store
        .create(observation("q1", "code-1", 4.5, "Patient/p1"))
        .await
        .unwrap();
    let statement = format!(
        "update {}.resource set body = decode('7b7d', 'hex') where resource_id = 'q1'",
        namespace.as_str()
    );
    sqlx::query(&statement).execute(&pool).await.unwrap();
    let query = SearchQuery {
        filters: vec![filter("Observation", "code", "code-1")],
        total: fhir_store::TotalMode::Accurate,
        count: 0,
        ..SearchQuery::of_type(kind("Observation"))
    };
    let page = store.search(&query).await.unwrap();
    assert_eq!(page.total, Some(1));
    assert!(page.entries.is_empty());
    support::drop_namespace(&pool, &namespace).await;
}

async fn count(pool: &PgPool, namespace: &str, table: &str) -> i64 {
    let statement = format!("select count(*) as total from {namespace}.{table}");
    sqlx::query(&statement)
        .fetch_one(pool)
        .await
        .unwrap()
        .get::<i64, _>("total")
}

#[tokio::test]
async fn dropping_a_parameter_takes_its_index_rows_with_it() {
    let Some((store, pool, namespace)) = support::fresh("dropped").await else {
        return;
    };
    store.create(banded("d1", "high")).await.unwrap();
    let spec = spec();
    store.index_parameter(&spec).await.unwrap();
    store.reindex(&[spec]).await.unwrap();
    let statement = format!(
        "select count(*) as total from {}.index_token where param = 'urn:p:band'",
        namespace.as_str()
    );
    let before: i64 = sqlx::query(&statement)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("total");
    assert!(before > 0);
    store.drop_parameter("urn:p:band").await.unwrap();
    let after: i64 = sqlx::query(&statement)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("total");
    assert_eq!(after, 0);
    assert!(count(&pool, namespace.as_str(), "index_token").await > 0);
    let _ = id("d1");
    support::drop_namespace(&pool, &namespace).await;
}
