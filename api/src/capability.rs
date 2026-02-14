use axum::extract::State;
use axum::http::header::{self, HeaderMap};
use axum::response::{IntoResponse, Response};
use fhir_core::{FhirVersion, ResourceType};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::app::{served, AppState, Route, Verb};
use crate::handlers::AppError;

const FHIR_JSON: &str = "application/fhir+json";

pub const BUILD: &str = env!("CARGO_PKG_VERSION");

pub const SOFTWARE: &str = "specification server";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    System,
    Type,
    Instance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    pub code: String,
    pub levels: BTreeSet<Level>,
    pub types: BTreeSet<String>,
    pub methods: BTreeSet<Verb>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Every,
    Named(&'static str),
}

fn parts(path: &'static str) -> Vec<&'static str> {
    path.split('/').filter(|part| !part.is_empty()).collect()
}

fn scope_of(segment: &'static str) -> Option<Scope> {
    match segment {
        "{type}" => Some(Scope::Every),
        name if name.parse::<ResourceType>().is_ok() => Some(Scope::Named(name)),
        _ => None,
    }
}

fn operation_of(route: &Route) -> Option<(String, Level, Scope)> {
    let parts = parts(route.path);
    let (last, head) = parts.split_last()?;
    let code = last.strip_prefix('$')?.to_owned();
    match head {
        [] => Some((code, Level::System, Scope::Every)),
        [kind] => scope_of(kind).map(|scope| (code, Level::Type, scope)),
        [kind, "{id}"] => scope_of(kind).map(|scope| (code, Level::Instance, scope)),
        _ => None,
    }
}

pub fn operations() -> Vec<Operation> {
    let mut found: BTreeMap<String, Operation> = BTreeMap::new();
    for route in served() {
        let Some((code, level, scope)) = operation_of(&route) else {
            continue;
        };
        let held = found.entry(code.clone()).or_insert_with(|| Operation {
            code,
            levels: BTreeSet::new(),
            types: BTreeSet::new(),
            methods: BTreeSet::new(),
        });
        held.levels.insert(level);
        held.methods.extend(route.methods.iter().copied());
        match scope {
            Scope::Every if level != Level::System => {
                held.types.clear();
                held.types.insert(String::new());
            }
            Scope::Named(name) if !held.types.contains("") => {
                held.types.insert(name.to_owned());
            }
            _ => {}
        }
    }
    found
        .into_values()
        .map(|mut held| {
            held.types.remove("");
            held
        })
        .collect()
}

impl Operation {
    pub fn applies(&self, kind: &str) -> bool {
        (self.levels.contains(&Level::Type) || self.levels.contains(&Level::Instance))
            && (self.types.is_empty() || self.types.contains(kind))
    }

    pub fn definition(&self, base: &str) -> String {
        format!("{base}/OperationDefinition/{}", self.code)
    }
}

fn interactions(route: &Route) -> Vec<(Scope, &'static str)> {
    let parts = parts(route.path);
    let verb = |wanted: Verb| route.methods.contains(&wanted);
    let mut found = Vec::new();
    match parts.as_slice() {
        [kind] if scope_of(kind).is_some() => {
            let scope = scope_of(kind).expect("the segment names a scope");
            if verb(Verb::Get) {
                found.push((scope, "search-type"));
            }
            if verb(Verb::Post) {
                found.push((scope, "create"));
            }
        }
        [kind, "_history"] if scope_of(kind).is_some() => {
            found.push((scope_of(kind).expect("the segment names a scope"), "history-type"));
        }
        [kind, "{id}"] if scope_of(kind).is_some() => {
            let scope = scope_of(kind).expect("the segment names a scope");
            for (held, code) in [
                (Verb::Get, "read"),
                (Verb::Put, "update"),
                (Verb::Delete, "delete"),
                (Verb::Patch, "patch"),
            ] {
                if verb(held) {
                    found.push((scope, code));
                }
            }
        }
        [kind, "{id}", "_history"] if scope_of(kind).is_some() => {
            found.push((
                scope_of(kind).expect("the segment names a scope"),
                "history-instance",
            ));
        }
        [kind, "{id}", "_history", "{vid}"] if scope_of(kind).is_some() => {
            found.push((scope_of(kind).expect("the segment names a scope"), "vread"));
        }
        _ => {}
    }
    found
}

fn system_interactions() -> BTreeSet<&'static str> {
    let mut found = BTreeSet::new();
    for route in served() {
        match parts(route.path).as_slice() {
            [] => {
                if route.methods.contains(&Verb::Get) {
                    found.insert("search-system");
                }
                if route.methods.contains(&Verb::Post) {
                    found.insert("transaction");
                    found.insert("batch");
                }
            }
            ["_history"] => {
                found.insert("history-system");
            }
            _ => {}
        }
    }
    found
}

