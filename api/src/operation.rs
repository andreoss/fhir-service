use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::header::{self, HeaderMap};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fhir_core::convert::{convert, Conversion, InputType};
use fhir_core::terminology::{expansion_json, ExpansionRequest, Stamp};
use fhir_core::validate::{validate, Mode, Request as ValidationRequest};
use fhir_core::search::{Compartment, Filter};
use fhir_core::{Error, ResourceId, ResourceType};
use fhir_store::SearchQuery;
use serde_json::Value;

use crate::app::AppState;
use crate::query::param;
use crate::search::{ResultControl, SearchRequest, CONTROL};
use crate::handlers::{allowed, served, AppError};
use fhir_core::security::scope::DataAction;

const FHIR_JSON: &str = "application/fhir+json";

pub fn value_of(body: &Value, name: &str) -> Option<String> {
    entries(body).find_map(|entry| match entry.get("name").and_then(Value::as_str) {
        Some(held) if held == name => primitive(entry),
        _ => None,
    })
}

pub fn values_of(body: &Value, name: &str) -> Vec<String> {
    entries(body)
        .filter(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        .filter_map(primitive)
        .collect()
}

pub fn resource_of<'a>(body: &'a Value, name: &str) -> Option<&'a Value> {
    entries(body).find_map(|entry| match entry.get("name").and_then(Value::as_str) {
        Some(held) if held == name => entry.get("resource"),
        _ => None,
    })
}

fn entries(body: &Value) -> impl Iterator<Item = &Value> {
    body.get("parameter")
        .and_then(Value::as_array)
        .map(|items| items.iter())
        .unwrap_or_else(|| [].iter())
}

fn primitive(entry: &Value) -> Option<String> {
    let object = entry.as_object()?;
    object.iter().find_map(|(name, value)| {
        let named = name.starts_with("value") && name != "value";
        match (named, value) {
            (true, Value::String(text)) => Some(text.clone()),
            (true, Value::Number(number)) => Some(number.to_string()),
            (true, Value::Bool(flag)) => Some(flag.to_string()),
            _ => None,
        }
    })
}

pub fn parameters(body: &[u8]) -> Result<Value, Error> {
    let value: Value =
        serde_json::from_slice(body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    match value.get("resourceType").and_then(Value::as_str) {
        Some("Parameters") => Ok(value),
        _ => Err(Error::InvalidEnvelope(
            "the operation takes a parameters resource".to_owned(),
        )),
    }
}

fn required(body: &Value, name: &str) -> Result<String, Error> {
    value_of(body, name).ok_or_else(|| Error::InvalidParameter(format!("{name:?} is missing")))
}

pub async fn convert_data(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let input = parameters(&body)?;
    let data = required(&input, "inputData")?;
    let input_type = required(&input, "inputDataType")?.parse::<InputType>()?;
    let collection = required(&input, "templateCollectionReference")?;
    let root = required(&input, "rootTemplate")?;
    let converted = convert(
        state.templates.as_ref(),
        &Conversion {
            input_type,
            data: &data,
            collection: &collection,
            root_template: &root,
        },
    )?;
    Ok(rendered(
        serde_json::to_vec(&converted).map_err(|error| Error::Internal(error.to_string()))?,
    ))
}

pub async fn validate_type(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = served(state.version, &type_name)?;
    allowed(&state, &headers, DataAction::Read, Some(resource_type), None).await?;
    validated(&state, Some(resource_type), None, query.as_deref(), &body).await
}

pub async fn validate_instance(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = served(state.version, &type_name)?;
    allowed(&state, &headers, DataAction::Read, Some(resource_type), None).await?;
    let id = id_text.parse::<ResourceId>()?;
    validated(&state, Some(resource_type), Some(id), query.as_deref(), &body).await
}

async fn validated(
    state: &AppState,
    resource_type: Option<ResourceType>,
    id: Option<ResourceId>,
    query: Option<&str>,
    body: &[u8],
) -> Result<Response, AppError> {
    let submitted = submitted(body)?;
    let mut profile = param(query, "profile");
    let mut mode = param(query, "mode");
    let value = match submitted {
        Some(Value::Object(ref object)) if object.get("resourceType") == Some(&Value::String("Parameters".to_owned())) => {
            let held = Value::Object(object.clone());
            profile = profile.or_else(|| value_of(&held, "profile"));
            mode = mode.or_else(|| value_of(&held, "mode"));
            resource_of(&held, "resource").cloned()
        }
        Some(value) => Some(value),
        None => None,
    };
    let mode = match mode {
        Some(text) => text.parse::<Mode>()?,
        None => Mode::Update,
    };
    let body = match (value, &id) {
        (Some(value), _) => value,
        (None, Some(id)) => {
            let stored = state.store.read(id).await?;
            if stored.is_deleted() {
                return Err(Error::Deleted.into());
            }
            serde_json::from_slice(stored.raw())
                .map_err(|error| Error::InvalidJson(error.to_string()))?
        }
        (None, None) if mode == Mode::Delete => Value::Null,
        (None, None) => {
            return Err(Error::InvalidParameter("no resource to validate".to_owned()).into())
        }
    };
    let report = validate(&ValidationRequest {
        version: state.version,
        resource_type,
        id,
        profile: profile.as_deref(),
        mode,
        body: &body,
    });
    Ok(rendered(report.to_fhir_json()))
}

fn submitted(body: &[u8]) -> Result<Option<Value>, Error> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(body)
        .map(Some)
        .map_err(|error| Error::InvalidJson(error.to_string()))
}

