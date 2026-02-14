use fhir_adapter_memory::MemoryStore;
use fhir_core::{Error, FhirInstant, FhirVersion, InstantPeriod, ResourceEnvelope, ResourceId, VersionId};
use fhir_core::search::{lookup, Filter};
use fhir_store::{HistoryOrder, HistoryPage, HistoryQuery, HistoryScope, ResourceStore, SearchQuery};
use std::sync::Arc;

fn store() -> MemoryStore {
    MemoryStore::with_clock(Arc::new(|| FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()))
}

fn envelope(version: FhirVersion, id: &str, active: bool) -> ResourceEnvelope {
    let bytes = format!(
        r#"{{"resourceType":"Patient","id":"{id}","meta":{{"versionId":"0","lastUpdated":"2026-09-06T04:00:00Z"}},"active":{active}}}"#
    )
    .into_bytes();
    ResourceEnvelope::parse(version, &bytes).unwrap()
}

fn id(value: &str) -> ResourceId {
    ResourceId::parse(value).unwrap()
}

fn observation(id: &str) -> ResourceEnvelope {
    let bytes = format!(
        r#"{{"resourceType":"Observation","id":"{id}","meta":{{"versionId":"0","lastUpdated":"2026-09-06T04:00:00Z"}},"status":"final"}}"#
    )
    .into_bytes();
    ResourceEnvelope::parse(FhirVersion::R4, &bytes).unwrap()
}

fn list(id: &str, members: &[&str]) -> ResourceEnvelope {
    let entries: Vec<String> = members
        .iter()
        .map(|reference| format!(r#"{{"item":{{"reference":"{reference}"}}}}"#))
        .collect();
    let bytes = format!(
        r#"{{"resourceType":"List","id":"{id}","meta":{{"versionId":"0","lastUpdated":"2026-09-06T04:00:00Z"}},"status":"current","mode":"working","entry":[{}]}}"#,
        entries.join(",")
    )
    .into_bytes();
    ResourceEnvelope::parse(FhirVersion::R4, &bytes).unwrap()
}

fn query(params: &[(&str, &str)]) -> SearchQuery {
    let resource_type = "Patient".parse().unwrap();
    let filters = params
        .iter()
        .map(|(name, value)| {
            let def = lookup(Some(resource_type), name).expect("parameter is registered");
            Filter::new(name, def.target.clone(), vec![def.value(value).unwrap()])
        })
        .collect();
    SearchQuery {
        types: vec![resource_type],
        filters,
        ..SearchQuery::default()
    }
}

fn version(value: &str) -> VersionId {
    VersionId::parse(value).unwrap()
}

#[tokio::test]
async fn health_reports_alive() {
    let store = store();
    assert_eq!(store.health().await, Ok(()));
}

#[tokio::test]
async fn create_read_round_trip_over_every_version() {
    for version in FhirVersion::ALL {
        let store = store();
        let created = store.create(envelope(version, "pt-1", true)).await.unwrap();
        assert_eq!(created.version_id().as_str(), "1");
        assert_eq!(created.version(), version);

        let read = store.read(&id("pt-1")).await.unwrap();
        assert_eq!(read.version_id().as_str(), "1");
        assert_eq!(read.raw(), created.raw());
        assert_eq!(read.version(), version);
    }
}

#[tokio::test]
async fn create_rejects_duplicate_id() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    let error = store.create(envelope(FhirVersion::R4, "pt-1", false)).await.unwrap_err();
    assert!(matches!(error, Error::Duplicate(_)));
    assert_eq!(error.http_status(), 409);
}

#[tokio::test]
async fn read_unknown_id_rejected() {
    let store = store();
    let error = store.read(&id("nobody")).await.unwrap_err();
    assert!(matches!(error, Error::NotFound));
    assert_eq!(error.http_status(), 404);
}

#[tokio::test]
async fn vread_returns_every_historical_version() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-2", true)).await.unwrap();
    let updated = store.update(envelope(FhirVersion::R4, "pt-2", false), Some(&version("1"))).await.unwrap();
    assert_eq!(updated.version_id().as_str(), "2");

    let v1 = store.vread(&id("pt-2"), &version("1")).await.unwrap();
    assert_eq!(v1.version_id().as_str(), "1");
    let old = std::str::from_utf8(v1.raw()).unwrap();
    assert!(old.contains("\"active\":true"));

    let v2 = store.vread(&id("pt-2"), &version("2")).await.unwrap();
    assert_eq!(v2.version_id().as_str(), "2");
    assert!(std::str::from_utf8(v2.raw()).unwrap().contains("\"active\":false"));

    let current = store.read(&id("pt-2")).await.unwrap();
    assert_eq!(current.version_id().as_str(), "2");
}