struct Surface {
    every: BTreeSet<&'static str>,
    named: BTreeMap<&'static str, BTreeSet<&'static str>>,
    conditional: BTreeSet<Verb>,
}

fn surface() -> Surface {
    let mut every = BTreeSet::new();
    let mut named: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
    let mut conditional = BTreeSet::new();
    for route in served() {
        for (scope, code) in interactions(&route) {
            match scope {
                Scope::Every => {
                    every.insert(code);
                }
                Scope::Named(name) => {
                    named.entry(name).or_default().insert(code);
                }
            }
        }
        if let [kind] = parts(route.path).as_slice() {
            if scope_of(kind) == Some(Scope::Every) {
                conditional.extend(route.methods.iter().copied());
            }
        }
    }
    Surface {
        every,
        named,
        conditional,
    }
}

fn coded(codes: &BTreeSet<&'static str>) -> Value {
    Value::Array(codes.iter().map(|code| json!({"code": code})).collect())
}

fn parameter_entries(state: &AppState, kind: ResourceType) -> Value {
    let mut listed = Vec::new();
    let mut seen = BTreeSet::new();
    for def in state.registry.for_type(kind) {
        if !seen.insert(def.name.clone()) {
            continue;
        }
        let mut entry = Map::new();
        entry.insert("name".to_owned(), json!(def.name));
        entry.insert("type".to_owned(), json!(def.value_type.as_str()));
        if let Some(url) = &def.url {
            entry.insert("definition".to_owned(), json!(url));
        }
        listed.push(Value::Object(entry));
    }
    Value::Array(listed)
}

fn common_entries() -> Value {
    Value::Array(
        fhir_core::search::common()
            .iter()
            .map(|def| json!({"name": def.name, "type": def.value_type.as_str()}))
            .collect(),
    )
}

fn reverse_includes(state: &AppState) -> (BTreeMap<String, BTreeSet<String>>, BTreeSet<String>) {
    let mut per_target: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut any = BTreeSet::new();
    for kind in ResourceType::served(state.version) {
        for def in state.registry.references(kind) {
            let spelling = format!("{}:{}", kind.as_str(), def.name);
            if def.targets.is_empty() {
                any.insert(spelling);
                continue;
            }
            for target in &def.targets {
                per_target
                    .entry(target.clone())
                    .or_default()
                    .insert(spelling.clone());
            }
        }
    }
    (per_target, any)
}

fn resource_entry(
    state: &AppState,
    kind: ResourceType,
    surface: &Surface,
    operations: &[Operation],
    reverse: &(BTreeMap<String, BTreeSet<String>>, BTreeSet<String>),
    base: &str,
) -> Value {
    let name = kind.as_str().to_owned();
    let mut codes = surface.every.clone();
    if let Some(extra) = surface.named.get(name.as_str()) {
        codes.extend(extra.iter().copied());
    }
    let includes: Vec<String> = state
        .registry
        .references(kind)
        .iter()
        .map(|def| format!("{name}:{}", def.name))
        .collect();
    let mut reverses = reverse.1.clone();
    if let Some(found) = reverse.0.get(&name) {
        reverses.extend(found.iter().cloned());
    }
    let applying: Vec<Value> = operations
        .iter()
        .filter(|operation| operation.applies(&name))
        .map(|operation| declared(operation, state.version, base))
        .collect();
    let mut entry = json!({
        "type": name,
        "profile": profile_of(&name, state.version),
        "interaction": coded(&codes),
        "versioning": "versioned",
        "readHistory": true,
        "updateCreate": true,
        "conditionalCreate": surface.conditional.contains(&Verb::Post),
        "conditionalUpdate": surface.conditional.contains(&Verb::Put),
        "conditionalDelete": if surface.conditional.contains(&Verb::Delete) { "single" } else { "not-supported" },
        "referencePolicy": ["literal", "local"],
        "searchInclude": includes,
        "searchRevInclude": reverses.into_iter().collect::<Vec<String>>(),
        "searchParam": parameter_entries(state, kind),
    });
    let held = entry.as_object_mut().expect("the entry is an object");
    if patch_is_conditional(state.version) {
        held.insert(
            "conditionalPatch".to_owned(),
            json!(surface.conditional.contains(&Verb::Patch)),
        );
    }
    if operations_are_typed(state.version) {
        held.insert("operation".to_owned(), Value::Array(applying));
    }
    entry
}

