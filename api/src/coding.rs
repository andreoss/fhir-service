use axum::extract::{Path, RawQuery, State};
use axum::http::header::HeaderMap;
use axum::response::Response;
use fhir_core::security::scope::DataAction;
use fhir_core::terminology::{concepts, flattened, Coding, ExpansionRequest};
use fhir_core::{Error, ResourceId, ResourceKey, ResourceType};
use fhir_store::Subsumption;
use serde_json::{json, Map, Value};

use crate::app::AppState;
use crate::handlers::{allowed, AppError};

struct Asked {
    query: Option<String>,
    body: Value,
}

impl Asked {
    fn new(query: Option<String>, body: &[u8]) -> Result<Asked, Error> {
        let held = match body.iter().all(u8::is_ascii_whitespace) {
            true => Value::Null,
            false => serde_json::from_slice(body)
                .map_err(|error| Error::InvalidJson(error.to_string()))?,
        };
        Ok(Asked { query, body: held })
    }

    fn text(&self, name: &str) -> Option<String> {
        crate::query::param(self.query.as_deref(), name)
            .or_else(|| crate::operation::value_of(&self.body, name))
    }

    fn resource(&self, name: &str) -> Option<Value> {
        crate::operation::resource_of(&self.body, name).cloned()
    }

    fn coding(&self) -> Option<(Option<String>, String)> {
        if let Some(code) = self.text("code") {
            return Some((self.text("system"), code));
        }
        let held = self
            .value("coding")
            .or_else(|| self.value("codeableConcept"))?;
        let one = match held.get("coding").and_then(Value::as_array) {
            Some(items) => items.first()?.clone(),
            None => held,
        };
        let code = one.get("code").and_then(Value::as_str)?.to_owned();
        Some((
            one.get("system").and_then(Value::as_str).map(str::to_owned),
            code,
        ))
    }

    fn value(&self, name: &str) -> Option<Value> {
        let items = self.body.get("parameter")?.as_array()?;
        items
            .iter()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name))?
            .as_object()?
            .iter()
            .find(|(key, _)| key.starts_with("value") && key.len() > "value".len())
            .map(|(_, value)| value.clone())
    }
}

fn answered(parameters: Vec<Value>) -> Response {
    let body = json!({"resourceType": "Parameters", "parameter": parameters});
    crate::handlers::rendered(
        serde_json::to_vec(&body).expect("a parameters resource is serializable"),
    )
}

fn told(result: bool, message: &str) -> Vec<Value> {
    vec![
        json!({"name": "result", "valueBoolean": result}),
        json!({"name": "message", "valueString": message}),
    ]
}

fn all_of(system: &Value) -> Vec<Coding> {
    flattened(&concepts(system))
}

async fn stored(state: &AppState, resource_type: &str, id: &str) -> Result<Value, Error> {
    let kind = resource_type.parse::<ResourceType>()?;
    let held = id.parse::<ResourceId>()?;
    let found = state.store.read(&ResourceKey::new(kind, held)).await?;
    serde_json::from_slice(found.raw()).map_err(|error| Error::InvalidJson(error.to_string()))
}

pub async fn value_set_validate_code(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = Asked::new(query, &body)?;
    let set = match asked.resource("valueSet") {
        Some(held) => held,
        None => {
            let url = asked
                .text("url")
                .ok_or_else(|| Error::InvalidParameter("no value set is named".to_owned()))?;
            state
                .terminology
                .value_set(&url, asked.text("valueSetVersion").as_deref())
                .await?
                .ok_or(Error::NotFound)?
        }
    };
    Ok(validated_against(&state, &set, &asked).await?)
}

pub async fn value_set_validate_code_instance(
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = Asked::new(query, &body)?;
    let set = stored(&state, "ValueSet", &id).await?;
    Ok(validated_against(&state, &set, &asked).await?)
}