#[tokio::test]
async fn vread_unknown_id_and_version_rejected() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-3", true)).await.unwrap();

    let missing_id = store.vread(&id("nobody"), &version("1")).await.unwrap_err();
    assert!(matches!(missing_id, Error::NotFound));

    let missing_version = store.vread(&id("pt-3"), &version("99")).await.unwrap_err();
    assert!(matches!(missing_version, Error::NotFound));
    assert_eq!(missing_version.http_status(), 404);
}

#[tokio::test]
async fn update_with_expected_version_creates_new_version() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-4", true)).await.unwrap();
    let updated = store.update(envelope(FhirVersion::R4, "pt-4", false), Some(&version("1"))).await.unwrap();
    assert_eq!(updated.version_id().as_str(), "2");
    let current = store.read(&id("pt-4")).await.unwrap();
    assert_eq!(current.version_id().as_str(), "2");
}

#[tokio::test]
async fn stale_expected_version_conflicts() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-5", true)).await.unwrap();
    store.update(envelope(FhirVersion::R4, "pt-5", false), Some(&version("1"))).await.unwrap();
    let error = store.update(envelope(FhirVersion::R4, "pt-5", false), Some(&version("1"))).await.unwrap_err();
    assert!(matches!(error, Error::VersionConflict));
    assert_eq!(error.http_status(), 409);
}

#[tokio::test]
async fn noop_update_creates_no_version() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-6", true)).await.unwrap();
    let current = store.read(&id("pt-6")).await.unwrap();

    let first = store.update(current.clone(), Some(current.version_id())).await.unwrap();
    assert_eq!(first.version_id().as_str(), "1");

    let second = store.update(envelope(FhirVersion::R4, "pt-6", true), None).await.unwrap();
    assert_eq!(second.version_id().as_str(), "1");
    assert_eq!(second.raw(), current.raw());

    let read = store.read(&id("pt-6")).await.unwrap();
    assert_eq!(read.version_id().as_str(), "1");
}

#[tokio::test]
async fn update_without_expected_version_changes_content() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-7", true)).await.unwrap();
    let updated = store.update(envelope(FhirVersion::R4, "pt-7", false), None).await.unwrap();
    assert_eq!(updated.version_id().as_str(), "2");
}

#[tokio::test]
async fn update_unknown_id_rejected() {
    let store = store();
    let error = store.update(envelope(FhirVersion::R4, "nobody", true), None).await.unwrap_err();
    assert!(matches!(error, Error::NotFound));
    assert_eq!(error.http_status(), 404);
}

#[tokio::test]
async fn concurrent_updates_with_same_expected_version_one_conflicts() {
    let store = Arc::new(store());
    store.create(envelope(FhirVersion::R4, "pt-8", true)).await.unwrap();

    let first = Arc::clone(&store);
    let second = Arc::clone(&store);
    let winner = tokio::spawn(async move {
        first.update(envelope(FhirVersion::R4, "pt-8", false), Some(&version("1"))).await
    });
    let loser = tokio::spawn(async move {
        second.update(envelope(FhirVersion::R4, "pt-8", false), Some(&version("1"))).await
    });
    let (winner, loser) = tokio::join!(winner, loser);

    let outcomes = [winner.unwrap().err().map(|_| ()), loser.unwrap().err().map(|_| ())];
    assert_eq!(outcomes.iter().filter(|o| o.is_none()).count(), 1, "one update must succeed");
    assert_eq!(outcomes.iter().filter(|o| o.is_some()).count(), 1, "one update must conflict");
}

