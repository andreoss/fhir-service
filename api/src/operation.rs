use crate::preference::Handling;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::header::{self, HeaderMap};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fhir_core::convert::{convert, Conversion, InputType};
use fhir_core::search::{Compartment, Filter, Target};
use fhir_core::terminology::{expansion_json, ExpansionRequest, Stamp};
use fhir_core::validate::{validate, Mode, Request as ValidationRequest};
use fhir_core::{Error, ResourceId, ResourceType};
use fhir_store::SearchQuery;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::app::AppState;
use crate::handlers::{allowed, AppError};
use crate::query::param;
use crate::search::{ResultControl, SearchRequest, CONTROL};
use fhir_core::security::scope::DataAction;

const FHIR_JSON: &str = "application/fhir+json";

pub fn value_of(body: &Value, name: &str) -> Option<String> {
    entries(body).find_map(|entry| match entry.get("name").and_then(Value::as_str) {
        Some(held) if held == name => primitive_of(entry),
        _ => None,
    })
}

pub fn values_of(body: &Value, name: &str) -> Vec<String> {
    entries(body)
        .filter(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        .filter_map(primitive_of)
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

pub(crate) fn primitive_of(entry: &Value) -> Option<String> {
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
    let resource_type = crate::handlers::served_here(&state, &type_name)?;
    allowed(
        &state,
        &headers,
        DataAction::Read,
        Some(resource_type),
        None,
    )
    .await?;
    validated(&state, Some(resource_type), None, query.as_deref(), &body).await
}

pub async fn validate_instance(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = crate::handlers::served_here(&state, &type_name)?;
    allowed(
        &state,
        &headers,
        DataAction::Read,
        Some(resource_type),
        None,
    )
    .await?;
    let id = id_text.parse::<ResourceId>()?;
    validated(
        &state,
        Some(resource_type),
        Some(id),
        query.as_deref(),
        &body,
    )
    .await
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
        Some(Value::Object(ref object))
            if object.get("resourceType") == Some(&Value::String("Parameters".to_owned())) =>
        {
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
            let held = resource_type.ok_or_else(|| {
                Error::InvalidParameter("a stored resource is named by its type".to_owned())
            })?;
            let stored = state
                .store
                .read(&fhir_core::ResourceKey::new(held, id.clone()))
                .await?;
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
    let claimed = profile.as_deref().or_else(|| declared(&body));
    let held = match claimed {
        None => None,
        Some(url) => crate::profile::resolve(&state.store, url).await?,
    };
    let codes = match &held {
        None => None,
        Some(profile) => {
            Some(crate::profile::HeldCodes::for_profile(state.terminology.as_ref(), profile).await?)
        }
    };
    let resolved = match (&held, &codes) {
        (Some(profile), Some(codes)) => Some(fhir_core::validate::Resolved { profile, codes }),
        _ => None,
    };
    let report = validate(&ValidationRequest {
        version: state.version,
        resource_type,
        id,
        profile: claimed,
        resolved,
        mode,
        unresolved: match state.profiles.asked() {
            true => fhir_core::validate::Unresolved::Required,
            false => fhir_core::validate::Unresolved::Reported,
        },
        body: &body,
    });
    Ok(rendered(report.to_fhir_json()))
}

fn declared(body: &Value) -> Option<&str> {
    body.get("meta")?
        .get("profile")?
        .as_array()?
        .iter()
        .find_map(Value::as_str)
}

fn submitted(body: &[u8]) -> Result<Option<Value>, Error> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(body)
        .map(Some)
        .map_err(|error| Error::InvalidJson(error.to_string()))
}

pub(crate) const EVERYTHING_PARAMS: [&str; 4] = ["_since", "_type", "start", "end"];

pub async fn everything(
    State(state): State<AppState>,
    Path(id_text): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let root = "Patient".parse::<ResourceType>()?;
    let id = id_text.parse::<ResourceId>()?;
    let stored = state
        .store
        .read(&fhir_core::ResourceKey::new(root, id.clone()))
        .await?;
    if stored.is_deleted() {
        return Err(Error::NotFound.into());
    }
    let def = fhir_core::search::compartment::definition(root.as_str())
        .ok_or_else(|| Error::UnsupportedParameter("compartment \"Patient\"".to_owned()))?;
    let handling = Handling::asked_for(&headers)?;
    let (query, dropped) = accepted(query.as_deref(), &EVERYTHING_PARAMS, handling)?;
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
            filters: window(&state, query.as_deref(), &types)?,
            types,
            compartment: Some(Compartment { kind: root, id }),
            count: control.count,
            offset: control.offset,
            total: control.total,
            ..SearchQuery::default()
        },
        summary: control.summary,
        elements: control.elements,
        dropped,
        named: crate::search::Named {
            count: crate::search::param(query.as_deref(), "_count").is_some(),
            sort: crate::search::param(query.as_deref(), "_sort").is_some(),
            total: crate::search::param(query.as_deref(), "_total").is_some(),
        },
    };
    let path = format!("/{}/{}/$everything", root.as_str(), id_text);
    crate::handlers::respond_page(&state, request, path, query, &headers).await
}

