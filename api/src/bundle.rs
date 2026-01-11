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
use tower::ServiceExt;

use crate::app::AppState;
use crate::handlers::AppError;

const FHIR_JSON: &str = "application/fhir+json";
const BUNDLE: &str = "Bundle";
const OUTCOME: &str = "OperationOutcome";
const TRANSACTION: &str = "transaction";
const BATCH: &str = "batch";
const IF_NONE_EXIST: &str = "if-none-exist";
const SCOPE: &str = "x-scope";

pub async fn process(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let value: Value =
        serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let incoming = Incoming::parse(&value)?;
    let grant = granted(&headers)?;
    match incoming.kind {
        Kind::Transaction => {
            transaction(&state, &headers, &incoming.entries, grant.as_ref()).await
        }
        Kind::Batch => Ok(batch(&state, &headers, &incoming.entries, grant.as_ref()).await),
    }
}

enum Kind {
    Transaction,
    Batch,
}

impl Kind {
    fn response_type(&self) -> &'static str {
        match self {
            Kind::Transaction => "transaction-response",
            Kind::Batch => "batch-response",
        }
    }
}

struct Entry {
    method: String,
    url: String,
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
            Some(other) => {
                return Err(Error::InvalidEnvelope(format!("bundle type {other:?}")))
            }
            None => return Err(Error::InvalidEnvelope("a bundle type is required".to_owned())),
        };
        let listed = match &value["entry"] {
            Value::Null => Vec::new(),
            Value::Array(entries) => entries.clone(),
            _ => return Err(Error::InvalidEnvelope("bundle entries are a list".to_owned())),
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
            resource: match &value["resource"] {
                Value::Null => None,
                found => Some(found.clone()),
            },
            if_none_exist: text(request, "ifNoneExist"),
            if_match: text(request, "ifMatch"),
        })
    }

    fn body(&self) -> Vec<u8> {
        match &self.resource {
            Some(resource) => resource.to_string().into_bytes(),
            None => Vec::new(),
        }
    }
}

fn text(request: &Value, name: &str) -> Option<String> {
    request[name].as_str().map(str::to_owned)
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
            [(header::CONTENT_TYPE, FHIR_JSON), (header::CACHE_CONTROL, "no-store")],
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
    grant: Option<&Grant>,
) -> Result<Response, AppError> {
    let scope = state.store.begin().await?;
    let router = crate::app::over(state, scope.store());
    let mut taken: Vec<Option<Taken>> = entries.iter().map(|_| None).collect();
    for index in ordered(entries) {
        let outcome = dispatch(&router, headers, &entries[index], grant).await;
        if outcome.failed() {
            scope.rollback().await?;
            return Ok(outcome.into_response());
        }
        taken[index] = Some(outcome);
    }
    scope.commit().await?;
    let listed = taken.into_iter().flatten().map(|outcome| outcome.to_entry()).collect();
    Ok(replied(Kind::Transaction, listed))
}

async fn batch(
    state: &AppState,
    headers: &HeaderMap,
    entries: &[Result<Entry, Error>],
    grant: Option<&Grant>,
) -> Response {
    let router = crate::app::over(state, std::sync::Arc::clone(&state.store));
    let mut listed = Vec::with_capacity(entries.len());
    for entry in entries {
        listed.push(dispatch(&router, headers, entry, grant).await.to_entry());
    }
    replied(Kind::Batch, listed)
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

fn replied(kind: Kind, entries: Vec<Value>) -> Response {
    let bundle = json!({
        "resourceType": BUNDLE,
        "type": kind.response_type(),
        "entry": entries,
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, FHIR_JSON), (header::CACHE_CONTROL, "no-store")],
        bundle.to_string(),
    )
        .into_response()
}

async fn dispatch(
    router: &Router<()>,
    outer: &HeaderMap,
    entry: &Result<Entry, Error>,
    grant: Option<&Grant>,
) -> Taken {
    let entry = match entry {
        Err(error) => return refused(error),
        Ok(entry) => entry,
    };
    if let Err(error) = permitted(grant, entry) {
        return refused(&error);
    }
    match built(outer, entry) {
        Err(error) => refused(&error),
        Ok(request) => match router.clone().oneshot(request).await {
            Err(_) => refused(&Error::Internal("an entry was not dispatched".to_owned())),
            Ok(response) => collected(response).await,
        },
    }
}

fn built(outer: &HeaderMap, entry: &Entry) -> Result<Request<Body>, Error> {
    let mut builder = Request::builder()
        .method(entry.method.as_str())
        .uri(format!("/{}", entry.url))
        .header(header::HOST, host_of(outer))
        .header(header::CONTENT_TYPE, FHIR_JSON);
    for (name, value) in [(IF_NONE_EXIST, &entry.if_none_exist), ("if-match", &entry.if_match)] {
        if let Some(value) = value {
            builder = builder.header(name, value.as_str());
        }
    }
    if let Some(scope) = outer.get(SCOPE) {
        builder = builder.header(HeaderName::from_static(SCOPE), scope.clone());
    }
    builder
        .body(Body::from(entry.body()))
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
    headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_owned)
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

fn permitted(grant: Option<&Grant>, entry: &Entry) -> Result<(), Error> {
    let Some(grant) = grant else { return Ok(()) };
    let Some(target) = target(&entry.url) else { return Ok(()) };
    let Ok(kind) = target.parse::<ResourceType>() else { return Ok(()) };
    match grant.admits(kind) {
        true => Ok(()),
        false => Err(Error::Forbidden(format!("type {:?}", kind.as_str()))),
    }
}

fn target(url: &str) -> Option<&str> {
    let head = url.split('?').next().unwrap_or_default().split('/').next().unwrap_or_default();
    match head.is_empty() || head.starts_with('_') || head.starts_with('$') {
        true => None,
        false => Some(head),
    }
}
