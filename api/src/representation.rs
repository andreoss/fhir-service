use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{Request, State};
use axum::http::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fhir_core::{Error, FhirVersion};
use serde_json::Value;

use crate::app::AppState;
use crate::handlers::AppError;
use crate::query::param;

pub const FHIR_JSON: &str = "application/fhir+json";
pub const JSON: &str = "application/json";
pub const FHIR_XML: &str = "application/fhir+xml";
pub const XML: &str = "application/xml";

const PRETTY_LIMIT: u64 = 8 * 1024 * 1024;
const BODY_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    FhirJson,
    Json,
    FhirXml,
    Xml,
}

impl MediaType {
    pub const DEFAULT: MediaType = MediaType::FhirJson;

    pub fn as_str(self) -> &'static str {
        match self {
            MediaType::FhirJson => FHIR_JSON,
            MediaType::Json => JSON,
            MediaType::FhirXml => FHIR_XML,
            MediaType::Xml => XML,
        }
    }

    pub fn is_xml(self) -> bool {
        matches!(self, MediaType::FhirXml | MediaType::Xml)
    }

    pub fn parse(text: &str) -> Option<MediaType> {
        let name = text
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let collapsed = name.replace(' ', "+");
        match collapsed.as_str() {
            "json" | "fhir+json" | "application/fhir+json" | "application/json+fhir" => {
                Some(MediaType::FhirJson)
            }
            "application/json" | "text/json" => Some(MediaType::Json),
            "xml" | "fhir+xml" | "application/fhir+xml" | "application/xml+fhir" => {
                Some(MediaType::FhirXml)
            }
            "application/xml" | "text/xml" => Some(MediaType::Xml),
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

pub async fn negotiated(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let asked = match Asked::read(&request) {
        Err(error) => {
            return finished(&state, AppError::from(error).into_response(), None, false).await
        }
        Ok(asked) => asked,
    };
    let media = asked.named.then_some(asked.media);
    match translated(&state, request).await {
        Err(error) => {
            finished(
                &state,
                AppError::from(error).into_response(),
                media,
                asked.pretty,
            )
            .await
        }
        Ok(request) => {
            let response = next.run(request).await;
            finished(&state, response, media, asked.pretty).await
        }
    }
}

async fn translated(state: &AppState, request: Request) -> Result<Request, Error> {
    let xml = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(MediaType::parse)
        .is_some_and(MediaType::is_xml);
    if !xml {
        return Ok(request);
    }
    let (mut parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, BODY_LIMIT)
        .await
        .map_err(|_| Error::InvalidXml("the body cannot be read".to_owned()))?;
    let text = String::from_utf8(bytes.to_vec())
        .map_err(|_| Error::InvalidXml("the body is not utf-8".to_owned()))?;
    let value = fhir_core::xml::from_xml(state.version, &text)?;
    let rendered =
        serde_json::to_vec(&value).map_err(|error| Error::Internal(error.to_string()))?;
    parts
        .headers
        .insert(CONTENT_TYPE, HeaderValue::from_static(FHIR_JSON));
    Ok(Request::from_parts(parts, Body::from(rendered)))
}

async fn finished(
    state: &AppState,
    response: Response,
    media: Option<MediaType>,
    pretty: bool,
) -> Response {
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
    if named != FHIR_JSON {
        return Response::from_parts(parts, body);
    }
    let asked_xml = media.is_some_and(MediaType::is_xml);
    if !asked_xml && !pretty {
        if let Some(media) = media {
            parts
                .headers
                .insert(CONTENT_TYPE, HeaderValue::from_static(media.as_str()));
        }
        return Response::from_parts(parts, body);
    }
    let bounded = body
        .size_hint()
        .upper()
        .is_some_and(|size| size <= PRETTY_LIMIT);
    if !bounded {
        if let Some(media) = media {
            parts
                .headers
                .insert(CONTENT_TYPE, HeaderValue::from_static(media.as_str()));
        }
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
    if let Some(media) = media.filter(|media| media.is_xml()) {
        match as_xml(state.version, &bytes, pretty) {
            Some(rendered) => {
                parts
                    .headers
                    .insert(CONTENT_TYPE, HeaderValue::from_static(media.as_str()));
                parts
                    .headers
                    .insert(CONTENT_LENGTH, sized(rendered.len() as u64));
                return Response::from_parts(parts, Body::from(rendered));
            }
            None => {
                parts
                    .headers
                    .insert(CONTENT_TYPE, HeaderValue::from_static(FHIR_JSON));
                return Response::from_parts(parts, Body::from(bytes));
            }
        }
    }
    if let Some(media) = media {
        parts
            .headers
            .insert(CONTENT_TYPE, HeaderValue::from_static(media.as_str()));
    }
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

fn as_xml(version: FhirVersion, bytes: &Bytes, pretty: bool) -> Option<Vec<u8>> {
    let value = serde_json::from_slice::<Value>(bytes).ok()?;
    let written = match pretty {
        true => fhir_core::xml::to_xml_pretty(version, &value),
        false => fhir_core::xml::to_xml(version, &value),
    };
    written.ok().map(String::into_bytes)
}

fn sized(length: u64) -> HeaderValue {
    HeaderValue::from_str(&length.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0"))
}

fn reprinted(bytes: &Bytes) -> Option<Vec<u8>> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
    serde_json::to_vec_pretty(&value).ok()
}