const CLINICAL_DATE: &str = "date";

type DateGroup = (Vec<String>, Vec<ResourceType>);

fn window(
    state: &AppState,
    query: Option<&str>,
    types: &[ResourceType],
) -> Result<Vec<Filter>, Error> {
    let mut filters = Vec::new();
    if let Some(text) = param(query, "_since") {
        let def = state
            .registry
            .searchable(None, "_lastUpdated")?
            .ok_or_else(|| Error::UnsupportedParameter("\"_since\"".to_owned()))?;
        let value = def
            .value_with(&fhir_core::search::Modifier::None, &format!("ge{text}"))
            .map_err(|_| Error::InvalidParameter(format!("_since {text:?}")))?;
        filters.push(Filter::new("_since", def.target.clone(), vec![value]));
    }
    let start = param(query, "start");
    let end = param(query, "end");
    if let (Some(from), Some(till)) = (start.as_deref(), end.as_deref()) {
        if from > till {
            return Err(Error::InvalidParameter(format!(
                "start {from:?} falls after end {till:?}"
            )));
        }
    }
    if start.is_none() && end.is_none() {
        return Ok(filters);
    }
    for (name, text, comparator) in [("start", start, "ge"), ("end", end, "le")] {
        let Some(text) = text else {
            continue;
        };
        for (paths, kinds) in dated(state, types)? {
            let def = state
                .registry
                .searchable(Some(kinds[0]), CLINICAL_DATE)?
                .ok_or_else(|| Error::UnsupportedParameter(format!("{name:?}")))?;
            let value = def
                .value_with(
                    &fhir_core::search::Modifier::None,
                    &format!("{comparator}{text}"),
                )
                .map_err(|_| Error::InvalidParameter(format!("{name} {text:?}")))?;
            let exempt = types
                .iter()
                .filter(|held| !kinds.contains(held))
                .copied()
                .collect();
            filters.push(
                Filter::new(CLINICAL_DATE, Target::Path(paths), vec![value]).exempting(exempt),
            );
        }
    }
    Ok(filters)
}

fn dated(state: &AppState, types: &[ResourceType]) -> Result<Vec<DateGroup>, Error> {
    let mut groups: BTreeMap<Vec<String>, Vec<ResourceType>> = BTreeMap::new();
    for kind in types {
        let Some(def) = state.registry.searchable(Some(*kind), CLINICAL_DATE)? else {
            continue;
        };
        if let Target::Path(paths) = &def.target {
            groups.entry(paths.clone()).or_default().push(*kind);
        }
    }
    Ok(groups.into_iter().collect())
}

fn listed(raw: &str) -> Result<Vec<ResourceType>, Error> {
    raw.split(',')
        .filter(|part| !part.is_empty())
        .map(str::parse::<ResourceType>)
        .collect()
}

