use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use fhir_core::search::Grant;
use fhir_core::{Error, ResourceType};
use http_body_util::BodyExt;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tower::ServiceExt;

use crate::app::AppState;
use crate::handlers::AppError;
use crate::preference::Return;
use fhir_core::security::scope::DataAction;
use fhir_core::security::Access;

const FHIR_JSON: &str = "application/fhir+json";
const BUNDLE: &str = "Bundle";
const OUTCOME: &str = "OperationOutcome";
const TRANSACTION: &str = "transaction";
const BATCH: &str = "batch";
const DOCUMENT: &str = "document";
const IF_NONE_EXIST: &str = "if-none-exist";
const SCOPE: &str = "x-scope";

pub const ENTRIES_AT_ONCE: usize = 8;

pub async fn process(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let value: Value =
        serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let incoming = Incoming::parse(&value)?;
    
    
    state.limits.admits_entries(incoming.entries.len())?;
    let grant = granted(&headers)?;
    let access = Arc::new(crate::access::access_of(&state, &headers).await?);
    let provenance = crate::provenance::carried(&headers, state.version)?;
    let entries = Arc::new(incoming.entries);
    if let Kind::Document = incoming.kind {
        return crate::document::received(&state, &value, &headers).await;
    }
    match incoming.kind {
        Kind::Document => unreachable!("a document was answered above"),
        Kind::Transaction => {
            transaction(
                &state,
                &headers,
                &entries,
                &access,
                grant.as_ref(),
                provenance,
            )
            .await
        }
        Kind::Batch => Ok(batch(
            &state,
            &headers,
            &entries,
            &access,
            grant.as_ref(),
            provenance,
        )
        .await),
    }
}

fn written_references(taken: &[Value]) -> Vec<String> {
    taken
        .iter()
        .filter_map(|entry| entry["response"]["location"].as_str())
        .filter_map(crate::provenance::reference_from)
        .collect()
}

enum Kind {
    Transaction,
    Batch,
    
    
    Document,
}

impl Kind {
    fn response_type(&self) -> &'static str {
        match self {
            Kind::Transaction => "transaction-response",
            Kind::Batch => "batch-response",
            Kind::Document => "document",
        }
    }
}

struct Entry {
    method: String,
    url: String,
    full_url: Option<String>,
    resource: Option<Value>,
    if_none_exist: Option<String>,
    if_match: Option<String>,
}

struct Incoming {
    kind: Kind,
    entries: Vec<Result<Entry, Error>>,
}

impl Incoming {
    fn parse(value: &Value) -> Result<Incoming, Error> {
        if value["resourceType"] != BUNDLE {
            return Err(Error::InvalidEnvelope("a bundle is expected".to_owned()));
        }
        let kind = match value["type"].as_str() {
            Some(TRANSACTION) => Kind::Transaction,
            Some(BATCH) => Kind::Batch,
            Some(DOCUMENT) => Kind::Document,
            Some(other) => return Err(Error::InvalidEnvelope(format!("bundle type {other:?}"))),
            None => {
                return Err(Error::InvalidEnvelope(
                    "a bundle type is required".to_owned(),
                ))
            }
        };
        let listed = match &value["entry"] {
            Value::Null => Vec::new(),
            Value::Array(entries) => entries.clone(),
            _ => {
                return Err(Error::InvalidEnvelope(
                    "bundle entries are a list".to_owned(),
                ))
            }
        };
        let entries = listed.iter().map(Entry::parse).collect();
        Ok(Incoming { kind, entries })
    }
}

impl Entry {
    fn parse(value: &Value) -> Result<Entry, Error> {
        let request = &value["request"];
        let method = request["method"]
            .as_str()
            .ok_or_else(|| Error::InvalidEnvelope("an entry needs a method".to_owned()))?
            .to_uppercase();
        if !matches!(method.as_str(), "GET" | "POST" | "PUT" | "DELETE" | "PATCH") {
            return Err(Error::InvalidEnvelope(format!("entry method {method:?}")));
        }
        let url = request["url"]
            .as_str()
            .ok_or_else(|| Error::InvalidEnvelope("an entry needs a url".to_owned()))?
            .trim_start_matches('/')
            .to_owned();
        if url.is_empty() {
            return Err(Error::InvalidEnvelope("an entry url is empty".to_owned()));
        }
        Ok(Entry {
            method,
            url,
            full_url: text(value, "fullUrl"),
            resource: match &value["resource"] {
                Value::Null => None,
                found => Some(found.clone()),
            },
            if_none_exist: text(request, "ifNoneExist"),
            if_match: text(request, "ifMatch"),
        })
    }
}

