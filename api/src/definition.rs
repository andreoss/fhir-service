use axum::extract::{Path, State};
use axum::http::header::{self, HeaderMap};
use axum::response::{IntoResponse, Response};
use fhir_core::Error;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::app::{AppState, Verb};
use crate::capability::{base_of, operations, Level, Operation};
use crate::handlers::AppError;

const FHIR_JSON: &str = "application/fhir+json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationParam {
    pub name: &'static str,
    pub required: bool,
    pub kind: &'static str,
}

const fn optional(name: &'static str) -> OperationParam {
    OperationParam {
        name,
        required: false,
        kind: "string",
    }
}

const fn insisted(name: &'static str, kind: &'static str) -> OperationParam {
    OperationParam {
        name,
        required: true,
        kind,
    }
}

const CONVERT: &[OperationParam] = &[
    insisted("inputData", "string"),
    insisted("inputDataType", "code"),
    insisted("templateCollectionReference", "string"),
    insisted("rootTemplate", "string"),
];

const MEMBER_MATCH: &[OperationParam] = &[insisted("MemberPatient", "Patient")];

const VALIDATE: &[OperationParam] = &[
    optional("mode"),
    optional("profile"),
    OperationParam {
        name: "resource",
        required: false,
        kind: "Resource",
    },
];

const IMPORT: &[OperationParam] = &[OperationParam {
    name: "resources",
    required: false,
    kind: "Resource",
}];

const STATUS: &[OperationParam] = &[optional("url"), optional("status")];

fn listed(names: &'static [&'static str]) -> Vec<OperationParam> {
    names.iter().map(|name| optional(name)).collect()
}

pub fn inputs(code: &str) -> Vec<OperationParam> {
    match code {
        "export" => listed(&crate::job::ACCEPTED_PARAMS),
        "bulk-delete" | "bulk-delete-soft-deleted" => listed(&crate::job::DELETE_PARAMS),
        "bulk-update" => listed(&crate::job::UPDATE_PARAMS),
        "reindex" => {
            let mut found = listed(&crate::job::REINDEX_PARAMS);
            found.push(optional("url"));
            found
        }
        "everything" => listed(&crate::operation::EVERYTHING_PARAMS),
        "docref" => listed(&crate::operation::DOCREF_PARAMS),
        "expand" => listed(&crate::operation::EXPAND_PARAMS),
        "includes" => listed(&crate::search::CONTROL),
        "convert-data" => CONVERT.to_vec(),
        "member-match" => MEMBER_MATCH.to_vec(),
        "validate" => VALIDATE.to_vec(),
        "import" => IMPORT.to_vec(),
        "status" => STATUS.to_vec(),
        _ => Vec::new(),
    }
}

fn output(code: &str) -> Option<&'static str> {
    match code {
        "everything" | "includes" | "docref" => Some("Bundle"),
        "expand" => Some("ValueSet"),
        "validate" => Some("OperationOutcome"),
        "convert-data" => Some("Resource"),
        "status" | "refresh" | "member-match" | "purge-history" => Some("Parameters"),
        _ => None,
    }
}

fn parameters(code: &str) -> Vec<Value> {
    let mut listed: Vec<Value> = inputs(code)
        .into_iter()
        .map(|param| {
            json!({
                "name": param.name,
                "use": "in",
                "min": u8::from(param.required),
                "max": "*",
                "type": param.kind,
            })
        })
        .collect();
    if let Some(kind) = output(code) {
        listed.push(json!({
            "name": "return",
            "use": "out",
            "min": 1,
            "max": "1",
            "type": kind,
        }));
    }
    listed
}

pub fn definition_json(operation: &Operation, base: &str) -> Value {
    json!({
        "resourceType": "OperationDefinition",
        "id": operation.code,
        "url": operation.definition(base),
        "name": operation.code,
        "status": "active",
        "kind": "operation",
        "code": operation.code,
        "affectsState": !operation.methods.contains(&Verb::Get),
        "system": operation.levels.contains(&Level::System),
        "type": operation.levels.contains(&Level::Type),
        "instance": operation.levels.contains(&Level::Instance),
        "resource": operation.types.iter().collect::<Vec<&String>>(),
        "parameter": parameters(&operation.code),
    })
}

pub fn definitions_bundle(base: &str, self_url: &str) -> Vec<u8> {
    let entries: Vec<Value> = operations()
        .iter()
        .map(|operation| {
            json!({
                "fullUrl": operation.definition(base),
                "resource": definition_json(operation, base),
                "search": {"mode": "match"},
            })
        })
        .collect();
    let mut bundle = Map::new();
    bundle.insert("resourceType".to_owned(), Value::String("Bundle".to_owned()));
    bundle.insert("id".to_owned(), Value::String(Uuid::new_v4().to_string()));
    bundle.insert("type".to_owned(), Value::String("searchset".to_owned()));
    bundle.insert("total".to_owned(), Value::from(entries.len()));
    bundle.insert(
        "link".to_owned(),
        Value::Array(vec![json!({"relation": "self", "url": self_url})]),
    );
    bundle.insert("entry".to_owned(), Value::Array(entries));
    serde_json::to_vec(&Value::Object(bundle)).expect("bundle is serializable")
}

fn rendered(body: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, FHIR_JSON),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

pub async fn operation_definitions(
    State(_state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let base = base_of(&headers);
    let self_url = format!("{base}/OperationDefinition");
    Ok(rendered(definitions_bundle(&base, &self_url)))
}

pub async fn operation_definition(
    State(_state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let base = base_of(&headers);
    let found = operations()
        .into_iter()
        .find(|operation| operation.code == code)
        .ok_or(Error::NotFound)?;
    let body = serde_json::to_vec(&definition_json(&found, &base))
        .map_err(|error| Error::Internal(error.to_string()))?;
    Ok(rendered(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation(code: &str) -> Operation {
        operations()
            .into_iter()
            .find(|held| held.code == code)
            .expect("the operation is routed")
    }

    #[test]
    fn a_read_operation_leaves_state_alone() {
        let value = definition_json(&operation("validate"), "http://localhost");
        assert_eq!(value["affectsState"], Value::Bool(false));
        let submitted = definition_json(&operation("import"), "http://localhost");
        assert_eq!(submitted["affectsState"], Value::Bool(true));
    }

    #[test]
    fn every_routed_operation_declares_its_parameters() {
        for operation in operations() {
            let value = definition_json(&operation, "http://localhost");
            assert_eq!(value["code"], operation.code.as_str());
            assert!(value["parameter"].is_array());
        }
    }
}
