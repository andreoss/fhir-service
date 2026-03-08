























use axum::http::HeaderMap;
use fhir_core::search::value::{Token, TokenSystem};
use fhir_core::search::{Filter, Grant, Modifier, SearchValue};
use fhir_core::security::Access;
use fhir_core::Error;
use serde_json::Value;


pub const HEADER: &str = "x-tenant";


pub const REVEAL: &str = "x-tenant-label";


#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tenancy {
    labelling: Option<Labelling>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Labelling {
    
    system: String,
    
    claim: String,
}

impl Tenancy {
    
    pub fn off() -> Tenancy {
        Tenancy::default()
    }

    
    
    pub fn by_label(system: &str, claim: &str) -> Result<Tenancy, Error> {
        if system.trim().is_empty() {
            return Err(Error::Config(
                "a tenant label written in no system separates nobody".to_owned(),
            ));
        }
        if claim.trim().is_empty() {
            return Err(Error::Config(
                "a tenant taken from no claim is a tenant the client chooses".to_owned(),
            ));
        }
        Ok(Tenancy {
            labelling: Some(Labelling {
                system: system.trim().to_owned(),
                claim: claim.trim().to_owned(),
            }),
        })
    }

    pub fn is_on(&self) -> bool {
        self.labelling.is_some()
    }

    pub fn system(&self) -> Option<&str> {
        self.labelling.as_ref().map(|held| held.system.as_str())
    }

    pub fn claim(&self) -> Option<&str> {
        self.labelling.as_ref().map(|held| held.claim.as_str())
    }

    
    
    
    pub fn of(&self, access: &Access, headers: &HeaderMap) -> Result<Option<String>, Error> {
        let Some(labelling) = &self.labelling else {
            return Ok(None);
        };
        let named = match access.secured {
            true => claimed(access, &labelling.claim),
            false => headers
                .get(HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|held| !held.is_empty())
                .map(str::to_owned),
        };
        match named {
            Some(tenant) => Ok(Some(tenant)),
            None => Err(Error::Forbidden(format!(
                "this instance serves several tenants and this request names none; \
                 it is taken from the {:?} claim of the token, or from the {HEADER} \
                 header where the instance is not secured",
                labelling.claim
            ))),
        }
    }

    
    
    
    pub fn narrowing(&self, tenant: &str) -> Option<Filter> {
        let system = self.system()?;
        let held = fhir_core::search::registry::lookup(None, "_security")?;
        Some(Filter {
            name: held.name.clone(),
            target: held.target.clone(),
            modifier: Modifier::None,
            values: vec![SearchValue::Token(Token {
                system: TokenSystem::Exact(system.to_owned()),
                code: Some(tenant.to_owned()),
            })],
            index: Some(held.name.clone()),
            exempt: Vec::new(),
        })
    }

    
    
    pub fn confining(
        &self,
        grant: Option<Grant>,
        access: &Access,
        headers: &HeaderMap,
    ) -> Result<Option<Grant>, Error> {
        let Some(tenant) = self.of(access, headers)? else {
            return Ok(grant);
        };
        let Some(filter) = self.narrowing(&tenant) else {
            return Ok(grant);
        };
        let mut grant = grant.unwrap_or_default();
        grant.every.push(filter);
        Ok(Some(grant))
    }

    
    
    
    pub fn labelled(&self, body: &mut Value, tenant: &str) {
        let Some(system) = self.system() else {
            return;
        };
        let Some(held) = body.as_object_mut() else {
            return;
        };
        let meta = held
            .entry("meta")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        let Some(meta) = meta.as_object_mut() else {
            return;
        };
        let mut security: Vec<Value> = meta
            .get("security")
            .and_then(Value::as_array)
            .map(|held| {
                held.iter()
                    .filter(|coding| coding.get("system").and_then(Value::as_str) != Some(system))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        security.push(serde_json::json!({"system": system, "code": tenant}));
        meta.insert("security".to_owned(), Value::Array(security));
    }

    
    
    
    
    pub fn unlabelled(&self, body: &mut Value, headers: &HeaderMap) {
        let Some(system) = self.system() else {
            return;
        };
        if asked_to_see(headers) {
            return;
        }
        strip(body, system);
    }

    
    pub fn refuses(&self, what: &str) -> Result<(), Error> {
        match self.is_on() {
            false => Ok(()),
            true => Err(Error::NotServed(format!(
                "{what} is not served while this instance serves several tenants: \
                 an id one tenant deleted may belong to another, and answering \
                 would say so"
            ))),
        }
    }
}

fn asked_to_see(headers: &HeaderMap) -> bool {
    headers
        .get(REVEAL)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .is_some_and(|held| held.eq_ignore_ascii_case("show"))
}



fn strip(body: &mut Value, system: &str) {
    match body {
        Value::Array(held) => {
            for entry in held {
                strip(entry, system);
            }
        }
        Value::Object(held) => {
            if let Some(meta) = held.get_mut("meta").and_then(Value::as_object_mut) {
                if let Some(security) = meta.get_mut("security").and_then(Value::as_array_mut) {
                    security.retain(|coding| {
                        coding.get("system").and_then(Value::as_str) != Some(system)
                    });
                    if security.is_empty() {
                        meta.remove("security");
                    }
                }
                if meta.is_empty() {
                    held.remove("meta");
                }
            }
            for (name, value) in held.iter_mut() {
                if name != "meta" {
                    strip(value, system);
                }
            }
        }
        _ => {}
    }
}

fn claimed(access: &Access, claim: &str) -> Option<String> {
    access
        .claims
        .get(claim)
        .map(|held| held.trim().to_owned())
        .filter(|held| !held.is_empty())
}





pub async fn unlabelling(
    axum::extract::State(state): axum::extract::State<crate::app::AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if !state.tenancy.is_on() {
        return next.run(request).await;
    }
    let headers = request.headers().clone();
    let response = next.run(request).await;
    if asked_to_see(&headers) {
        return response;
    }
    let (parts, body) = response.into_parts();
    let json = parts
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("json"));
    if !json {
        return axum::response::Response::from_parts(parts, body);
    }
    let Ok(bytes) = axum::body::to_bytes(body, usize::MAX).await else {
        return axum::response::Response::from_parts(parts, axum::body::Body::empty());
    };
    let Ok(mut held) = serde_json::from_slice::<Value>(&bytes) else {
        return axum::response::Response::from_parts(parts, axum::body::Body::from(bytes));
    };
    let Some(system) = state.tenancy.system() else {
        return axum::response::Response::from_parts(parts, axum::body::Body::from(bytes));
    };
    strip(&mut held, system);
    let written = serde_json::to_vec(&held).unwrap_or_else(|_| bytes.to_vec());
    let mut parts = parts;
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    axum::response::Response::from_parts(parts, axum::body::Body::from(written))
}