#[tokio::test]
async fn history_reaches_final_state_after_update_chain() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-9", true)).await.unwrap();
    store.update(envelope(FhirVersion::R4, "pt-9", false), Some(&version("1"))).await.unwrap();
    let v3 = store.update(envelope(FhirVersion::R4, "pt-9", true), Some(&version("2"))).await.unwrap();
    assert_eq!(v3.version_id().as_str(), "3");
    assert_eq!(store.vread(&id("pt-9"), &version("3")).await.unwrap().version_id().as_str(), "3");
    assert_eq!(store.vread(&id("pt-9"), &version("2")).await.unwrap().version_id().as_str(), "2");
    assert_eq!(store.vread(&id("pt-9"), &version("1")).await.unwrap().version_id().as_str(), "1");
}

#[tokio::test]
async fn search_without_parameters_returns_every_current_resource() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    store.create(envelope(FhirVersion::R4, "pt-2", false)).await.unwrap();
    let page = store.search(&SearchQuery::default()).await.unwrap();
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.total, Some(2));
    assert_eq!(page.offset, 0);
}

#[tokio::test]
async fn search_matches_a_registered_parameter() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    store.create(envelope(FhirVersion::R4, "pt-2", false)).await.unwrap();
    let page = store.search(&query(&[("active", "true")])).await.unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].id().as_str(), "pt-1");
}

#[tokio::test]
async fn search_matches_the_resource_id() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    store.create(envelope(FhirVersion::R4, "pt-2", true)).await.unwrap();
    let page = store.search(&query(&[("_id", "pt-2")])).await.unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].id().as_str(), "pt-2");
}

#[tokio::test]
async fn search_restricted_to_a_type_ignores_other_types() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    store.create(observation("ob-1")).await.unwrap();
    let patients = store
        .search(&SearchQuery::of_type("Patient".parse().unwrap()))
        .await
        .unwrap();
    assert_eq!(patients.entries.len(), 1);
    assert_eq!(patients.entries[0].resource_type().as_str(), "Patient");
}

#[tokio::test]
async fn search_reports_no_match_for_an_absent_element() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    assert!(store.search(&query(&[("gender", "female")])).await.unwrap().entries.is_empty());
}

#[tokio::test]
async fn search_sees_the_current_version_only() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    store.update(envelope(FhirVersion::R4, "pt-1", false), None).await.unwrap();
    assert!(store.search(&query(&[("active", "true")])).await.unwrap().entries.is_empty());
    assert_eq!(store.search(&query(&[("active", "false")])).await.unwrap().entries.len(), 1);
}

#[tokio::test]
async fn search_filters_are_conjunctive() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    store.create(envelope(FhirVersion::R4, "pt-2", true)).await.unwrap();
    let both = query(&[("active", "true"), ("_id", "pt-2")]);
    assert_eq!(store.search(&both).await.unwrap().entries.len(), 1);
    let neither = query(&[("active", "false"), ("_id", "pt-2")]);
    assert!(store.search(&neither).await.unwrap().entries.is_empty());
}

#[tokio::test]
async fn search_selects_the_members_of_a_list() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    store.create(envelope(FhirVersion::R4, "pt-2", true)).await.unwrap();
    store.create(list("ls-1", &["Patient/pt-2"])).await.unwrap();
    let mut selection = SearchQuery::of_type("Patient".parse().unwrap());
    selection.list = Some(id("ls-1"));
    let page = store.search(&selection).await.unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].id().as_str(), "pt-2");
    let mut unknown = SearchQuery::of_type("Patient".parse().unwrap());
    unknown.list = Some(id("ls-none"));
    assert!(store.search(&unknown).await.unwrap().entries.is_empty());
}

