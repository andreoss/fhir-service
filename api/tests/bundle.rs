use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    body: String,
}

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
}

async fn request(
    app: &Service,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::from(body.to_vec())).unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn post(app: &Service, bundle: &Value) -> (StatusCode, Value) {
    let reply = request(app, "POST", "/", &[], bundle.to_string().as_bytes()).await;
    let value = serde_json::from_str(&reply.body).unwrap_or(Value::Null);
    (reply.status, value)
}

fn patient(id: &str, active: bool) -> Value {
    json!({"resourceType": "Patient", "id": id, "active": active})
}

fn bundle(kind: &str, entries: Vec<Value>) -> Value {
    json!({"resourceType": "Bundle", "type": kind, "entry": entries})
}

fn write(method: &str, url: &str, resource: Value) -> Value {
    json!({"resource": resource, "request": {"method": method, "url": url}})
}

fn plain(method: &str, url: &str) -> Value {
    json!({"request": {"method": method, "url": url}})
}

fn statuses(value: &Value) -> Vec<String> {
    value["entry"]
        .as_array()
        .expect("a response bundle carries entries")
        .iter()
        .map(|entry| {
            entry["response"]["status"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn a_transaction_applies_every_entry() {
    let app = service();
    let sent = bundle(
        "transaction",
        vec![
            write("POST", "Patient", patient("tx-1", true)),
            write("POST", "Patient", patient("tx-2", false)),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["resourceType"], "Bundle");
    assert_eq!(body["type"], "transaction-response");
    assert_eq!(
        statuses(&body),
        vec!["201 Created".to_owned(), "201 Created".to_owned()]
    );
    assert_eq!(
        body["entry"][0]["response"]["location"],
        "Patient/tx-1/_history/1"
    );
    assert_eq!(body["entry"][0]["response"]["etag"], "W/\"1\"");
    assert_eq!(body["entry"][0]["resource"]["id"], "tx-1");
    assert_eq!(
        request(&app, "GET", "/Patient/tx-2", &[], &[]).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn one_failing_entry_rolls_the_whole_transaction_back() {
    let app = service();
    let sent = bundle(
        "transaction",
        vec![
            write("POST", "Patient", patient("tx-3", true)),
            write("POST", "Nonesuch", patient("tx-4", true)),
            write("POST", "Patient", patient("tx-5", true)),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["resourceType"], "OperationOutcome");
    assert_eq!(
        request(&app, "GET", "/Patient/tx-3", &[], &[]).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&app, "GET", "/Patient/tx-5", &[], &[]).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_transaction_that_fails_late_leaves_nothing_behind() {
    let app = service();
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        patient("tx-6", true).to_string().as_bytes(),
    )
    .await;
    let sent = bundle(
        "transaction",
        vec![
            write("PUT", "Patient/tx-6", patient("tx-6", false)),
            write("POST", "Patient", patient("tx-6", true)),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["resourceType"], "OperationOutcome", "{body}");
    assert_eq!(body["issue"][0]["code"], "duplicate", "{body}");
    let after = request(&app, "GET", "/Patient/tx-6", &[], &[]).await;
    let stored: Value = serde_json::from_str(&after.body).unwrap();
    assert_eq!(stored["active"], true);
    assert_eq!(stored["meta"]["versionId"], "1");
}

#[tokio::test]
async fn an_unknown_bundle_type_is_rejected() {
    let app = service();
    let (status, body) = post(&app, &bundle("collection", Vec::new())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["resourceType"], "OperationOutcome");
}

#[tokio::test]
async fn a_body_that_is_not_a_bundle_is_rejected() {
    let app = service();
    let (status, _) = post(&app, &patient("tx-7", true)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_nested_bundle_entry_is_rejected() {
    let app = service();
    let inner = bundle("batch", vec![plain("GET", "Patient/tx-8")]);
    let (status, _) = post(&app, &bundle("transaction", vec![write("POST", "", inner)])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_batch_reports_one_outcome_per_entry() {
    let app = service();
    let sent = bundle(
        "batch",
        vec![
            write("POST", "Patient", patient("ba-1", true)),
            write("POST", "Nonesuch", patient("ba-2", true)),
            write("POST", "Patient", patient("ba-3", true)),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["type"], "batch-response");
    let reported = statuses(&body);
    assert_eq!(reported[0], "201 Created");
    assert!(reported[1].starts_with("400"));
    assert_eq!(reported[2], "201 Created");
    assert_eq!(
        body["entry"][1]["outcome"]["resourceType"],
        "OperationOutcome"
    );
    assert!(body["entry"][1].get("resource").is_none());
    assert_eq!(
        request(&app, "GET", "/Patient/ba-1", &[], &[]).await.status,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", "/Patient/ba-3", &[], &[]).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_malformed_batch_entry_fails_only_itself() {
    let app = service();
    let sent = bundle(
        "batch",
        vec![
            json!({"request": {"method": "SING", "url": "Patient"}}),
            write("POST", "Patient", patient("ba-4", true)),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert!(statuses(&body)[0].starts_with("400"));
    assert_eq!(statuses(&body)[1], "201 Created");
    assert_eq!(
        request(&app, "GET", "/Patient/ba-4", &[], &[]).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_malformed_transaction_entry_fails_the_bundle() {
    let app = service();
    let sent = bundle(
        "transaction",
        vec![
            write("POST", "Patient", patient("ba-5", true)),
            json!({"request": {"method": "SING", "url": "Patient"}}),
        ],
    );
    let (status, _) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        request(&app, "GET", "/Patient/ba-5", &[], &[]).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_batch_leaves_earlier_entries_in_place_when_a_later_one_fails() {
    let app = service();
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        patient("ba-6", true).to_string().as_bytes(),
    )
    .await;
    let sent = bundle(
        "batch",
        vec![
            write("PUT", "Patient/ba-6", patient("ba-6", false)),
            write("POST", "Patient", patient("ba-6", true)),
        ],
    );
    let (_, body) = post(&app, &sent).await;
    assert_eq!(statuses(&body)[0], "200 OK");
    assert!(statuses(&body)[1].starts_with("409"));
    let after = request(&app, "GET", "/Patient/ba-6", &[], &[]).await;
    let stored: Value = serde_json::from_str(&after.body).unwrap();
    assert_eq!(stored["active"], false);
    assert_eq!(stored["meta"]["versionId"], "2");
}

fn named(id: &str, family: &str) -> Value {
    json!({"resourceType": "Patient", "id": id, "name": [{"family": family}], "active": true})
}

fn conditional(method: &str, url: &str, resource: Value, condition: &str) -> Value {
    json!({
        "resource": resource,
        "request": {"method": method, "url": url, "ifNoneExist": condition},
    })
}

#[tokio::test]
async fn a_conditional_create_entry_resolves_against_stored_state() {
    let app = service();
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        named("cd-1", "Stone").to_string().as_bytes(),
    )
    .await;
    let sent = bundle(
        "transaction",
        vec![conditional(
            "POST",
            "Patient",
            named("cd-2", "Stone"),
            "family=Stone",
        )],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(statuses(&body)[0], "200 OK");
    assert_eq!(body["entry"][0]["resource"]["id"], "cd-1");
    assert_eq!(
        request(&app, "GET", "/Patient/cd-2", &[], &[]).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_conditional_entry_sees_a_write_made_earlier_in_the_same_transaction() {
    let app = service();
    let sent = bundle(
        "transaction",
        vec![
            write("POST", "Patient", named("cd-3", "Rivers")),
            conditional("POST", "Patient", named("cd-4", "Rivers"), "family=Rivers"),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(statuses(&body)[1], "200 OK");
    assert_eq!(body["entry"][1]["resource"]["id"], "cd-3");
    assert_eq!(
        request(&app, "GET", "/Patient/cd-4", &[], &[]).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_conditional_update_entry_selects_the_resource_to_replace() {
    let app = service();
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        named("cd-5", "Brook").to_string().as_bytes(),
    )
    .await;
    let replacement =
        json!({"resourceType": "Patient", "name": [{"family": "Brook"}], "active": false});
    let sent = bundle(
        "transaction",
        vec![write("PUT", "Patient?family=Brook", replacement)],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(statuses(&body)[0], "200 OK");
    let stored: Value =
        serde_json::from_str(&request(&app, "GET", "/Patient/cd-5", &[], &[]).await.body).unwrap();
    assert_eq!(stored["active"], false);
    assert_eq!(stored["meta"]["versionId"], "2");
}

#[tokio::test]
async fn delete_entries_run_against_current_state() {
    let app = service();
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        named("cd-6", "Vale").to_string().as_bytes(),
    )
    .await;
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        named("cd-7", "Marsh").to_string().as_bytes(),
    )
    .await;
    let sent = bundle(
        "transaction",
        vec![
            plain("DELETE", "Patient/cd-6"),
            plain("DELETE", "Patient?family=Marsh"),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entry"].as_array().unwrap().len(), 2);
    assert_eq!(
        request(&app, "GET", "/Patient/cd-6", &[], &[]).await.status,
        StatusCode::GONE
    );
    assert_eq!(
        request(&app, "GET", "/Patient/cd-7", &[], &[]).await.status,
        StatusCode::GONE
    );
}

#[tokio::test]
async fn a_patch_entry_changes_the_selected_resource() {
    let app = service();
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        named("cd-8", "Ford").to_string().as_bytes(),
    )
    .await;
    let patch = json!([{"op": "replace", "path": "/active", "value": false}]);
    let sent = bundle("transaction", vec![write("PATCH", "Patient/cd-8", patch)]);
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(statuses(&body)[0], "200 OK");
    let stored: Value =
        serde_json::from_str(&request(&app, "GET", "/Patient/cd-8", &[], &[]).await.body).unwrap();
    assert_eq!(stored["active"], false);
}

#[tokio::test]
async fn a_search_entry_returns_a_result_set() {
    let app = service();
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        named("cd-9", "Quarry").to_string().as_bytes(),
    )
    .await;
    let sent = bundle("batch", vec![plain("GET", "Patient?family=Quarry")]);
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(statuses(&body)[0], "200 OK");
    assert_eq!(body["entry"][0]["resource"]["resourceType"], "Bundle");
    assert_eq!(body["entry"][0]["resource"]["type"], "searchset");
    assert_eq!(
        body["entry"][0]["resource"]["entry"][0]["resource"]["id"],
        "cd-9"
    );
}

#[tokio::test]
async fn a_search_entry_in_a_transaction_sees_the_uncommitted_writes() {
    let app = service();
    let sent = bundle(
        "transaction",
        vec![
            write("POST", "Patient", named("cd-10", "Hollow")),
            plain("GET", "Patient?family=Hollow"),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entry"][1]["resource"]["total"], 1);
}

fn observation(id: &str) -> Value {
    json!({
        "resourceType": "Observation",
        "id": id,
        "status": "final",
        "code": {"coding": [{"system": "urn:s", "code": "c1"}]},
    })
}

#[tokio::test]
async fn an_unauthorized_batch_entry_fails_only_that_entry() {
    let app = service();
    let sent = bundle(
        "batch",
        vec![
            write("POST", "Patient", patient("au-1", true)),
            write("POST", "Observation", observation("au-2")),
        ],
    );
    let reply = request(
        &app,
        "POST",
        "/",
        &[("x-scope", "types=Patient")],
        sent.to_string().as_bytes(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let body: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(statuses(&body)[0], "201 Created");
    assert!(statuses(&body)[1].starts_with("403"));
    assert_eq!(
        body["entry"][1]["outcome"]["resourceType"],
        "OperationOutcome"
    );
    assert_eq!(
        request(&app, "GET", "/Patient/au-1", &[], &[]).await.status,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", "/Observation/au-2", &[], &[])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn an_unauthorized_transaction_entry_fails_the_whole_bundle() {
    let app = service();
    let sent = bundle(
        "transaction",
        vec![
            write("POST", "Patient", patient("au-3", true)),
            write("POST", "Observation", observation("au-4")),
        ],
    );
    let reply = request(
        &app,
        "POST",
        "/",
        &[("x-scope", "types=Patient")],
        sent.to_string().as_bytes(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(
        request(&app, "GET", "/Patient/au-3", &[], &[]).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&app, "GET", "/Observation/au-4", &[], &[])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn an_open_scope_admits_every_entry() {
    let app = service();
    let sent = bundle(
        "transaction",
        vec![
            write("POST", "Patient", patient("au-5", true)),
            write("POST", "Observation", observation("au-6")),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        statuses(&body),
        vec!["201 Created".to_owned(), "201 Created".to_owned()]
    );
}

#[tokio::test]
async fn a_malformed_scope_rejects_the_bundle() {
    let app = service();
    let sent = bundle(
        "batch",
        vec![write("POST", "Patient", patient("au-7", true))],
    );
    let reply = request(
        &app,
        "POST",
        "/",
        &[("x-scope", "types")],
        sent.to_string().as_bytes(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_delete_entry_outside_the_scope_is_refused() {
    let app = service();
    request(
        &app,
        "POST",
        "/Observation",
        &[],
        observation("au-8").to_string().as_bytes(),
    )
    .await;
    let sent = bundle("batch", vec![plain("DELETE", "Observation/au-8")]);
    let reply = request(
        &app,
        "POST",
        "/",
        &[("x-scope", "types=Patient")],
        sent.to_string().as_bytes(),
    )
    .await;
    let body: Value = serde_json::from_str(&reply.body).unwrap();
    assert!(statuses(&body)[0].starts_with("403"));
    assert_eq!(
        request(&app, "GET", "/Observation/au-8", &[], &[])
            .await
            .status,
        StatusCode::OK
    );
}

use async_trait::async_trait;
use fhir_core::search::ParameterSpec;
use fhir_core::{Error, ResourceEnvelope, ResourceId, VersionId};
use fhir_store::{
    HistoryPage, HistoryQuery, HistoryScope, IndexReport, ResourceStore, SearchPage, SearchQuery,
    StoreScope,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Clone)]
struct Counter {
    live: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl Counter {
    fn new() -> Counter {
        Counter {
            live: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn entered(&self) {
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
    }

    fn left(&self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

struct Probe {
    inner: Arc<dyn ResourceStore>,
    counter: Counter,
}

#[async_trait]
impl ResourceStore for Probe {
    async fn create(&self, envelope: ResourceEnvelope) -> Result<ResourceEnvelope, Error> {
        self.counter.entered();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let stored = self.inner.create(envelope).await;
        self.counter.left();
        stored
    }

    async fn read(&self, id: &ResourceId) -> Result<ResourceEnvelope, Error> {
        self.inner.read(id).await
    }

    async fn vread(&self, id: &ResourceId, version: &VersionId) -> Result<ResourceEnvelope, Error> {
        self.inner.vread(id, version).await
    }

    async fn update(
        &self,
        envelope: ResourceEnvelope,
        expected_version: Option<&VersionId>,
    ) -> Result<ResourceEnvelope, Error> {
        self.inner.update(envelope, expected_version).await
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage, Error> {
        self.inner.search(query).await
    }

    async fn delete(&self, id: &ResourceId) -> Result<ResourceEnvelope, Error> {
        self.inner.delete(id).await
    }

    async fn hard_delete(&self, id: &ResourceId) -> Result<(), Error> {
        self.inner.hard_delete(id).await
    }

    async fn purge_history(&self, id: &ResourceId) -> Result<usize, Error> {
        self.inner.purge_history(id).await
    }

    async fn history(
        &self,
        scope: &HistoryScope,
        query: &HistoryQuery,
    ) -> Result<HistoryPage, Error> {
        self.inner.history(scope, query).await
    }

    async fn index_parameter(&self, spec: &ParameterSpec) -> Result<IndexReport, Error> {
        self.inner.index_parameter(spec).await
    }

    async fn begin(&self) -> Result<Arc<dyn StoreScope>, Error> {
        let inner = self.inner.begin().await?;
        Ok(Arc::new(ProbeScope {
            inner,
            counter: self.counter.clone(),
        }))
    }

    async fn health(&self) -> Result<(), Error> {
        self.inner.health().await
    }
}

struct ProbeScope {
    inner: Arc<dyn StoreScope>,
    counter: Counter,
}

#[async_trait]
impl StoreScope for ProbeScope {
    fn store(&self) -> Arc<dyn ResourceStore> {
        Arc::new(Probe {
            inner: self.inner.store(),
            counter: self.counter.clone(),
        })
    }

    async fn commit(&self) -> Result<(), Error> {
        self.inner.commit().await
    }

    async fn rollback(&self) -> Result<(), Error> {
        self.inner.rollback().await
    }
}

fn watched(limit: usize) -> (Service, Counter) {
    let counter = Counter::new();
    let inner: Arc<dyn ResourceStore> = Arc::new(MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    })));
    let store = Arc::new(Probe {
        inner,
        counter: counter.clone(),
    });
    let service = Service::new(store, FhirVersion::R4, Vec::new()).with_entries(limit);
    (service, counter)
}

fn creates(count: usize, prefix: &str) -> Vec<Value> {
    (0..count)
        .map(|number| {
            write(
                "POST",
                "Patient",
                patient(&format!("{prefix}-{number}"), true),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_batch_admits_only_the_permitted_number_of_entries_at_once() {
    let (app, counter) = watched(2);
    let (status, body) = post(&app, &bundle("batch", creates(6, "or"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(statuses(&body).len(), 6);
    assert!(statuses(&body).iter().all(|status| status == "201 Created"));
    assert!(counter.peak() <= 2, "peak {}", counter.peak());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_batch_runs_entries_side_by_side() {
    let (app, counter) = watched(8);
    let (status, _) = post(&app, &bundle("batch", creates(6, "os"))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(counter.peak() > 1, "peak {}", counter.peak());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_batch_answers_in_the_order_it_was_asked() {
    let (app, _) = watched(4);
    let (_, body) = post(&app, &bundle("batch", creates(5, "ot"))).await;
    let located: Vec<String> = body["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            entry["response"]["location"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    let expected: Vec<String> = (0..5)
        .map(|number| format!("Patient/ot-{number}/_history/1"))
        .collect();
    assert_eq!(located, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transaction_runs_its_entries_one_at_a_time() {
    let (app, counter) = watched(8);
    let (status, _) = post(&app, &bundle("transaction", creates(4, "ou"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(counter.peak(), 1);
}

#[tokio::test]
async fn a_transaction_deletes_before_it_writes_whatever_order_it_was_written_in() {
    let app = service();
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        patient("tx-order-1", true).to_string().as_bytes(),
    )
    .await;
    let sent = bundle(
        "transaction",
        vec![
            write("PUT", "Patient/tx-order-1", patient("tx-order-1", false)),
            plain("DELETE", "Patient/tx-order-1"),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let after = request(&app, "GET", "/Patient/tx-order-1", &[], &[]).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "the write runs after the delete: {}",
        after.body
    );
    let stored: Value = serde_json::from_str(&after.body).unwrap();
    assert_eq!(stored["active"], false, "{}", after.body);
}

#[tokio::test]
async fn a_transaction_updates_what_a_later_entry_created() {
    let app = service();
    let sent = bundle(
        "transaction",
        vec![
            write("PUT", "Patient/tx-order-2", patient("tx-order-2", false)),
            write("POST", "Patient", patient("tx-order-2", true)),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let after = request(&app, "GET", "/Patient/tx-order-2", &[], &[]).await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    let stored: Value = serde_json::from_str(&after.body).unwrap();
    assert_eq!(
        stored["active"], false,
        "the update runs after the create: {}",
        after.body
    );
    assert_eq!(stored["meta"]["versionId"], "2", "{}", after.body);
}

fn placed(method: &str, url: &str, resource: Value, full_url: &str) -> Value {
    json!({"fullUrl": full_url, "resource": resource, "request": {"method": method, "url": url}})
}

#[tokio::test]
async fn a_transaction_replaces_a_placeholder_with_the_reference_it_assigned() {
    let app = service();
    let held = "urn:uuid:2c6f9a1e-1111-4111-8111-111111111111";
    let subject = json!({
        "resourceType": "Observation",
        "id": "ph-2",
        "status": "final",
        "code": {"coding": [{"system": "urn:s", "code": "c1"}]},
        "subject": {"reference": held},
    });
    let sent = bundle(
        "transaction",
        vec![
            placed("POST", "Patient", patient("ph-1", true), held),
            write("POST", "Observation", subject.clone()),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["entry"][0]["response"]["location"], "Patient/ph-1/_history/1",
        "{body}"
    );
    assert_eq!(
        body["entry"][1]["resource"]["subject"]["reference"], "Patient/ph-1",
        "{body}"
    );
    let read = request(&app, "GET", "/Observation/ph-2", &[], &[]).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    let stored: Value = serde_json::from_str(&read.body).unwrap();
    assert_eq!(
        stored["subject"]["reference"], "Patient/ph-1",
        "{}",
        read.body
    );
}

#[tokio::test]
async fn a_placeholder_inside_a_narrative_and_a_fragmented_url_are_replaced() {
    let app = service();
    let place = "urn:uuid:2c6f9a1e-2222-4222-8222-222222222222";
    let carrying = json!({
        "resourceType": "Observation",
        "id": "ph-3",
        "status": "final",
        "code": {"coding": [{"system": "urn:s", "code": "c1"}]},
        "text": {"status": "generated", "div": format!("<div xmlns=\"http://www.w3.org/1999/xhtml\"><img src=\"{place}\"/></div>")},
        "extension": [{"url": "http://example.org/x", "valueUri": format!("{place}#a")}],
    });
    let sent = bundle(
        "transaction",
        vec![
            placed("POST", "Patient", patient("ph-4", true), place),
            write("POST", "Observation", carrying),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["entry"][1]["resource"]["text"]["div"],
        "<div xmlns=\"http://www.w3.org/1999/xhtml\"><img src=\"Patient/ph-4\"/></div>",
        "{body}"
    );
    assert_eq!(
        body["entry"][1]["resource"]["extension"][0]["valueUri"], "Patient/ph-4#a",
        "{body}"
    );
}

#[tokio::test]
async fn a_batch_replaces_a_placeholder_with_the_reference_it_assigned() {
    let app = service();
    let place = "urn:uuid:2c6f9a1e-3333-4333-8333-333333333333";
    let subject = json!({
        "resourceType": "Observation",
        "id": "ph-5",
        "status": "final",
        "code": {"coding": [{"system": "urn:s", "code": "c1"}]},
        "subject": {"reference": place},
    });
    let sent = bundle(
        "batch",
        vec![
            placed("POST", "Patient", patient("ph-6", true), place),
            write("POST", "Observation", subject),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["entry"][1]["resource"]["subject"]["reference"], "Patient/ph-6",
        "{body}"
    );
}

#[tokio::test]
async fn a_read_entry_runs_after_every_write_in_the_same_transaction() {
    let app = service();
    let sent = bundle(
        "transaction",
        vec![
            plain("GET", "Patient/tx-order-3"),
            write("POST", "Patient", patient("tx-order-3", true)),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(statuses(&body)[0], "200 OK", "{body}");
    assert_eq!(statuses(&body)[1], "201 Created", "{body}");
}
