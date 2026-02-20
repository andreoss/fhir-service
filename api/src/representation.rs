use axum::body::{Body, Bytes, HttpBody};
use axum::extract::Request;
use axum::http::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fhir_core::Error;

use crate::handlers::AppError;
use crate::query::param;

pub const FHIR_JSON: &str = "application/fhir+json";
pub const JSON: &str = "application/json";

const PRETTY_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    FhirJson,
    Json,
}

impl MediaType {
    pub const DEFAULT: MediaType = MediaType::FhirJson;

    pub fn as_str(self) -> &'static str {
        match self {
            MediaType::FhirJson => FHIR_JSON,
            MediaType::Json => JSON,
        }
    }

    pub fn parse(text: &str) -> Option<MediaType> {
        let name = text
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match name.as_str() {
            "json" | "fhir+json" | "application/fhir+json" | "application/json+fhir" => {
                Some(MediaType::FhirJson)
            }
            "application/json" | "text/json" => Some(MediaType::Json),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asked {
    pub media: MediaType,
    pub named: bool,
    pub pretty: bool,
}

impl Asked {
    pub fn read(request: &Request) -> Result<Asked, Error> {
        let query = request.uri().query();
        let pretty = match param(query, "_pretty").as_deref() {
            None => false,
            Some("true") => true,
            Some("false") => false,
            Some(other) => return Err(Error::InvalidParameter(format!("_pretty {other:?}"))),
        };
        let (media, named) = match param(query, "_format") {
            Some(text) => (
                MediaType::parse(&text).ok_or(Error::UnsupportedFormat(text))?,
                true,
            ),
            None => match accepted(request.headers())? {
                Some(media) => (media, true),
                None => (MediaType::DEFAULT, false),
            },
        };
        Ok(Asked {
            media,
            named,
            pretty,
        })
    }
}

fn accepted(headers: &HeaderMap) -> Result<Option<MediaType>, Error> {
    let Some(value) = headers.get(ACCEPT) else {
        return Ok(None);
    };
    let text = value
        .to_str()
        .map_err(|_| Error::InvalidParameter("accept is not ascii".to_owned()))?;
    let mut best: Option<(MediaType, u32)> = None;
    let mut named = false;
    for part in text.split(',') {
        let mut pieces = part.split(';');
        let name = pieces.next().unwrap_or_default().trim();
        if name.is_empty() {
            continue;
        }
        named = true;
        let quality = pieces
            .filter_map(|piece| piece.split_once('='))
            .find(|(key, _)| key.trim().eq_ignore_ascii_case("q"))
            .and_then(|(_, value)| value.trim().parse::<f32>().ok())
            .map(|factor| (factor * 1000.0) as u32)
            .unwrap_or(1000);
        if quality == 0 {
            continue;
        }
        let media = match MediaType::parse(name) {
            Some(media) => media,
            None if name == "*/*" || name.ends_with("/*") => MediaType::DEFAULT,
            None => continue,
        };
        if best.is_none() || best.is_some_and(|(_, held)| quality > held) {
            best = Some((media, quality));
        }
    }
    match (best, named) {
        (Some((media, _)), _) => Ok(Some(media)),
        (None, true) => Err(Error::UnsupportedFormat(text.to_owned())),
        (None, false) => Ok(None),
    }
}

pub async fn negotiated(request: Request, next: Next) -> Response {
    match Asked::read(&request) {
        Err(error) => finished(AppError::from(error).into_response(), None, false).await,
        Ok(asked) => {
            let response = next.run(request).await;
            finished(response, asked.named.then_some(asked.media), asked.pretty).await
        }
    }
}

async fn finished(response: Response, media: Option<MediaType>, pretty: bool) -> Response {
    let (mut parts, body) = response.into_parts();
    let named = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned()
        })
        .unwrap_or_default();
    let fhir = named == FHIR_JSON;
    let json = fhir || named == JSON;
    if fhir {
        if let Some(media) = media {
            parts
                .headers
                .insert(CONTENT_TYPE, HeaderValue::from_static(media.as_str()));
        }
    }
    if !pretty || !json {
        return Response::from_parts(parts, body);
    }
    let bounded = body
        .size_hint()
        .upper()
        .is_some_and(|size| size <= PRETTY_LIMIT);
    if !bounded {
        return Response::from_parts(parts, body);
    }
    let bytes = match axum::body::to_bytes(body, PRETTY_LIMIT as usize).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return AppError::from(Error::Internal(
                "the answer exceeds the size that can be reprinted".to_owned(),
            ))
            .into_response()
        }
    };
    match reprinted(&bytes) {
        Some(rendered) => {
            parts
                .headers
                .insert(CONTENT_LENGTH, sized(rendered.len() as u64));
            Response::from_parts(parts, Body::from(rendered))
        }
        None => Response::from_parts(parts, Body::from(bytes)),
    }
}

fn sized(length: u64) -> HeaderValue {
    HeaderValue::from_str(&length.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0"))
}

fn reprinted(bytes: &Bytes) -> Option<Vec<u8>> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
    serde_json::to_vec_pretty(&value).ok()
}