fn accepted(
    query: Option<&str>,
    allowed: &[&str],
    handling: Handling,
) -> Result<(Option<String>, Vec<String>), Error> {
    let mut kept: Vec<&str> = Vec::new();
    let mut dropped = Vec::new();
    for segment in query
        .unwrap_or_default()
        .split('&')
        .filter(|part| !part.is_empty())
    {
        let name = crate::query::decoded(
            segment
                .split_once('=')
                .map(|(held, _)| held)
                .unwrap_or(segment),
        );
        let known = allowed.contains(&name.as_str()) || CONTROL.contains(&name.as_str());
        match (known, handling.is_lenient()) {
            (true, _) => kept.push(segment),
            (false, true) => dropped.push(name),
            (false, false) => return Err(Error::UnsupportedParameter(format!("{name:?}"))),
        }
    }
    let kept = match kept.is_empty() {
        true => None,
        false => Some(kept.join("&")),
    };
    Ok((kept, dropped))
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
    let submitted = resource_of(&input, "MemberPatient")
        .ok_or_else(|| Error::InvalidParameter("\"MemberPatient\" is missing".to_owned()))?;
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
    let resource_type = crate::handlers::served_here(&state, &type_name)?;
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
    let (request, query) =
        crate::handlers::parsed_search(state, base_type, query.as_deref(), headers)?;
    if request.query.includes.is_empty() {
        return Err(Error::InvalidParameter("the operation needs an include".to_owned()).into());
    }
    let control = ResultControl::parse(query.as_deref())?;
    let mut selection = SearchQuery {
        count: usize::MAX,
        offset: 0,
        include_depth: state.capabilities.include_depth,
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
        bounded: found.bounded,
    };
    let base = crate::handlers::addressed(state, headers);
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
        &request.dropped,
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
        return Err(Error::UnsupportedParameter("\"on-demand\" generation".to_owned()).into());
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

pub(crate) const DOCUMENT_PARAMS: [&str; 2] = ["id", "persist"];

const COMPOSITION: &str = "Composition";

pub async fn document(
    State(state): State<AppState>,
    Path(id_text): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let composition = COMPOSITION.parse::<ResourceType>()?;
    allowed(&state, &headers, DataAction::Read, Some(composition), None).await?;
    let persist =
        param(query.as_deref(), "persist").is_some_and(|held| held.eq_ignore_ascii_case("true"));
    Ok(gathered_document(&state, &id_text, persist, &headers).await?)
}

pub async fn document_type(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let composition = COMPOSITION.parse::<ResourceType>()?;
    allowed(&state, &headers, DataAction::Read, Some(composition), None).await?;
    let asked: Value = match body.iter().all(u8::is_ascii_whitespace) {
        true => Value::Null,
        false => {
            serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?
        }
    };
    let id = param(query.as_deref(), "id")
        .or_else(|| value_of(&asked, "id"))
        .ok_or_else(|| Error::InvalidParameter("no composition is named".to_owned()))?;
    let persist = param(query.as_deref(), "persist")
        .or_else(|| value_of(&asked, "persist"))
        .is_some_and(|held| held.eq_ignore_ascii_case("true"));
    Ok(gathered_document(&state, &id, persist, &headers).await?)
}

async fn gathered_document(
    state: &AppState,
    id_text: &str,
    persist: bool,
    headers: &HeaderMap,
) -> Result<Response, Error> {
    let composition = COMPOSITION.parse::<ResourceType>()?;
    let id = id_text.parse::<ResourceId>()?;
    let root = state
        .store
        .read(&fhir_core::ResourceKey::new(composition, id))
        .await?;
    if root.is_deleted() {
        return Err(Error::Deleted);
    }
    let base = crate::handlers::addressed(state, headers);
    let mut entries: Vec<Value> = Vec::new();
    let mut issues: Vec<Value> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let body: Value = serde_json::from_slice(root.raw())
        .map_err(|error| Error::InvalidJson(error.to_string()))?;
    seen.insert(format!("{COMPOSITION}/{id_text}"));
    entries.push(json!({
        "fullUrl": format!("{base}/{COMPOSITION}/{id_text}"),
        "resource": body,
    }));

    let mut frontier: Vec<Value> = vec![entries[0]["resource"].clone()];
    while let Some(held) = frontier.pop() {
        for reference in references_of(&held) {
            if reference.contains("://") {
                issues.push(outcome_issue(&format!(
                    "{reference} is outside this instance and was not followed"
                )));
                continue;
            }
            if !seen.insert(reference.clone()) {
                continue;
            }
            let Ok(key) = reference.parse::<fhir_core::ResourceKey>() else {
                issues.push(outcome_issue(&format!("{reference} names no resource")));
                continue;
            };
            match state.store.read(&key).await {
                Ok(found) if !found.is_deleted() => {
                    let body: Value = serde_json::from_slice(found.raw())
                        .map_err(|error| Error::InvalidJson(error.to_string()))?;
                    entries.push(json!({
                        "fullUrl": format!("{base}/{reference}"),
                        "resource": body.clone(),
                    }));
                    frontier.push(body);
                }
                Ok(_) | Err(Error::NotFound) => issues.push(outcome_issue(&format!(
                    "{reference} is not held by this instance"
                ))),
                Err(error) => return Err(error),
            }
        }
    }
    if !issues.is_empty() {
        entries.push(json!({
            "fullUrl": format!("{base}/OperationOutcome/document"),
            "resource": {"resourceType": "OperationOutcome", "issue": issues},
        }));
    }
    let bundle = json!({
        "resourceType": "Bundle",
        "id": uuid::Uuid::new_v4().to_string(),
        "type": "document",
        "timestamp": fhir_store::system_clock()().as_str(),
        "entry": entries,
    });
    if persist {
        let mut held = bundle.clone();
        fhir_core::with_assigned_meta(&mut held)?;
        let stored = fhir_core::ResourceEnvelope::parse(
            state.version,
            &serde_json::to_vec(&held).map_err(|error| Error::Internal(error.to_string()))?,
        )?;
        state.store.create(stored).await?;
    }
    Ok(crate::handlers::rendered(
        serde_json::to_vec(&bundle).map_err(|error| Error::Internal(error.to_string()))?,
    ))
}

fn outcome_issue(diagnostics: &str) -> Value {
    json!({
        "severity": "warning",
        "code": "not-found",
        "diagnostics": diagnostics,
    })
}

fn references_of(body: &Value) -> Vec<String> {
    let mut held = Vec::new();
    gather_references(body, &mut held);
    held
}

fn gather_references(value: &Value, held: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("reference").and_then(Value::as_str) {
                if !reference.starts_with('#') {
                    held.push(reference.to_owned());
                }
            }
            for (name, inner) in object {
                if name != "reference" {
                    gather_references(inner, held);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                gather_references(item, held);
            }
        }
        _ => {}
    }
}