#[tokio::test]
async fn a_deleted_list_selects_nothing() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-1", true)).await.unwrap();
    store.create(list("ls-2", &["Patient/pt-1"])).await.unwrap();
    store.delete(&id("ls-2")).await.unwrap();
    let mut selection = SearchQuery::of_type("Patient".parse().unwrap());
    selection.list = Some(id("ls-2"));
    assert!(store.search(&selection).await.unwrap().entries.is_empty());
}

#[tokio::test]
async fn delete_appends_a_marker_version() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d1", true)).await.unwrap();
    let marker = store.delete(&id("pt-d1")).await.unwrap();
    assert!(marker.is_deleted());
    assert_eq!(marker.version_id().as_str(), "2");
    let current = store.read(&id("pt-d1")).await.unwrap();
    assert!(current.is_deleted());
}

#[tokio::test]
async fn delete_leaves_earlier_versions_readable() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d2", true)).await.unwrap();
    store.delete(&id("pt-d2")).await.unwrap();
    let first = store.vread(&id("pt-d2"), &version("1")).await.unwrap();
    assert!(!first.is_deleted());
    assert_eq!(first.version_id().as_str(), "1");
}

#[tokio::test]
async fn delete_of_an_unknown_id_is_not_found() {
    let store = store();
    assert_eq!(store.delete(&id("pt-none")).await.unwrap_err(), Error::NotFound);
}

#[tokio::test]
async fn deleting_twice_reports_the_resource_as_deleted() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d3", true)).await.unwrap();
    store.delete(&id("pt-d3")).await.unwrap();
    assert_eq!(store.delete(&id("pt-d3")).await.unwrap_err(), Error::Deleted);
}

#[tokio::test]
async fn a_deleted_resource_is_invisible_to_search() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d4", true)).await.unwrap();
    store.delete(&id("pt-d4")).await.unwrap();
    assert!(store.search(&SearchQuery::default()).await.unwrap().entries.is_empty());
}

#[tokio::test]
async fn updating_a_deleted_resource_restores_it() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d5", true)).await.unwrap();
    store.delete(&id("pt-d5")).await.unwrap();
    let restored = store.update(envelope(FhirVersion::R4, "pt-d5", true), None).await.unwrap();
    assert!(!restored.is_deleted());
    assert_eq!(restored.version_id().as_str(), "3");
    assert_eq!(store.search(&SearchQuery::default()).await.unwrap().entries.len(), 1);
}

#[tokio::test]
async fn hard_delete_removes_every_version() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d6", true)).await.unwrap();
    store.update(envelope(FhirVersion::R4, "pt-d6", false), None).await.unwrap();
    store.hard_delete(&id("pt-d6")).await.unwrap();
    assert_eq!(store.read(&id("pt-d6")).await.unwrap_err(), Error::NotFound);
    assert_eq!(store.vread(&id("pt-d6"), &version("1")).await.unwrap_err(), Error::NotFound);
}

#[tokio::test]
async fn hard_delete_of_an_unknown_id_is_not_found() {
    let store = store();
    assert_eq!(store.hard_delete(&id("pt-none")).await.unwrap_err(), Error::NotFound);
}

#[tokio::test]
async fn purge_history_keeps_the_current_version_only() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-d7", true)).await.unwrap();
    store.update(envelope(FhirVersion::R4, "pt-d7", false), None).await.unwrap();
    let purged = store.purge_history(&id("pt-d7")).await.unwrap();
    assert_eq!(purged, 1);
    assert_eq!(store.read(&id("pt-d7")).await.unwrap().version_id().as_str(), "2");
    assert_eq!(store.vread(&id("pt-d7"), &version("1")).await.unwrap_err(), Error::NotFound);
    assert_eq!(store.purge_history(&id("pt-d7")).await.unwrap(), 0);
}

#[tokio::test]
async fn purge_history_of_an_unknown_id_is_not_found() {
    let store = store();
    assert_eq!(store.purge_history(&id("pt-none")).await.unwrap_err(), Error::NotFound);
}

