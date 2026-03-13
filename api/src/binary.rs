use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use fhir_core::security::scope::DataAction;
use fhir_core::{Error, ResourceEnvelope, ResourceId, ResourceKey, ResourceType};
use fhir_store::Interaction;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::handlers::{allowed_doing, AppError};

const BINARY: &str = "Binary";

pub const HELD_TYPES: [&str; 6] = [
    "application/pdf",
    "text/plain",
    "text/csv",
    "image/png",
    "image/jpeg",
    "application/json",
];

pub const HELD_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifacts {
    pub media_types: Vec<String>,
    pub most_bytes: usize,
}

impl Default for Artifacts {
    fn default() -> Artifacts {
        Artifacts {
            media_types: HELD_TYPES.iter().map(|held| (*held).to_owned()).collect(),
            most_bytes: HELD_BYTES,
        }
    }
}

impl Artifacts {
    pub fn holds(&self, media_type: &str) -> bool {
        let name = media_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        self.media_types
            .iter()
            .any(|held| held.eq_ignore_ascii_case(&name))
    }
}

pub fn is_artifact(path: &str, headers: &HeaderMap) -> bool {
    if !path.starts_with("/Binary") {
        return false;
    }
    let named = |name: header::HeaderName| -> Option<String> {
        headers
            .get(name)?
            .to_str()
            .ok()
            .map(|held| held.to_ascii_lowercase())
    };
    let fhir = |held: &Option<String>| -> bool {
        held.as_deref().is_none_or(|value| {
            value.contains("fhir") || value.contains("*/*") || value.contains("json")
        })
    };
    !(fhir(&named(header::CONTENT_TYPE)) && fhir(&named(header::ACCEPT)))
}

pub async fn write(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let id = ResourceId::parse(&uuid::Uuid::new_v4().to_string())?;
    Ok(stored(&state, id, &headers, &body, true).await?)
}

pub async fn write_at(
    State(state): State<AppState>,
    Path(id_text): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let id = id_text.parse::<ResourceId>()?;
    let held = ResourceKey::new(BINARY.parse::<ResourceType>()?, id.clone());
    let fresh = state.store.read(&held).await.is_err();
    Ok(stored(&state, id, &headers, &body, fresh).await?)
}

async fn stored(
    state: &AppState,
    id: ResourceId,
    headers: &HeaderMap,
    body: &[u8],
    fresh: bool,
) -> Result<Response, Error> {
    let binary = BINARY.parse::<ResourceType>()?;
    allowed_doing(
        state,
        headers,
        DataAction::Write,
        Interaction::Create,
        Some(binary),
        Some(&id),
    )
    .await?;
    let media_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| Error::InvalidEnvelope("an artifact names its media type".to_owned()))?;
    if !state.artifacts.holds(media_type) {
        return Err(Error::UnsupportedFormat(format!(
            "{media_type:?} is not among the media types this instance holds"
        )));
    }
    if body.len() > state.artifacts.most_bytes {
        return Err(Error::InvalidEnvelope(format!(
            "the artifact is {} bytes, past the {} this instance holds",
            body.len(),
            state.artifacts.most_bytes
        )));
    }
    let mut value = json!({
        "resourceType": BINARY,
        "id": id.as_str(),
        "contentType": media_type.split(';').next().unwrap_or(media_type).trim(),
        "data": STANDARD.encode(body),
    });
    fhir_core::with_assigned_meta(&mut value)?;
    let envelope = ResourceEnvelope::parse(
        state.version,
        &serde_json::to_vec(&value).map_err(|error| Error::Internal(error.to_string()))?,
    )?;
    let written = match fresh {
        true => state.store.create(envelope).await?,
        false => state.store.update(envelope, None).await?,
    };
    let status = match fresh {
        true => StatusCode::CREATED,
        false => StatusCode::OK,
    };
    let location = format!(
        "http://{}/{BINARY}/{}/_history/{}",
        crate::handlers::addressed(state, headers),
        written.id(),
        written.version_id()
    );
    let mut response = (status, Vec::new()).into_response();
    let held = response.headers_mut();
    held.insert(
        header::LOCATION,
        HeaderValue::from_str(&location).expect("a location is a header value"),
    );
    held.insert(
        header::ETAG,
        HeaderValue::from_str(&format!("W/\"{}\"", written.version_id()))
            .expect("an etag is a header value"),
    );
    Ok(response)
}

pub async fn read(
    State(state): State<AppState>,
    Path(id_text): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let binary = BINARY.parse::<ResourceType>()?;
    let id = id_text.parse::<ResourceId>()?;
    allowed_doing(
        &state,
        &headers,
        DataAction::Read,
        Interaction::Read,
        Some(binary),
        Some(&id),
    )
    .await?;
    let stored = state.store.read(&ResourceKey::new(binary, id)).await?;
    if stored.is_deleted() {
        return Err(Error::Deleted.into());
    }

    if !is_artifact("/Binary", &headers) {
        return Ok(crate::handlers::rendered(stored.raw().to_vec()));
    }
    let body: Value = serde_json::from_slice(stored.raw())
        .map_err(|error| Error::InvalidJson(error.to_string()))?;
    let media_type = body
        .get("contentType")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream")
        .to_owned();
    let data = body
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidEnvelope("the binary holds no data".to_owned()))?;
    let held = STANDARD
        .decode(data)
        .map_err(|error| Error::InvalidEnvelope(format!("the artifact is not base64: {error}")))?;
    let mut response = (StatusCode::OK, held).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&media_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
        let mut held = HeaderMap::new();
        for (name, value) in pairs {
            held.insert(name.clone(), HeaderValue::from_str(value).unwrap());
        }
        held
    }

    #[test]
    fn the_media_types_an_instance_holds_are_a_list() {
        let held = Artifacts::default();
        assert!(held.holds("application/pdf"));
        assert!(held.holds("application/pdf; charset=binary"));
        assert!(held.holds("APPLICATION/PDF"));
        assert!(!held.holds("application/x-anything"));
    }

    #[test]
    fn a_fhir_request_about_a_binary_is_not_an_artifact_request() {
        assert!(!is_artifact(
            "/Binary/one",
            &headers(&[(header::ACCEPT, "application/fhir+json")])
        ));
        assert!(!is_artifact("/Patient/one", &HeaderMap::new()));
        assert!(!is_artifact("/Binary/one", &HeaderMap::new()));
    }

    #[test]
    fn a_request_carrying_an_artifact_is_one() {
        assert!(is_artifact(
            "/Binary",
            &headers(&[(header::CONTENT_TYPE, "application/pdf")])
        ));
        assert!(is_artifact(
            "/Binary/one",
            &headers(&[(header::ACCEPT, "application/pdf")])
        ));
    }
}