pub(crate) const LASTN_PARAMS: [&str; 3] = ["patient", "subject", "category"];

pub async fn last_n(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let observation = "Observation".parse::<ResourceType>()?;
    allowed(&state, &headers, DataAction::Read, Some(observation), None).await?;
    for refused in ["_sort", "_count", "ct"] {
        if param(query.as_deref(), refused).is_some() {
            return Err(Error::UnsupportedParameter(format!(
                "{refused:?} is not a parameter of $lastn: the operation orders and bounds its own answer"
            ))
            .into());
        }
    }
    let subject = param(query.as_deref(), "patient")
        .or_else(|| param(query.as_deref(), "subject"))
        .ok_or_else(|| Error::InvalidParameter("$lastn names a patient or a subject".to_owned()))?;
    if param(query.as_deref(), "category").is_none() {
        return Err(Error::InvalidParameter("$lastn names a category".to_owned()).into());
    }
    let most = match param(query.as_deref(), "max") {
        None => 1,
        Some(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|held| *held > 0)
            .ok_or_else(|| Error::InvalidParameter(format!("max {raw:?} is not a count")))?,
    };

    let mut narrowing: Vec<String> = Vec::new();
    for (name, value) in crate::query::pairs(query.as_deref()) {
        if name == "max" || LASTN_PARAMS.contains(&name.as_str()) {
            continue;
        }
        narrowing.push(format!("{name}={value}"));
    }
    let held = subject.rsplit('/').next().unwrap_or(&subject).to_owned();
    narrowing.push(format!("subject=Patient/{held}"));
    if let Some(category) = param(query.as_deref(), "category") {
        narrowing.push(format!("category={category}"));
    }
    let raw = narrowing.join("&");
    let mut request = SearchRequest::parse(&state.registry, Some(observation), Some(&raw))?;
    request.query.count = usize::MAX;
    request.query.offset = 0;
    request.query.sort = sorted_by_date(&state, observation)?;
    crate::handlers::confine(&mut request.query, crate::handlers::grant_of(&headers)?)?;
    let found = state.store.search(&request.query).await?;

    let mut groups: BTreeMap<String, Vec<fhir_core::ResourceEnvelope>> = BTreeMap::new();
    for entry in found.entries {
        let body: Value = serde_json::from_slice(entry.raw())
            .map_err(|error| Error::InvalidJson(error.to_string()))?;
        let key = coded(&body);
        let held = groups.entry(key).or_default();
        if held.len() < most {
            held.push(entry);
        }
    }
    let entries: Vec<fhir_core::ResourceEnvelope> = groups.into_values().flatten().collect();
    let total = entries.len();
    let page = fhir_store::SearchPage {
        entries,
        included: Vec::new(),
        total: Some(total),
        offset: 0,
        bounded: false,
    };
    let base = crate::handlers::addressed(&state, &headers);
    let self_url = match query.as_deref() {
        Some(raw) if !raw.is_empty() => format!("{base}/Observation/$lastn?{raw}"),
        _ => format!("{base}/Observation/$lastn"),
    };
    Ok(crate::handlers::rendered(crate::search::search_bundle(
        &base,
        &self_url,
        &page,
        crate::history::Summary::Full,
        &[],
        &[],
    )))
}