fn text(request: &Value, name: &str) -> Option<String> {
    request[name].as_str().map(str::to_owned)
}

#[derive(Default)]
struct Places {
    known: Vec<(String, String)>,
}

impl Places {
    fn applied(&self, entry: &Entry) -> Vec<u8> {
        match &entry.resource {
            Some(resource) => walked(resource, &self.known).to_string().into_bytes(),
            None => Vec::new(),
        }
    }

    fn note(&mut self, entry: &Result<Entry, Error>, location: Option<&str>) {
        let (Ok(entry), Some(location)) = (entry, location) else {
            return;
        };
        let Some(place) = &entry.full_url else {
            return;
        };
        match place.is_empty() {
            true => (),
            false => self.known.push((place.clone(), reference_of(location))),
        }
    }
}

fn walked(value: &Value, known: &[(String, String)]) -> Value {
    match value {
        Value::String(text) => {
            Value::String(known.iter().fold(text.clone(), |text, (place, reference)| {
                replaced(&text, place, reference)
            }))
        }
        Value::Array(items) => Value::Array(items.iter().map(|item| walked(item, known)).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, item)| (name.clone(), walked(item, known)))
                .collect(),
        ),
        found => found.clone(),
    }
}

fn replaced(text: &str, place: &str, reference: &str) -> String {
    match text {
        whole if whole == place => reference.to_owned(),
        other => match other.split_once('#') {
            Some((head, tail)) if head == place => format!("{reference}#{tail}"),
            _ => other.replace(place, reference),
        },
    }
}

fn reference_of(location: &str) -> String {
    match location.split_once("/_history/") {
        Some((head, _)) => head.to_owned(),
        None => location.to_owned(),
    }
}

struct Taken {
    status: StatusCode,
    location: Option<String>,
    etag: Option<String>,
    last_modified: Option<String>,
    body: Option<Value>,
}

impl Taken {
    fn failed(&self) -> bool {
        self.status.is_client_error() || self.status.is_server_error()
    }

    fn to_entry(&self) -> Value {
        let mut response = Map::new();
        response.insert("status".to_owned(), json!(spelled(self.status)));
        if let Some(location) = &self.location {
            response.insert("location".to_owned(), json!(location));
        }
        if let Some(etag) = &self.etag {
            response.insert("etag".to_owned(), json!(etag));
        }
        if let Some(last_modified) = &self.last_modified {
            response.insert("lastModified".to_owned(), json!(last_modified));
        }
        let mut entry = Map::new();
        entry.insert("response".to_owned(), Value::Object(response));
        if let Some(body) = &self.body {
            let slot = match body["resourceType"] == OUTCOME {
                true => "outcome",
                false => "resource",
            };
            entry.insert(slot.to_owned(), body.clone());
        }
        Value::Object(entry)
    }