pub(crate) const EVERYTHING_PARAMS: [&str; 3] = ["_since", "_till", "_type"];

pub async fn everything(
    State(state): State<AppState>,
    Path(id_text): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let root = "Patient".parse::<ResourceType>()?;
    let id = id_text.parse::<ResourceId>()?;
    let stored = state.store.read(&id).await?;
    if stored.resource_type() != root || stored.is_deleted() {
        return Err(Error::NotFound.into());
    }
    let def = fhir_core::search::compartment::definition(root.as_str())
        .ok_or_else(|| Error::UnsupportedParameter("compartment \"Patient\"".to_owned()))?;
    accepts(query.as_deref(), &EVERYTHING_PARAMS)?;
    let gathered = def
        .types()
        .iter()
        .map(|name| name.parse::<ResourceType>())
        .collect::<Result<Vec<ResourceType>, Error>>()?;
    let types = match param(query.as_deref(), "_type") {
        None => gathered,
        Some(text) => {
            let wanted = listed(&text)?;
            if let Some(refused) = wanted.iter().find(|kind| !gathered.contains(kind)) {
                return Err(Error::UnsupportedParameter(format!(
                    "_type {:?} is not gathered around a patient",
                    refused.as_str()
                ))
                .into());
            }
            wanted
        }
    };
    let control = ResultControl::parse(query.as_deref())?;
    let request = SearchRequest {
        query: SearchQuery {
            types,
            filters: window(&state, query.as_deref())?,
            compartment: Some(Compartment { kind: root, id }),
            count: control.count,
            offset: control.offset,
            total: control.total,
            ..SearchQuery::default()
        },
        summary: control.summary,
        elements: control.elements,
    };
    let path = format!("/{}/{}/$everything", root.as_str(), id_text);
    crate::handlers::respond_page(&state, request, path, query, &headers).await
}

fn window(state: &AppState, query: Option<&str>) -> Result<Vec<Filter>, Error> {
    let mut filters = Vec::new();
    for (name, comparator) in [("_since", "ge"), ("_till", "le")] {
        let Some(text) = param(query, name) else { continue };
        let def = state
            .registry
            .searchable(None, "_lastUpdated")?
            .ok_or_else(|| Error::UnsupportedParameter(format!("{name:?}")))?;
        let value = def
            .value_with(&fhir_core::search::Modifier::None, &format!("{comparator}{text}"))
            .map_err(|_| Error::InvalidParameter(format!("{name} {text:?}")))?;
        filters.push(Filter::new(name, def.target.clone(), vec![value]));
    }
    Ok(filters)
}

fn listed(raw: &str) -> Result<Vec<ResourceType>, Error> {
    raw.split(',')
        .filter(|part| !part.is_empty())
        .map(str::parse::<ResourceType>)
        .collect()
}