async fn validated_against(
    state: &AppState,
    set: &Value,
    asked: &Asked,
) -> Result<Response, Error> {
    let (system, code) = asked
        .coding()
        .ok_or_else(|| Error::InvalidParameter("no code is named".to_owned()))?;
    let url = set
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidEnvelope("the value set names no url".to_owned()))?;
    let expansion = state
        .terminology
        .expand(url, &ExpansionRequest::default())
        .await?;
    let held = flattened(&expansion.concepts);
    let found = held.iter().find(|concept| {
        concept.code == code
            && match (&concept.system, &system) {
                (Some(one), Some(other)) => one == other,
                _ => true,
            }
    });
    let Some(found) = found else {
        return Ok(answered(told(false, &format!("{code:?} is not in {url}"))));
    };
    if let Some(display) = asked.text("display") {
        if found.display.as_deref() != Some(display.as_str()) {
            return Ok(answered(told(
                false,
                &format!("{code:?} is in {url} but is not displayed as {display:?}"),
            )));
        }
    }
    let mut parameters = told(true, &format!("{code:?} is in {url}"));
    if let Some(display) = &found.display {
        parameters.push(json!({"name": "display", "valueString": display}));
    }
    Ok(answered(parameters))
}

pub async fn code_system_validate_code(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = Asked::new(query, &body)?;
    let system = match asked.resource("codeSystem") {
        Some(held) => held,
        None => named_system(&state, &asked).await?,
    };
    Ok(code_in(&system, &asked)?)
}

pub async fn code_system_validate_code_instance(
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = Asked::new(query, &body)?;
    let system = stored(&state, "CodeSystem", &id).await?;
    Ok(code_in(&system, &asked)?)
}

async fn named_system(state: &AppState, asked: &Asked) -> Result<Value, Error> {
    let url = asked
        .text("url")
        .or_else(|| asked.text("system"))
        .ok_or_else(|| Error::InvalidParameter("no code system is named".to_owned()))?;
    state
        .terminology
        .code_system(&url, asked.text("version").as_deref())
        .await?
        .ok_or(Error::NotFound)
}

fn code_in(system: &Value, asked: &Asked) -> Result<Response, Error> {
    let (_, code) = asked
        .coding()
        .ok_or_else(|| Error::InvalidParameter("no code is named".to_owned()))?;
    let url = system
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("the system");
    let held = all_of(system);
    let Some(found) = held.iter().find(|concept| concept.code == code) else {
        return Ok(answered(told(false, &format!("{code:?} is not in {url}"))));
    };
    let mut parameters = told(true, &format!("{code:?} is in {url}"));
    if let Some(display) = &found.display {
        parameters.push(json!({"name": "display", "valueString": display}));
    }
    Ok(answered(parameters))
}

pub async fn lookup(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = Asked::new(query, &body)?;
    let system = named_system(&state, &asked).await?;
    let (_, code) = asked
        .coding()
        .ok_or_else(|| Error::InvalidParameter("no code is named".to_owned()))?;
    let held = all_of(&system);
    let found = held
        .iter()
        .find(|concept| concept.code == code)
        .ok_or(Error::NotFound)?;
    let mut parameters = vec![json!({
        "name": "name",
        "valueString": system.get("name").and_then(Value::as_str).unwrap_or_default()
    })];
    if let Some(version) = system.get("version").and_then(Value::as_str) {
        parameters.push(json!({"name": "version", "valueString": version}));
    }
    parameters.push(json!({
        "name": "display",
        "valueString": found.display.clone().unwrap_or_default()
    }));
    for designation in &found.designations {
        parameters.push(json!({
            "name": "designation",
            "part": [
                {"name": "language", "valueCode": designation.language.clone().unwrap_or_default()},
                {"name": "value", "valueString": designation.value}
            ]
        }));
    }
    Ok(answered(parameters))
}

pub async fn subsumes(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = Asked::new(query, &body)?;
    let one = asked
        .text("codeA")
        .ok_or_else(|| Error::InvalidParameter("codeA is missing".to_owned()))?;
    let other = asked
        .text("codeB")
        .ok_or_else(|| Error::InvalidParameter("codeB is missing".to_owned()))?;
    let system = asked.text("system");
    if one == other {
        return Ok(answered(vec![
            json!({"name": "outcome", "valueCode": "equivalent"}),
        ]));
    }
    let below = state
        .terminology
        .subsumption(system.as_deref(), &one, Subsumption::Below)
        .await?;
    if below.iter().any(|concept| concept.code == other) {
        return Ok(answered(vec![
            json!({"name": "outcome", "valueCode": "subsumes"}),
        ]));
    }
    let above = state
        .terminology
        .subsumption(system.as_deref(), &one, Subsumption::Above)
        .await?;
    if above.iter().any(|concept| concept.code == other) {
        return Ok(answered(vec![
            json!({"name": "outcome", "valueCode": "subsumed-by"}),
        ]));
    }
    Ok(answered(vec![
        json!({"name": "outcome", "valueCode": "not-subsumed"}),
    ]))
}