    fn into_response(self) -> Response {
        let body = match self.body {
            Some(body) => body.to_string(),
            None => String::new(),
        };
        (
            self.status,
            [
                (header::CONTENT_TYPE, FHIR_JSON),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response()
    }
}

fn spelled(status: StatusCode) -> String {
    match status.canonical_reason() {
        Some(reason) => format!("{} {reason}", status.as_u16()),
        None => status.as_u16().to_string(),
    }
}

async fn transaction(
    state: &AppState,
    headers: &HeaderMap,
    entries: &[Result<Entry, Error>],
    access: &Access,
    grant: Option<&Grant>,
    provenance: Option<Value>,
) -> Result<Response, AppError> {
    let scope = state.store.begin().await?;
    let router = crate::app::over(state, scope.store());
    let mut taken: Vec<Option<Taken>> = entries.iter().map(|_| None).collect();
    let mut places = Places::default();
    for index in ordered(entries) {
        let outcome = dispatch(&router, headers, &entries[index], &places, access, grant).await;
        if outcome.failed() {
            scope.rollback().await?;
            return Ok(outcome.into_response());
        }
        places.note(&entries[index], outcome.location.as_deref());
        taken[index] = Some(outcome);
    }
    let listed: Vec<Value> = taken
        .into_iter()
        .flatten()
        .map(|outcome| outcome.to_entry())
        .collect();
    let scoped = AppState {
        store: scope.store(),
        ..state.clone()
    };
    if let Err(error) =
        crate::provenance::record(&scoped, access, provenance, &written_references(&listed)).await
    {
        scope.rollback().await?;
        return Err(error.into());
    }
    scope.commit().await?;
    Ok(replied(
        Kind::Transaction,
        listed,
        Return::asked_for(headers),
    ))
}

async fn batch(
    state: &AppState,
    headers: &HeaderMap,
    entries: &Arc<Vec<Result<Entry, Error>>>,
    access: &Arc<Access>,
    grant: Option<&Grant>,
    provenance: Option<Value>,
) -> Response {
    let router = crate::app::over(state, Arc::clone(&state.store));
    let mut listed = vec![Value::Null; entries.len()];
    let gate = Arc::clone(&state.entries);
    match linked(entries) {
        true => sequential(&router, headers, entries, access, grant, &mut listed).await,
        false => parallel(&router, headers, entries, access, grant, &gate, &mut listed).await,
    }
    if let Err(error) =
        crate::provenance::record(state, access, provenance, &written_references(&listed)).await
    {
        return AppError::from(error).into_response_now();
    }
    replied(Kind::Batch, listed, Return::asked_for(headers))
}

fn linked(entries: &[Result<Entry, Error>]) -> bool {
    entries
        .iter()
        .any(|entry| matches!(entry, Ok(entry) if entry.full_url.is_some()))
}

async fn sequential(
    router: &Router<()>,
    headers: &HeaderMap,
    entries: &[Result<Entry, Error>],
    access: &Access,
    grant: Option<&Grant>,
    listed: &mut [Value],
) {
    let mut places = Places::default();
    for index in 0..entries.len() {
        let taken = dispatch(router, headers, &entries[index], &places, access, grant).await;
        places.note(&entries[index], taken.location.as_deref());
        listed[index] = taken.to_entry();
    }
}

async fn parallel(
    router: &Router<()>,
    headers: &HeaderMap,
    entries: &Arc<Vec<Result<Entry, Error>>>,
    access: &Arc<Access>,
    grant: Option<&Grant>,
    gate: &Arc<Semaphore>,
    listed: &mut [Value],
) {
    let mut running = JoinSet::new();
    for index in 0..entries.len() {
        let router = router.clone();
        let headers = headers.clone();
        let entries = Arc::clone(entries);
        let gate = Arc::clone(gate);
        let grant = grant.cloned();
        let access = Arc::clone(access);
        running.spawn(async move {
            let _permit = gate.acquire().await;
            let taken = dispatch(
                &router,
                &headers,
                &entries[index],
                &Places::default(),
                &access,
                grant.as_ref(),
            )
            .await;
            (index, taken.to_entry())
        });
    }
    while let Some(joined) = running.join_next().await {
        match joined {
            Ok((index, entry)) => listed[index] = entry,
            Err(_) => continue,
        }
    }
}

fn ordered(entries: &[Result<Entry, Error>]) -> Vec<usize> {
    let mut indexes: Vec<usize> = (0..entries.len()).collect();
    indexes.sort_by_key(|index| match &entries[*index] {
        Ok(entry) => rank(&entry.method),
        Err(_) => 0,
    });
    indexes
}

fn rank(method: &str) -> u8 {
    match method {
        "DELETE" => 0,
        "POST" => 1,
        "PUT" | "PATCH" => 2,
        _ => 3,
    }
}

fn replied(kind: Kind, entries: Vec<Value>, asked: Option<Return>) -> Response {
    let entries = match asked {
        Some(Return::Minimal) => entries.into_iter().map(stripped).collect(),
        _ => entries,
    };
    let bundle = json!({
        "resourceType": BUNDLE,
        "type": kind.response_type(),
        "entry": entries,
    });
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, FHIR_JSON),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bundle.to_string(),
    )
        .into_response()
}

fn stripped(entry: Value) -> Value {
    match entry {
        Value::Object(mut fields) => {
            fields.remove("resource");
            Value::Object(fields)
        }
        other => other,
    }
}