fn accepts(query: Option<&str>, allowed: &[&str]) -> Result<(), Error> {
    for (name, _) in crate::query::pairs(query) {
        let known = allowed.contains(&name.as_str()) || CONTROL.contains(&name.as_str());
        if !known {
            return Err(Error::UnsupportedParameter(format!("{name:?}")));
        }
    }
    Ok(())
}

pub async fn member_match(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let input = parameters(&body)?;
    let submitted = resource_of(&input, "MemberPatient").ok_or_else(|| {
        Error::InvalidParameter("\"MemberPatient\" is missing".to_owned())
    })?;
    let identifiers = identifiers(submitted);
    if identifiers.is_empty() {
        return Err(Error::InvalidParameter(
            "the submitted member carries no identifier".to_owned(),
        )
        .into());
    }
    let patient = "Patient".parse::<ResourceType>()?;
    let def = state
        .registry
        .searchable(Some(patient), "identifier")?
        .ok_or_else(|| Error::UnsupportedParameter("\"identifier\"".to_owned()))?;
    let values = identifiers
        .iter()
        .map(|text| def.value_with(&fhir_core::search::Modifier::None, text))
        .collect::<Result<Vec<fhir_core::SearchValue>, Error>>()?;
    let query = SearchQuery {
        types: vec![patient],
        filters: vec![Filter::new("identifier", def.target.clone(), values)],
        ..SearchQuery::default()
    };
    let page = state.store.search(&query).await?;
    let birth_date = submitted.get("birthDate").and_then(Value::as_str);
    let mut candidates: Vec<&fhir_core::ResourceEnvelope> = Vec::new();
    for found in &page.entries {
        let body: Value = serde_json::from_slice(found.raw())
            .map_err(|error| Error::InvalidJson(error.to_string()))?;
        let agrees = match (birth_date, body.get("birthDate").and_then(Value::as_str)) {
            (Some(asked), Some(held)) => asked == held,
            _ => true,
        };
        if agrees {
            candidates.push(found);
        }
    }
    let matched = match candidates.len() {
        1 => candidates[0],
        0 => {
            return Err(Error::NoMatch("no member matched the submitted patient".to_owned()).into())
        }
        _ => {
            return Err(Error::NoMatch(
                "the submitted patient matched more than one member".to_owned(),
            )
            .into())
        }
    };
    let stored: Value = serde_json::from_slice(matched.raw())
        .map_err(|error| Error::InvalidJson(error.to_string()))?;
    let identifier = stored
        .get("identifier")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or(Value::Null);
    let answer = serde_json::json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "MemberIdentifier", "valueIdentifier": identifier},
            {"name": "MemberPatient", "resource": stored}
        ]
    });
    Ok(rendered(
        serde_json::to_vec(&answer).map_err(|error| Error::Internal(error.to_string()))?,
    ))
}