pub async fn find_matches(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = Asked::new(query, &body)?;
    let system = named_system(&state, &asked).await?;
    let wanted = asked
        .text("property.value")
        .or_else(|| asked.text("value"))
        .ok_or_else(|| Error::InvalidParameter("no property value is named".to_owned()))?;
    let exact = asked
        .text("exact")
        .is_some_and(|held| held.eq_ignore_ascii_case("true"));
    let matched: Vec<&Coding> = all_of(&system)
        .iter()
        .filter(|concept| match exact {
            true => concept.display.as_deref() == Some(wanted.as_str()) || concept.code == wanted,
            false => {
                concept.code.to_lowercase().contains(&wanted.to_lowercase())
                    || concept
                        .display
                        .as_deref()
                        .is_some_and(|held| held.to_lowercase().contains(&wanted.to_lowercase()))
            }
        })
        .cloned()
        .collect::<Vec<Coding>>()
        .leak()
        .iter()
        .collect();
    let parameters: Vec<Value> = matched
        .iter()
        .map(|concept| {
            json!({
                "name": "match",
                "part": [{
                    "name": "code",
                    "valueCoding": {
                        "system": concept.system.clone().unwrap_or_default(),
                        "code": concept.code,
                        "display": concept.display.clone().unwrap_or_default()
                    }
                }]
            })
        })
        .collect();
    Ok(answered(parameters))
}

pub async fn translate(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = Asked::new(query, &body)?;
    let maps = state
        .terminology
        .concept_maps(asked.text("url").as_deref())
        .await?;
    Ok(translated(&maps, &asked)?)
}

pub async fn translate_instance(
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = Asked::new(query, &body)?;
    let map = stored(&state, "ConceptMap", &id).await?;
    Ok(translated(std::slice::from_ref(&map), &asked)?)
}

fn translated(maps: &[Value], asked: &Asked) -> Result<Response, Error> {
    let (system, code) = asked
        .coding()
        .ok_or_else(|| Error::InvalidParameter("no code is named".to_owned()))?;
    let target = asked.text("target").or_else(|| asked.text("targetsystem"));
    let mut matches = Vec::new();
    for map in maps {
        for group in map
            .get("group")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let source = group.get("source").and_then(Value::as_str);
            if system
                .as_deref()
                .is_some_and(|wanted| source.is_some_and(|held| held != wanted))
            {
                continue;
            }
            let to = group.get("target").and_then(Value::as_str);
            if target
                .as_deref()
                .is_some_and(|wanted| to.is_some_and(|held| held != wanted))
            {
                continue;
            }
            for element in group
                .get("element")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if element.get("code").and_then(Value::as_str) != Some(code.as_str()) {
                    continue;
                }
                for held in element
                    .get("target")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let mut part = Map::new();
                    part.insert("name".to_owned(), Value::String("equivalence".to_owned()));
                    let relation = held
                        .get("equivalence")
                        .or_else(|| held.get("relationship"))
                        .and_then(Value::as_str)
                        .unwrap_or("equivalent");
                    part.insert("valueCode".to_owned(), Value::String(relation.to_owned()));
                    matches.push(json!({
                        "name": "match",
                        "part": [
                            Value::Object(part),
                            {"name": "concept", "valueCoding": {
                                "system": to.unwrap_or_default(),
                                "code": held.get("code").and_then(Value::as_str).unwrap_or_default(),
                                "display": held.get("display").and_then(Value::as_str).unwrap_or_default()
                            }}
                        ]
                    }));
                }
            }
        }
    }
    let mut parameters = vec![json!({"name": "result", "valueBoolean": !matches.is_empty()})];
    if matches.is_empty() {
        parameters.push(json!({
            "name": "message",
            "valueString": format!("no map this instance holds translates {code:?}")
        }));
    }
    parameters.extend(matches);
    Ok(answered(parameters))
}

pub async fn closure(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    Err(Error::UnsupportedParameter(
        "$closure needs a terminology service that keeps a closure table; this instance keeps none"
            .to_owned(),
    )
    .into())
}