async fn dispatch(
    router: &Router<()>,
    outer: &HeaderMap,
    entry: &Result<Entry, Error>,
    places: &Places,
    access: &Access,
    grant: Option<&Grant>,
) -> Taken {
    let entry = match entry {
        Err(error) => return refused(error),
        Ok(entry) => entry,
    };
    if let Err(error) = permitted(access, grant, entry) {
        return refused(&error);
    }
    match built(outer, entry, places) {
        Err(error) => refused(&error),
        Ok(request) => match router.clone().oneshot(request).await {
            Err(_) => refused(&Error::Internal("an entry was not dispatched".to_owned())),
            Ok(response) => collected(response).await,
        },
    }
}

fn built(outer: &HeaderMap, entry: &Entry, places: &Places) -> Result<Request<Body>, Error> {
    let mut builder = Request::builder()
        .method(entry.method.as_str())
        .uri(format!("/{}", entry.url))
        .header(header::HOST, host_of(outer))
        .header(header::CONTENT_TYPE, FHIR_JSON);
    for (name, value) in [
        (IF_NONE_EXIST, &entry.if_none_exist),
        ("if-match", &entry.if_match),
    ] {
        if let Some(value) = value {
            builder = builder.header(name, value.as_str());
        }
    }
    if let Some(scope) = outer.get(SCOPE) {
        builder = builder.header(HeaderName::from_static(SCOPE), scope.clone());
    }
    if let Some(credential) = outer.get(header::AUTHORIZATION) {
        builder = builder.header(header::AUTHORIZATION, credential.clone());
    }
    
    
    
    for name in [crate::tenancy::HEADER, crate::tenancy::REVEAL] {
        if let Some(value) = outer.get(name) {
            builder = builder.header(name, value.clone());
        }
    }
    builder
        .body(Body::from(places.applied(entry)))
        .map_err(|_| Error::InvalidEnvelope(format!("entry url {:?}", entry.url)))
}

fn host_of(headers: &HeaderMap) -> HeaderValue {
    headers
        .get(header::HOST)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("localhost"))
}

fn refused(error: &Error) -> Taken {
    let outcome = error.to_operation_outcome();
    let status =
        StatusCode::from_u16(outcome.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    Taken {
        status,
        location: None,
        etag: None,
        last_modified: None,
        body: serde_json::from_slice(&outcome.to_fhir_json()).ok(),
    }
}

async fn collected(response: Response) -> Taken {
    let (parts, body) = response.into_parts();
    let status = parts.status;
    let location = named(&parts.headers, header::LOCATION).map(|value| trimmed(&value));
    let etag = named(&parts.headers, header::ETAG);
    let last_modified = named(&parts.headers, header::LAST_MODIFIED);
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => Bytes::new(),
    };
    Taken {
        status,
        location,
        etag,
        last_modified,
        body: serde_json::from_slice(&bytes).ok(),
    }
}

fn trimmed(location: &str) -> String {
    match location.split_once("://") {
        None => location.trim_start_matches('/').to_owned(),
        Some((_, rest)) => match rest.split_once('/') {
            None => rest.to_owned(),
            Some((_, path)) => path.to_owned(),
        },
    }
}

fn named(headers: &HeaderMap, name: HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn granted(headers: &HeaderMap) -> Result<Option<Grant>, Error> {
    match headers.get(SCOPE) {
        None => Ok(None),
        Some(value) => {
            let raw = value
                .to_str()
                .map_err(|_| Error::InvalidParameter("scope is not ascii".to_owned()))?;
            Ok(Some(Grant::parse(raw)?))
        }
    }
}

fn permitted(access: &Access, grant: Option<&Grant>, entry: &Entry) -> Result<(), Error> {
    let kind = target(&entry.url).and_then(|name| name.parse::<ResourceType>().ok());
    access.require(action_of(&entry.method), kind)?;
    let (Some(grant), Some(kind)) = (grant, kind) else {
        return Ok(());
    };
    match grant.admits(kind) {
        true => Ok(()),
        false => Err(Error::Forbidden(format!("type {:?}", kind.as_str()))),
    }
}

fn action_of(method: &str) -> DataAction {
    match method {
        "GET" | "HEAD" => DataAction::Read,
        _ => DataAction::Write,
    }
}

fn target(url: &str) -> Option<&str> {
    let head = url
        .split('?')
        .next()
        .unwrap_or_default()
        .split('/')
        .next()
        .unwrap_or_default();
    match head.is_empty() || head.starts_with('_') || head.starts_with('$') {
        true => None,
        false => Some(head),
    }
}
