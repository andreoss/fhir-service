use crate::handler::{JobContext, JobHandler, Unit, UnitOutcome};
use async_trait::async_trait;
use fhir_core::search::{Compartment, Filter};
use fhir_core::{Error, FhirInstant, ResourceEnvelope, ResourceId, ResourceType};
use fhir_store::{
    system_clock, BulkStore, Clock, HistoryOrder, HistoryQuery, HistoryScope, JobKind, Output,
    ResourceStore, SearchQuery,
};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportScope {
    System,
    Patient(Option<ResourceId>),
    Group(ResourceId),
}

#[derive(Debug, Clone)]
pub struct TypeFilter {
    pub resource_type: ResourceType,
    pub filters: Vec<Filter>,
}

fn narrowing(text: &str) -> Result<TypeFilter, Error> {
    let (head, query) = text.split_once('?').unwrap_or((text, ""));
    let resource_type = head.trim().parse::<ResourceType>()?;
    let mut filters = Vec::new();
    for pair in query.split('&').filter(|part| !part.trim().is_empty()) {
        let (name, raw) = pair.split_once('=').ok_or_else(|| {
            Error::InvalidParameter(format!("_typeFilter {pair:?} carries no value"))
        })?;
        if name.contains(':') || name.contains('.') {
            return Err(Error::UnsupportedParameter(format!(
                "_typeFilter {name:?}"
            )));
        }
        let def = fhir_core::search::lookup(Some(resource_type), name).ok_or_else(|| {
            Error::UnsupportedParameter(format!("_typeFilter {name:?}"))
        })?;
        let values = raw
            .split(',')
            .filter(|part| !part.is_empty())
            .map(|part| def.value(part))
            .collect::<Result<Vec<_>, Error>>()?;
        filters.push(Filter::new(name, def.target.clone(), values));
    }
    Ok(TypeFilter {
        resource_type,
        filters,
    })
}

#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub scope: ExportScope,
    pub types: Vec<ResourceType>,
    pub filters: Vec<TypeFilter>,
    pub narrowings: Vec<String>,
    pub since: Option<FhirInstant>,
    pub till: FhirInstant,
    pub container: String,
}

fn text(payload: &Value, name: &str) -> Option<String> {
    payload
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|found| !found.trim().is_empty())
}