fn identifiers(patient: &Value) -> Vec<String> {
    patient
        .get("identifier")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|held| {
                    let value = held.get("value").and_then(Value::as_str)?;
                    Some(match held.get("system").and_then(Value::as_str) {
                        Some(system) => format!("{system}|{value}"),
                        None => value.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub async fn includes_type(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let resource_type = served(state.version, &type_name)?;
    let path = format!("/{resource_type}/$includes");
    related(&state, Some(resource_type), path, query, &headers).await
}

pub async fn includes_system(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    related(&state, None, "/$includes".to_owned(), query, &headers).await
}

async fn related(
    state: &AppState,
    base_type: Option<ResourceType>,
    path: String,
    query: Option<String>,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let request = SearchRequest::parse(&state.registry, base_type, query.as_deref())?;
    if request.query.includes.is_empty() {
        return Err(Error::InvalidParameter(
            "the operation needs an include".to_owned(),
        )
        .into());
    }
    let control = ResultControl::parse(query.as_deref())?;
    let mut selection = SearchQuery {
        count: usize::MAX,
        offset: 0,
        ..request.query
    };
    crate::handlers::confine(&mut selection, crate::handlers::grant_of(headers)?)?;
    crate::terminology::resolve(state.terminology.as_ref(), &mut selection).await?;
    let found = state.store.search(&selection).await?;
    let total = found.included.len();
    let entries: Vec<fhir_core::ResourceEnvelope> = found
        .included
        .into_iter()
        .skip(control.offset)
        .take(control.count)
        .collect();
    let page = fhir_store::SearchPage {
        entries,
        included: Vec::new(),
        total: Some(total),
        offset: control.offset,
    };
    let base = format!("http://{}", crate::handlers::host_from(headers));
    let self_url = match query.as_deref() {
        Some(raw) if !raw.is_empty() => format!("{base}{path}?{raw}"),
        _ => format!("{base}{path}"),
    };
    Ok(rendered(crate::search::includes_bundle(
        &base,
        &self_url,
        &page,
        control.summary,
        &control.elements,
    )))
}

pub(crate) const DOCREF_PARAMS: [&str; 5] = ["patient", "start", "end", "type", "on-demand"];

pub async fn docref_query(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = from_query(query.as_deref())?;
    documents(&state, asked, &headers).await
}

pub async fn docref_body(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = match body.iter().all(u8::is_ascii_whitespace) {
        true => from_query(None)?,
        false => from_parameters(&parameters(&body)?)?,
    };
    documents(&state, asked, &headers).await
}

fn from_query(query: Option<&str>) -> Result<Vec<(String, String)>, Error> {
    accepts(query, &DOCREF_PARAMS)?;
    Ok(crate::query::pairs(query))
}

fn from_parameters(input: &Value) -> Result<Vec<(String, String)>, Error> {
    let mut asked = Vec::new();
    for name in DOCREF_PARAMS.iter().chain(CONTROL.iter()) {
        for value in values_of(input, name) {
            asked.push(((*name).to_owned(), value));
        }
    }
    for held in values_of(input, "_include") {
        asked.push(("_include".to_owned(), held));
    }
    Ok(asked)
}

async fn documents(
    state: &AppState,
    asked: Vec<(String, String)>,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let held = |name: &str| {
        asked
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.clone())
    };
    let patient = held("patient")
        .ok_or_else(|| Error::InvalidParameter("\"patient\" is missing".to_owned()))?;
    if held("on-demand").is_some_and(|value| value.eq_ignore_ascii_case("true")) {
        return Err(Error::UnsupportedParameter(
            "\"on-demand\" generation".to_owned(),
        )
        .into());
    }
    let mut selection: Vec<(String, String)> = vec![(
        "patient".to_owned(),
        patient.rsplit('/').next().unwrap_or(&patient).to_owned(),
    )];
    if let Some(start) = held("start") {
        selection.push(("date".to_owned(), format!("ge{start}")));
    }
    if let Some(end) = held("end") {
        selection.push(("date".to_owned(), format!("le{end}")));
    }
    if let Some(kind) = held("type") {
        selection.push(("type".to_owned(), kind));
    }
    for (name, value) in &asked {
        if CONTROL.contains(&name.as_str()) || name == "_include" {
            selection.push((name.clone(), value.clone()));
        }
    }
    let raw = selection
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<String>>()
        .join("&");
    let resource_type = "DocumentReference".parse::<ResourceType>()?;
    let request = SearchRequest::parse(&state.registry, Some(resource_type), Some(&raw))?;
    let path = format!("/{resource_type}/$docref");
    crate::handlers::respond_page(state, request, path, Some(raw), headers).await
}

pub(crate) const EXPAND_PARAMS: [&str; 11] = [
    "url",
    "filter",
    "count",
    "offset",
    "date",
    "activeOnly",
    "displayLanguage",
    "includeDesignations",
    "designations",
    "excludeNested",
    "valueSetVersion",
];

const SYSTEM_VERSION: &str = "system-version";

pub async fn expand_query(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = crate::query::pairs(query.as_deref());
    accepts(query.as_deref(), &[&EXPAND_PARAMS[..], &[SYSTEM_VERSION]].concat())?;
    expanded(&state, &asked).await
}

pub async fn expand_body(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let input = parameters(&body)?;
    let mut asked = Vec::new();
    for name in EXPAND_PARAMS.iter().chain([SYSTEM_VERSION].iter()) {
        for value in values_of(&input, name) {
            asked.push(((*name).to_owned(), value));
        }
    }
    expanded(&state, &asked).await
}

async fn expanded(state: &AppState, asked: &[(String, String)]) -> Result<Response, AppError> {
    let held = |name: &str| {
        asked
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.clone())
    };
    let url = held("url").ok_or_else(|| Error::InvalidParameter("\"url\" is missing".to_owned()))?;
    let request = ExpansionRequest {
        filter: held("filter"),
        count: match held("count") {
            None => None,
            Some(text) => Some(
                text.parse::<usize>()
                    .map_err(|_| Error::InvalidParameter(format!("count {text:?}")))?,
            ),
        },
        offset: match held("offset") {
            None => 0,
            Some(text) => text
                .parse::<usize>()
                .map_err(|_| Error::InvalidParameter(format!("offset {text:?}")))?,
        },
        date: held("date"),
        active_only: flag(held("activeOnly").as_deref())?,
        display_language: held("displayLanguage"),
        designations: flag(held("includeDesignations").as_deref())?
            || flag(held("designations").as_deref())?,
        exclude_nested: flag(held("excludeNested").as_deref())?,
        system_versions: asked
            .iter()
            .filter(|(name, _)| name == SYSTEM_VERSION)
            .map(|(_, value)| match value.split_once('|') {
                Some((system, version)) => Ok((system.to_owned(), version.to_owned())),
                None => Err(Error::InvalidParameter(format!(
                    "{SYSTEM_VERSION} {value:?}"
                ))),
            })
            .collect::<Result<Vec<(String, String)>, Error>>()?,
        value_set_version: held("valueSetVersion"),
    };
    let expansion = state.terminology.expand(&url, &request).await?;
    let stamp = Stamp {
        identifier: format!("urn:uuid:{}", uuid::Uuid::new_v4()),
        timestamp: fhir_store::system_clock()().as_str().to_owned(),
    };
    let body = expansion_json(&expansion, &request, &stamp);
    Ok(rendered(
        serde_json::to_vec(&body).map_err(|error| Error::Internal(error.to_string()))?,
    ))
}

fn flag(value: Option<&str>) -> Result<bool, Error> {
    match value {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(other) => Err(Error::InvalidParameter(format!("{other:?} is not a flag"))),
    }
}

pub(crate) fn rendered(body: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, FHIR_JSON),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_are_read_by_name() {
        let body = serde_json::json!({
            "resourceType": "Parameters",
            "parameter": [
                {"name": "url", "valueUri": "urn:x"},
                {"name": "count", "valueInteger": 5},
                {"name": "active", "valueBoolean": true},
                {"name": "code", "valueCode": "a"},
                {"name": "code", "valueCode": "b"},
                {"name": "patient", "resource": {"resourceType": "Patient"}},
                {"name": "empty"}
            ]
        });
        assert_eq!(value_of(&body, "url"), Some("urn:x".to_owned()));
        assert_eq!(value_of(&body, "count"), Some("5".to_owned()));
        assert_eq!(value_of(&body, "active"), Some("true".to_owned()));
        assert_eq!(values_of(&body, "code"), vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(value_of(&body, "empty"), None);
        assert_eq!(value_of(&body, "nonesuch"), None);
        assert_eq!(
            resource_of(&body, "patient").and_then(|found| found["resourceType"].as_str()),
            Some("Patient")
        );
        assert!(resource_of(&body, "url").is_none());
        assert!(values_of(&serde_json::json!({}), "code").is_empty());
    }

    #[test]
    fn a_body_that_is_not_a_parameters_resource_is_refused() {
        assert!(matches!(
            parameters(b"not json").unwrap_err(),
            Error::InvalidJson(_)
        ));
        assert!(matches!(
            parameters(br#"{"resourceType":"Patient"}"#).unwrap_err(),
            Error::InvalidEnvelope(_)
        ));
        let body = parameters(br#"{"resourceType":"Parameters"}"#).unwrap();
        assert!(matches!(
            required(&body, "url").unwrap_err(),
            Error::InvalidParameter(_)
        ));
    }
}