fn ticking_store() -> MemoryStore {
    let tick = Arc::new(std::sync::atomic::AtomicU32::new(0));
    MemoryStore::with_clock(Arc::new(move || {
        let second = tick.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        FhirInstant::parse(&format!("2026-09-06T04:00:{second:02}Z")).unwrap()
    }))
}


fn versions(page: &HistoryPage) -> Vec<String> {
    page.entries
        .iter()
        .map(|entry| format!("{}/{}", entry.id().as_str(), entry.version_id().as_str()))
        .collect()
}

async fn seeded() -> MemoryStore {
    let store = ticking_store();
    store.create(envelope(FhirVersion::R4, "pt-h1", true)).await.unwrap();
    store.update(envelope(FhirVersion::R4, "pt-h1", false), None).await.unwrap();
    store.delete(&id("pt-h1")).await.unwrap();
    store.create(observation("ob-h1")).await.unwrap();
    store
}

#[tokio::test]
async fn instance_history_is_newest_first_and_keeps_the_delete_marker() {
    let store = seeded().await;
    let scope = HistoryScope::Instance("Patient".parse().unwrap(), id("pt-h1"));
    let page = store.history(&scope, &HistoryQuery::default()).await.unwrap();
    assert_eq!(versions(&page), ["pt-h1/3", "pt-h1/2", "pt-h1/1"]);
    assert_eq!(page.total, 3);
    assert!(page.entries[0].is_deleted());
}

#[tokio::test]
async fn oldest_first_reverses_the_order() {
    let store = seeded().await;
    let scope = HistoryScope::Instance("Patient".parse().unwrap(), id("pt-h1"));
    let query = HistoryQuery { order: HistoryOrder::Oldest, ..HistoryQuery::default() };
    let page = store.history(&scope, &query).await.unwrap();
    assert_eq!(versions(&page), ["pt-h1/1", "pt-h1/2", "pt-h1/3"]);
}

#[tokio::test]
async fn type_scope_covers_one_type_and_system_scope_covers_all() {
    let store = seeded().await;
    let typed = store
        .history(&HistoryScope::Type("Patient".parse().unwrap()), &HistoryQuery::default())
        .await
        .unwrap();
    assert_eq!(typed.total, 3);
    assert!(typed.entries.iter().all(|entry| entry.id().as_str() == "pt-h1"));
    let system = store.history(&HistoryScope::System, &HistoryQuery::default()).await.unwrap();
    assert_eq!(system.total, 4);
    assert_eq!(versions(&system)[0], "ob-h1/1");
    let empty = store
        .history(&HistoryScope::Type("Encounter".parse().unwrap()), &HistoryQuery::default())
        .await
        .unwrap();
    assert_eq!(empty.total, 0);
    assert!(empty.entries.is_empty());
}

#[tokio::test]
async fn time_filters_select_versions_by_write_time() {
    let store = seeded().await;
    let since = HistoryQuery {
        since: Some(InstantPeriod::parse("2026-09-06T04:00:02Z").unwrap()),
        ..HistoryQuery::default()
    };
    assert_eq!(versions(&store.history(&HistoryScope::System, &since).await.unwrap()), ["ob-h1/1", "pt-h1/3"]);

    let before = HistoryQuery {
        before: Some(InstantPeriod::parse("2026-09-06T04:00:01Z").unwrap()),
        ..HistoryQuery::default()
    };
    assert_eq!(versions(&store.history(&HistoryScope::System, &before).await.unwrap()), ["pt-h1/1"]);

    let at = HistoryQuery {
        at: Some(InstantPeriod::parse("2026-09-06T04:00:01Z").unwrap()),
        ..HistoryQuery::default()
    };
    assert_eq!(versions(&store.history(&HistoryScope::System, &at).await.unwrap()), ["pt-h1/2"]);

    let day = HistoryQuery {
        at: Some(InstantPeriod::parse("2026-09-06").unwrap()),
        ..HistoryQuery::default()
    };
    assert_eq!(store.history(&HistoryScope::System, &day).await.unwrap().total, 4);
}