fn listed(payload: &Value, name: &str) -> Vec<String> {
    match payload.get(name) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        Some(Value::String(text)) => text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn resource_types(names: &[String]) -> Result<Vec<ResourceType>, Error> {
    names.iter().map(|name| name.parse::<ResourceType>()).collect()
}

fn format_of(payload: &Value) -> Result<String, Error> {
    match text(payload, "_outputFormat").or_else(|| text(payload, "outputFormat")) {
        None => Ok(fhir_store::NDJSON.to_owned()),
        Some(found) => fhir_store::output_format(&found),
    }
}

fn scope_of(payload: &Value) -> Result<ExportScope, Error> {
    let named = text(payload, "scope").unwrap_or_else(|| "system".to_owned());
    let id = match text(payload, "id") {
        Some(found) => Some(ResourceId::parse(&found)?),
        None => None,
    };
    match named.as_str() {
        "system" => Ok(ExportScope::System),
        "patient" => Ok(ExportScope::Patient(id)),
        "group" => match id {
            Some(found) => Ok(ExportScope::Group(found)),
            None => Err(Error::InvalidParameter(
                "a group export names no group".to_owned(),
            )),
        },
        other => Err(Error::UnsupportedParameter(format!("scope {other:?}"))),
    }
}

impl ExportRequest {
    pub fn parse(payload: &str, fallback: &FhirInstant) -> Result<ExportRequest, Error> {
        let payload: Value = serde_json::from_str(payload)
            .map_err(|error| Error::InvalidJson(error.to_string()))?;
        let _ = format_of(&payload)?;
        let mut names = listed(&payload, "_type");
        if names.is_empty() {
            names = listed(&payload, "types");
        }
        let till = match text(&payload, "_till").or_else(|| text(&payload, "till")) {
            Some(found) => FhirInstant::parse(&found)?,
            None => fallback.clone(),
        };
        let container = text(&payload, "_container")
            .or_else(|| text(&payload, "container"))
            .unwrap_or_default();
        let since = match text(&payload, "_since").or_else(|| text(&payload, "since")) {
            Some(found) => Some(FhirInstant::parse(&found)?),
            None => None,
        };
        let narrowings = listed(&payload, "_typeFilter");
        let filters = narrowings
            .iter()
            .map(|text| narrowing(text))
            .collect::<Result<Vec<TypeFilter>, Error>>()?;
        Ok(ExportRequest {
            scope: scope_of(&payload)?,
            types: resource_types(&names)?,
            filters,
            narrowings,
            since,
            till,
            container,
        })
    }

    fn to_value(&self, resource_type: &ResourceType) -> Value {
        let (scope, id) = match &self.scope {
            ExportScope::System => ("system", None),
            ExportScope::Patient(id) => ("patient", id.as_ref().map(|id| id.as_str().to_owned())),
            ExportScope::Group(id) => ("group", Some(id.as_str().to_owned())),
        };
        let mut carried = Map::new();
        carried.insert("scope".to_owned(), Value::String(scope.to_owned()));
        if let Some(id) = id {
            carried.insert("id".to_owned(), Value::String(id));
        }
        carried.insert(
            "_type".to_owned(),
            Value::Array(vec![Value::String(resource_type.as_str().to_owned())]),
        );
        carried.insert(
            "_till".to_owned(),
            Value::String(self.till.as_str().to_owned()),
        );
        carried.insert(
            "_container".to_owned(),
            Value::String(self.container.clone()),
        );
        if let Some(since) = &self.since {
            carried.insert(
                "_since".to_owned(),
                Value::String(since.as_str().to_owned()),
            );
        }
        carried.insert(
            "_typeFilter".to_owned(),
            Value::Array(
                self.narrowings
                    .iter()
                    .map(|text| Value::String(text.clone()))
                    .collect(),
            ),
        );
        Value::Object(carried)
    }

    fn file(&self, resource_type: &ResourceType) -> String {
        match self.container.is_empty() {
            true => format!("{}.ndjson", resource_type.as_str()),
            false => format!("{}/{}.ndjson", self.container, resource_type.as_str()),
        }
    }
}

fn body_of(envelope: &ResourceEnvelope) -> Result<Value, Error> {
    serde_json::from_slice(envelope.raw()).map_err(|error| Error::InvalidJson(error.to_string()))
}

async fn snapshot(
    store: &dyn ResourceStore,
    resource_type: ResourceType,
    till: &FhirInstant,
) -> Result<Vec<ResourceEnvelope>, Error> {
    let query = HistoryQuery {
        order: HistoryOrder::Oldest,
        ..HistoryQuery::default()
    };
    let page = store
        .history(&HistoryScope::Type(resource_type), &query)
        .await?;
    let mut current: BTreeMap<String, ResourceEnvelope> = BTreeMap::new();
    let point = till.key();
    for entry in page.entries {
        if entry.last_updated().key() > point {
            continue;
        }
        current.insert(entry.id().as_str().to_owned(), entry);
    }
    Ok(current
        .into_values()
        .filter(|entry| !entry.is_deleted())
        .collect())
}

async fn every_type(store: &dyn ResourceStore) -> Result<Vec<ResourceType>, Error> {
    let page = store.search(&SearchQuery::default()).await?;
    let mut found: Vec<ResourceType> = Vec::new();
    for entry in page.entries {
        let resource_type = entry.resource_type();
        if !found.contains(&resource_type) {
            found.push(resource_type);
        }
    }
    found.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    Ok(found)
}

fn gathered_types() -> Vec<&'static str> {
    fhir_core::search::compartment::definition("Patient")
        .map(|def| def.types())
        .unwrap_or_default()
}

async fn members(
    store: &dyn ResourceStore,
    id: &ResourceId,
    till: &FhirInstant,
) -> Result<Vec<ResourceId>, Error> {
    let group_type = "Group".parse::<ResourceType>()?;
    let group = snapshot(store, group_type, till)
        .await?
        .into_iter()
        .find(|entry| entry.id() == id)
        .ok_or(Error::NotFound)?;
    let body = body_of(&group)?;
    let mut found = Vec::new();
    for element in fhir_core::search::select(&body, "member.entity") {
        for pointer in fhir_core::search::pointers(element) {
            let bare = pointer.rsplit('/').next().unwrap_or_default();
            if let Ok(id) = ResourceId::parse(bare) {
                if !found.contains(&id) {
                    found.push(id);
                }
            }
        }
    }
    Ok(found)
}

