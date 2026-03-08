














use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use fhir_core::security::scope::DataAction;
use fhir_core::{Error, ResourceType};
use serde_json::Value;

use crate::app::AppState;
use crate::handlers::AppError;



pub const USERS: &[&str] = &["Practitioner", "Patient", "RelatedPerson", "Person"];



const KIND: &str = "resourceType";

pub async fn lookup(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    
    
    let access = crate::handlers::allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked: Value = match body.is_empty() {
        true => Value::Null,
        false => {
            serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?
        }
    };
    let wanted = crate::operation::value_of(&asked, KIND);
    let types = types_of(&state, wanted.as_deref())?;
    let query = query_of(&asked);
    if query.is_empty() {
        return Err(Error::InvalidParameter(
            "a lookup names at least one search parameter; one that named none would \
             match every user this instance holds"
                .to_owned(),
        )
        .into());
    }

    let mut found: Vec<fhir_core::ResourceEnvelope> = Vec::new();
    for resource_type in types {
        let mut request = crate::search::SearchRequest::parse(
            &state.registry,
            Some(resource_type),
            Some(&query),
        )?;
        crate::handlers::confine(
            &mut request.query,
            crate::handlers::confining(&state, &access, &headers, DataAction::Read)?,
        )?;
        
        
        request.query.count = 2;
        let page = state.store.search(&request.query).await?;
        found.extend(page.entries);
        if found.len() > 1 {
            break;
        }
    }

    match found.len() {
        0 => Err(Error::NotFound.into()),
        1 => Ok(crate::handlers::rendered(found[0].raw().to_vec())),
        held => Err(Error::Unprocessable(format!(
            "{held} users match; a lookup that guessed between them would name the \
             wrong person in a token"
        ))
        .into()),
    }
}


fn types_of(state: &AppState, wanted: Option<&str>) -> Result<Vec<ResourceType>, Error> {
    let held: Vec<ResourceType> = match wanted {
        Some(name) => vec![crate::handlers::served_here(state, name)?],
        None => USERS
            .iter()
            .filter_map(|name| name.parse::<ResourceType>().ok())
            .filter(|kind| kind.served_by(state.version) && state.restricted.serves(*kind))
            .collect(),
    };
    if let Some(refused) = held.iter().find(|kind| !USERS.contains(&kind.as_str())) {
        return Err(Error::InvalidParameter(format!(
            "{refused} is not a type a user may be; those are {}",
            USERS.join(", ")
        )));
    }
    Ok(held)
}




fn query_of(asked: &Value) -> String {
    let mut held = Vec::new();
    for entry in asked["parameter"].as_array().into_iter().flatten() {
        let Some(name) = entry["name"].as_str() else {
            continue;
        };
        if name == KIND {
            continue;
        }
        let Some(value) = crate::operation::primitive_of(entry) else {
            continue;
        };
        held.push(format!(
            "{}={}",
            crate::query::encoded(name),
            crate::query::encoded(&value)
        ));
    }
    held.join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_parameters_becomes_a_query() {
        let held = query_of(&json!({
            "resourceType": "Parameters",
            "parameter": [
                {"name": "identifier", "valueString": "urn:oid:1.2|1234"},
                {"name": "family", "valueString": "Stone"},
            ],
        }));
        assert_eq!(held, "identifier=urn%3Aoid%3A1.2%7C1234&family=Stone");
    }

    #[test]
    fn the_type_is_not_a_search_parameter() {
        let held = query_of(&json!({
            "resourceType": "Parameters",
            "parameter": [
                {"name": "resourceType", "valueString": "Practitioner"},
                {"name": "family", "valueString": "Stone"},
            ],
        }));
        assert_eq!(held, "family=Stone");
    }

    #[test]
    fn a_parameters_naming_nothing_is_an_empty_query() {
        assert!(query_of(&json!({"resourceType": "Parameters"})).is_empty());
        assert!(query_of(&Value::Null).is_empty());
    }
}