#[tokio::test]
async fn paging_reports_the_total_beyond_the_page() {
    let store = seeded().await;
    let first = HistoryQuery { count: 2, ..HistoryQuery::default() };
    let page = store.history(&HistoryScope::System, &first).await.unwrap();
    assert_eq!(versions(&page), ["ob-h1/1", "pt-h1/3"]);
    assert_eq!(page.total, 4);
    assert_eq!(page.offset, 0);

    let second = HistoryQuery { count: 2, offset: 2, ..HistoryQuery::default() };
    let page = store.history(&HistoryScope::System, &second).await.unwrap();
    assert_eq!(versions(&page), ["pt-h1/2", "pt-h1/1"]);
    assert_eq!(page.total, 4);
    assert_eq!(page.offset, 2);

    let past_end = HistoryQuery { count: 2, offset: 9, ..HistoryQuery::default() };
    let page = store.history(&HistoryScope::System, &past_end).await.unwrap();
    assert!(page.entries.is_empty());
    assert_eq!(page.total, 4);

    let none = HistoryQuery { count: 0, ..HistoryQuery::default() };
    let page = store.history(&HistoryScope::System, &none).await.unwrap();
    assert!(page.entries.is_empty());
    assert_eq!(page.total, 4);
}

#[tokio::test]
async fn instance_history_of_an_unknown_id_is_not_found() {
    let store = seeded().await;
    let scope = HistoryScope::Instance("Patient".parse().unwrap(), id("pt-none"));
    assert_eq!(store.history(&scope, &HistoryQuery::default()).await, Err(Error::NotFound));
    let mismatch = HistoryScope::Instance("Observation".parse().unwrap(), id("pt-h1"));
    assert_eq!(store.history(&mismatch, &HistoryQuery::default()).await, Err(Error::NotFound));
}

fn value_set(id: &str, url: &str, codes: &[&str]) -> ResourceEnvelope {
    let concepts: Vec<String> = codes
        .iter()
        .map(|code| format!(r#"{{"code":"{code}"}}"#))
        .collect();
    let bytes = format!(
        r#"{{"resourceType":"ValueSet","id":"{id}","meta":{{"versionId":"0","lastUpdated":"2026-09-06T04:00:00Z"}},"url":"{url}","status":"active","compose":{{"include":[{{"system":"urn:s","concept":[{}]}}]}}}}"#,
        concepts.join(",")
    )
    .into_bytes();
    ResourceEnvelope::parse(FhirVersion::R4, &bytes).unwrap()
}

fn coded_observation(id: &str, code: &str) -> ResourceEnvelope {
    let bytes = format!(
        r#"{{"resourceType":"Observation","id":"{id}","meta":{{"versionId":"0","lastUpdated":"2026-09-06T04:00:00Z"}},"status":"final","code":{{"coding":[{{"system":"urn:s","code":"{code}"}}]}}}}"#
    )
    .into_bytes();
    ResourceEnvelope::parse(FhirVersion::R4, &bytes).unwrap()
}

fn coded_query(modifier: fhir_core::search::Modifier, url: &str) -> SearchQuery {
    let resource_type: fhir_core::ResourceType = "Observation".parse().unwrap();
    let def = lookup(Some(resource_type), "code").unwrap();
    let filter = Filter {
        name: "code".to_owned(),
        target: def.target.clone(),
        values: vec![def.value_with(&modifier, url).unwrap()],
        modifier,
        index: None,
    };
    SearchQuery {
        types: vec![resource_type],
        filters: vec![filter],
        ..SearchQuery::default()
    }
}

#[tokio::test]
async fn a_code_set_membership_filter_is_expanded_by_the_store() {
    let store = store();
    store.create(value_set("vs-1", "http://x/vs", &["a", "b"])).await.unwrap();
    store.create(coded_observation("ob-1", "a")).await.unwrap();
    store.create(coded_observation("ob-2", "z")).await.unwrap();
    let inside = store
        .search(&coded_query(fhir_core::search::Modifier::In, "http://x/vs"))
        .await
        .unwrap();
    assert_eq!(inside.entries.len(), 1);
    assert_eq!(inside.entries[0].id().as_str(), "ob-1");
    let outside = store
        .search(&coded_query(fhir_core::search::Modifier::NotIn, "http://x/vs"))
        .await
        .unwrap();
    assert_eq!(outside.entries.len(), 1);
    assert_eq!(outside.entries[0].id().as_str(), "ob-2");
}

#[tokio::test]
async fn an_unknown_code_set_is_an_invalid_parameter() {
    let store = store();
    store.create(coded_observation("ob-1", "a")).await.unwrap();
    let error = store
        .search(&coded_query(fhir_core::search::Modifier::In, "http://x/none"))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::InvalidParameter(_)));
}

