use async_trait::async_trait;
use axum::http::StatusCode;
use fhir_core::search::Registry;
use fhir_core::{Error, FhirVersion};
use fhir_store::{InteractionEntry, Interactions, ResourceStore, Terminology};
use serde_json::Value;
use std::sync::Arc;

use crate::search::{search_bundle, SearchRequest};

pub const SEARCH: &str = "search";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    pub interaction: String,
    pub path: String,
    pub kind: Option<String>,
    pub query: Option<String>,
    pub host: String,
}

impl Asked {
    pub fn new(
        interaction: &str,
        path: impl Into<String>,
        kind: Option<&str>,
        query: Option<&str>,
        host: impl Into<String>,
    ) -> Asked {
        Asked {
            interaction: interaction.to_owned(),
            path: path.into(),
            kind: kind.map(str::to_owned),
            query: query.map(str::to_owned),
            host: host.into(),
        }
    }

    pub fn to_request(&self) -> String {
        let mut carried = serde_json::Map::new();
        carried.insert(
            "interaction".to_owned(),
            Value::String(self.interaction.clone()),
        );
        carried.insert("path".to_owned(), Value::String(self.path.clone()));
        if let Some(kind) = &self.kind {
            carried.insert("type".to_owned(), Value::String(kind.clone()));
        }
        if let Some(query) = &self.query {
            carried.insert("query".to_owned(), Value::String(query.clone()));
        }
        carried.insert("host".to_owned(), Value::String(self.host.clone()));
        Value::Object(carried).to_string()
    }

    fn parse(text: &str) -> Result<Asked, Error> {
        let parsed = serde_json::from_str::<Value>(text)
            .map_err(|error| Error::InvalidJson(error.to_string()))?;
        let carried = parsed
            .as_object()
            .ok_or_else(|| Error::InvalidJson("a deferred request is an object".to_owned()))?;
        let named = |key: &str| -> Option<String> {
            carried
                .get(key)
                .and_then(Value::as_str)
                .filter(|found| !found.is_empty())
                .map(str::to_owned)
        };
        Ok(Asked {
            interaction: named("interaction")
                .ok_or_else(|| Error::InvalidParameter("no interaction was named".to_owned()))?,
            path: named("path").unwrap_or_else(|| "/".to_owned()),
            kind: named("type"),
            query: named("query"),
            host: named("host").unwrap_or_else(|| "localhost".to_owned()),
        })
    }
}

pub struct Searches {
    store: Arc<dyn ResourceStore>,
    version: FhirVersion,
    registry: Arc<Registry>,
    terminology: Arc<dyn Terminology>,
}

impl Searches {
    pub fn new(
        store: Arc<dyn ResourceStore>,
        version: FhirVersion,
        registry: Arc<Registry>,
        terminology: Arc<dyn Terminology>,
    ) -> Searches {
        Searches {
            store,
            version,
            registry,
            terminology,
        }
    }

    async fn searched(&self, asked: &Asked) -> Result<InteractionEntry, Error> {
        let base_type = match &asked.kind {
            Some(name) => Some(crate::handlers::served(self.version, name)?),
            None => None,
        };
        let mut request = SearchRequest::parse(&self.registry, base_type, asked.query.as_deref())?;
        crate::terminology::resolve(self.terminology.as_ref(), &mut request.query).await?;
        crate::membership::resolve(&self.store, &mut request.query).await?;
        let page = self.store.search(&request.query).await?;
        let base = format!("http://{}", asked.host);
        let self_url = match asked.query.as_deref() {
            Some(raw) if !raw.is_empty() => format!("{}{}?{raw}", base, asked.path),
            _ => format!("{}{}", base, asked.path),
        };
        let body = search_bundle(
            &base,
            &self_url,
            &page,
            request.summary,
            &request.elements,
            &request.dropped,
        );
        let resource = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
        Ok(InteractionEntry::answered("200 OK").carrying(resource))
    }
}

#[async_trait]
impl Interactions for Searches {
    async fn perform(&self, request: &str) -> InteractionEntry {
        let asked = match Asked::parse(request) {
            Ok(asked) => asked,
            Err(error) => return refused(error),
        };
        match asked.interaction.as_str() {
            SEARCH => match self.searched(&asked).await {
                Ok(entry) => entry,
                Err(error) => refused(error),
            },
            other => refused(Error::UnsupportedParameter(format!("{other:?}"))),
        }
    }
}

fn refused(error: Error) -> InteractionEntry {
    let outcome = error.to_operation_outcome();
    let code = outcome.http_status();
    let reason = StatusCode::from_u16(code)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Error");
    let carried = serde_json::from_slice::<Value>(&outcome.to_fhir_json()).unwrap_or(Value::Null);
    InteractionEntry::answered(format!("{code} {reason}")).reporting(carried)
}