fn sorted_by_date(
    state: &AppState,
    observation: ResourceType,
) -> Result<Vec<fhir_store::SortKey>, Error> {
    let def = state
        .registry
        .searchable(Some(observation), CLINICAL_DATE)?
        .ok_or_else(|| {
            Error::UnsupportedParameter("this release gives an Observation no date".to_owned())
        })?;
    Ok(vec![fhir_store::SortKey {
        name: CLINICAL_DATE.to_owned(),
        target: def.target.clone(),
        direction: fhir_store::SortDirection::Descending,
    }])
}

fn coded(body: &Value) -> String {
    let code = body.get("code");
    let coding = code
        .and_then(|held| held.get("coding"))
        .and_then(Value::as_array)
        .and_then(|items| items.first());
    match coding {
        Some(one) => format!(
            "{}|{}",
            one.get("system")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            one.get("code").and_then(Value::as_str).unwrap_or_default()
        ),
        None => code
            .and_then(|held| held.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

pub async fn snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let definition: Value =
        serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let held = fhir_core::snapshot::generate(state.version, &definition)?;
    Ok(crate::handlers::rendered(
        serde_json::to_vec(&held).map_err(|error| Error::Internal(error.to_string()))?,
    ))
}

pub async fn represented(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(crate::representation::MediaType::parse)
        .is_none()
    {
        return Err(Error::UnsupportedFormat(
            "the content type names no representation".to_owned(),
        )
        .into());
    }

    let value: Value =
        serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let named = value
        .get("resourceType")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidEnvelope("the body names no resource type".to_owned()))?;
    crate::handlers::served_here(&state, named)?;
    let findings = fhir_core::Model::of(state.version).check(&value);
    if let Some(first) = findings.first() {
        return Err(Error::InvalidEnvelope(format!(
            "{} at {}: {}",
            first.rule, first.path, first.detail
        ))
        .into());
    }
    Ok(crate::handlers::rendered(
        serde_json::to_vec(&value).map_err(|error| Error::Internal(error.to_string()))?,
    ))
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
    accepts(
        query.as_deref(),
        &[&EXPAND_PARAMS[..], &[SYSTEM_VERSION]].concat(),
    )?;
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
    let url =
        held("url").ok_or_else(|| Error::InvalidParameter("\"url\" is missing".to_owned()))?;
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
        assert_eq!(
            values_of(&body, "code"),
            vec!["a".to_owned(), "b".to_owned()]
        );
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