fn narrowed(
    request: &ExportRequest,
    resource_type: ResourceType,
    entry: &ResourceEnvelope,
    body: &Value,
) -> bool {
    let mut wanted = request
        .filters
        .iter()
        .filter(|found| found.resource_type == resource_type)
        .peekable();
    if wanted.peek().is_none() {
        return true;
    }
    wanted.any(|found| {
        found
            .filters
            .iter()
            .all(|filter| filter.matches(entry.id(), entry.last_updated(), body))
    })
}

fn gathered(roots: &[ResourceId], resource_type: ResourceType, body: &Value) -> bool {
    let kind = match "Patient".parse::<ResourceType>() {
        Ok(kind) => kind,
        Err(_) => return false,
    };
    roots.iter().any(|id| {
        fhir_core::search::compartment::contains(
            &Compartment {
                kind,
                id: id.clone(),
            },
            resource_type,
            body,
        )
    })
}

pub struct ExportJob {
    store: Arc<dyn ResourceStore>,
    sink: Arc<dyn BulkStore>,
    clock: Clock,
}

impl ExportJob {
    pub fn new(store: Arc<dyn ResourceStore>, sink: Arc<dyn BulkStore>) -> ExportJob {
        ExportJob {
            store,
            sink,
            clock: system_clock(),
        }
    }

    pub fn with_clock(self, clock: Clock) -> ExportJob {
        ExportJob { clock, ..self }
    }

    async fn planned(&self, request: &ExportRequest) -> Result<Vec<ResourceType>, Error> {
        let mut types = match request.types.is_empty() {
            false => request.types.clone(),
            true => every_type(self.store.as_ref()).await?,
        };
        if !matches!(request.scope, ExportScope::System) {
            let gathered = gathered_types();
            types.retain(|found| gathered.contains(&found.as_str()));
        }
        Ok(types)
    }

    async fn roots(&self, request: &ExportRequest) -> Result<Option<Vec<ResourceId>>, Error> {
        match &request.scope {
            ExportScope::System => Ok(None),
            ExportScope::Patient(None) => Ok(None),
            ExportScope::Patient(Some(id)) => Ok(Some(vec![id.clone()])),
            ExportScope::Group(id) => Ok(Some(
                members(self.store.as_ref(), id, &request.till).await?,
            )),
        }
    }
}

#[async_trait]
impl JobHandler for ExportJob {
    fn kind(&self) -> JobKind {
        JobKind::Export
    }

    async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error> {
        let request = ExportRequest::parse(&job.payload, &(self.clock)())?;
        Ok(self
            .planned(&request)
            .await?
            .into_iter()
            .map(|resource_type| {
                Unit::new(
                    resource_type.as_str().to_owned(),
                    request.to_value(&resource_type).to_string(),
                )
            })
            .collect())
    }

    async fn process(&self, job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {
        let request = ExportRequest::parse(&unit.detail, &(self.clock)())?;
        let resource_type = unit.label.parse::<ResourceType>()?;
        let roots = self.roots(&request).await?;
        let mut body = Vec::new();
        let mut outcome = UnitOutcome::default();
        for entry in snapshot(self.store.as_ref(), resource_type, &request.till).await? {
            let parsed = match body_of(&entry) {
                Ok(parsed) => parsed,
                Err(error) => {
                    outcome
                        .failures
                        .push(format!("{}/{}: {error}", resource_type.as_str(), entry.id().as_str()));
                    continue;
                }
            };
            if roots
                .as_ref()
                .is_some_and(|roots| !gathered(roots, resource_type, &parsed))
            {
                continue;
            }
            if request
                .since
                .as_ref()
                .is_some_and(|since| entry.last_updated().key() < since.key())
            {
                continue;
            }
            if !narrowed(&request, resource_type, &entry, &parsed) {
                continue;
            }
            body.extend_from_slice(&serde_json::to_vec(&parsed).unwrap_or_default());
            body.push(b'\n');
            outcome.handled += 1;
        }
        if outcome.handled > 0 {
            let output = Output::new(
                request.file(&resource_type),
                resource_type.as_str(),
                outcome.handled,
            );
            self.sink.write(&job.id, &output, &body).await?;
        }
        outcome.detail.insert(
            "transactionTime".to_owned(),
            Value::String(request.till.as_str().to_owned()),
        );
        Ok(outcome)
    }
}