fn profile_of(name: &str, version: FhirVersion) -> Value {
    let url = format!("http://hl7.org/fhir/StructureDefinition/{name}");
    match version {
        FhirVersion::Stu3 => json!({"reference": url}),
        _ => json!(url),
    }
}

fn patch_is_conditional(version: FhirVersion) -> bool {
    matches!(version, FhirVersion::R5)
}

fn operations_are_typed(version: FhirVersion) -> bool {
    !matches!(version, FhirVersion::Stu3)
}

fn declared(operation: &Operation, version: FhirVersion, base: &str) -> Value {
    let url = operation.definition(base);
    match version {
        FhirVersion::Stu3 => json!({"name": operation.code, "definition": {"reference": url}}),
        _ => json!({"name": operation.code, "definition": url}),
    }
}

pub fn statement(state: &AppState, base: &str) -> Value {
    let surface = surface();
    let operations = operations();
    let reverse = reverse_includes(state);
    let resources: Vec<Value> = ResourceType::served(state.version)
        .into_iter()
        .map(|kind| resource_entry(state, kind, &surface, &operations, &reverse, base))
        .collect();
    let system: Vec<Value> = operations
        .iter()
        .filter(|operation| {
            operation.levels.contains(&Level::System) || !operations_are_typed(state.version)
        })
        .map(|operation| declared(operation, state.version, base))
        .collect();
    let mut rest = Map::new();
    rest.insert("mode".to_owned(), json!("server"));
    if let Some(security) = crate::smart::security(state) {
        rest.insert("security".to_owned(), security);
    }
    rest.insert("interaction".to_owned(), coded(&system_interactions()));
    rest.insert("searchParam".to_owned(), common_entries());
    rest.insert("operation".to_owned(), Value::Array(system));
    rest.insert("resource".to_owned(), Value::Array(resources));
    let mut held = json!({
        "resourceType": "CapabilityStatement",
        "status": "active",
        "date": fhir_store::system_clock()().as_str(),
        "kind": "instance",
        "software": {"name": SOFTWARE, "version": BUILD},
        "implementation": {"description": "conformance of the running instance", "url": base},
        "fhirVersion": state.version.release(),
        "format": ["json", FHIR_JSON],
        "patchFormat": ["application/json-patch+json", FHIR_JSON],
        "rest": [Value::Object(rest)],
    });
    if !operations_are_typed(state.version) {
        held
            .as_object_mut()
            .expect("the statement is an object")
            .insert("acceptUnknown".to_owned(), json!("no"));
    }
    held
}

pub(crate) fn base_of(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    format!("http://{host}")
}

pub async fn capability(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let body = statement(&state, &base_of(&headers));
    Ok((
        [(header::CONTENT_TYPE, FHIR_JSON)],
        serde_json::to_vec(&body).map_err(|error| fhir_core::Error::Internal(error.to_string()))?,
    )
        .into_response())
}

pub fn versions(state: &AppState) -> Value {
    let mut listed: Vec<Value> = fhir_core::FhirVersion::ALL
        .iter()
        .map(|version| json!({"name": "version", "valueCode": version.release()}))
        .collect();
    listed.push(json!({"name": "default", "valueCode": state.version.release()}));
    listed.push(json!({"name": "build", "valueCode": BUILD}));
    json!({"resourceType": "Parameters", "parameter": listed})
}

pub async fn version_report(State(state): State<AppState>) -> Result<Response, AppError> {
    let body = serde_json::to_vec(&versions(&state))
        .map_err(|error| fhir_core::Error::Internal(error.to_string()))?;
    Ok((
        [
            (header::CONTENT_TYPE, FHIR_JSON),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response())
}