fn by_id(values: &[&str]) -> SearchQuery {
    let resource_type: fhir_core::ResourceType = "Patient".parse().unwrap();
    let def = lookup(Some(resource_type), "_id").unwrap();
    let values = values.iter().map(|value| def.value(value).unwrap()).collect();
    SearchQuery {
        types: vec![resource_type],
        filters: vec![Filter::new("_id", def.target.clone(), values)],
        ..SearchQuery::default()
    }
}

#[tokio::test]
async fn a_repeated_query_reuses_one_cached_plan() {
    let store = store();
    for name in ["pt-p1", "pt-p2"] {
        store.create(envelope(FhirVersion::R4, name, true)).await.unwrap();
    }
    let query = by_id(&["pt-p1"]);
    for _ in 0..3 {
        assert_eq!(store.search(&query).await.unwrap().entries.len(), 1);
    }
    let plans = store.plans();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].uses, 3);
    assert!(plans[0].indexed);
    assert!(!plans[0].disabled);
}

#[tokio::test]
async fn a_plan_that_regresses_is_disabled_and_results_stay_right() {
    let store = store();
    let mut names = Vec::new();
    for index in 0..40 {
        let name = format!("pt-q{index}");
        store.create(envelope(FhirVersion::R4, &name, true)).await.unwrap();
        names.push(name);
    }
    assert_eq!(store.search(&by_id(&["pt-q0"])).await.unwrap().entries.len(), 1);
    let wide: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(store.search(&by_id(&wide)).await.unwrap().entries.len(), 40);
    assert!(store.plans()[0].disabled);
    let page = store.search(&by_id(&["pt-q7"])).await.unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].id().as_str(), "pt-q7");
}

#[tokio::test]
async fn a_repeated_filter_is_simplified_away() {
    let store = store();
    store.create(envelope(FhirVersion::R4, "pt-s9", true)).await.unwrap();
    let one = by_id(&["pt-s9"]);
    let mut twice = one.clone();
    twice.filters.push(twice.filters[0].clone());
    assert_eq!(twice.simplified().filters.len(), 1);
    assert_eq!(store.search(&twice).await.unwrap().entries.len(), 1);
    assert_eq!(store.plans().len(), 1);
}

#[tokio::test]
async fn the_shared_contract_holds_over_this_adapter() {
    fhir_store_contract::lifecycle(&store()).await;
    fhir_store_contract::versioning(&store()).await;
    fhir_store_contract::removal(&store()).await;
    fhir_store_contract::record(&store()).await;
    fhir_store_contract::readiness(&store()).await;
    fhir_store_contract::atomicity(&store()).await;
    fhir_store_contract::scoped_search(&store()).await;
}

#[tokio::test]
async fn the_shared_search_contract_holds_over_this_adapter() {
    fhir_store_contract::search::selection(&store()).await;
    fhir_store_contract::search::qualifiers(&store()).await;
    fhir_store_contract::search::ordering(&store()).await;
    fhir_store_contract::search::linking(&store()).await;
    fhir_store_contract::search::composites(&store()).await;
    fhir_store_contract::search::targeted_index(&store()).await;
}
