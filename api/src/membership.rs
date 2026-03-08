use fhir_core::search::{
    active, collection_types, is_collection, Criterion, Filter, Modifier, SearchValue, Target,
    ValueType,
};
use fhir_core::{Error, FhirInstant, ResourceType};
use fhir_store::{ResourceStore, SearchQuery, TotalMode};
use std::collections::BTreeSet;
use std::sync::Arc;

pub async fn resolve(store: &Arc<dyn ResourceStore>, query: &mut SearchQuery) -> Result<(), Error> {
    let now = (fhir_store::system_clock())();
    for filter in query.filters.iter_mut() {
        if matches!(filter.target, Target::Collection) {
            *filter = lowered(store.as_ref(), filter, &now).await?;
        }
    }
    for chain in query.chains.iter_mut() {
        walk(store.as_ref(), &mut chain.next, &now).await?;
    }
    Ok(())
}

fn walk<'a>(
    store: &'a dyn ResourceStore,
    criterion: &'a mut Criterion,
    now: &'a FhirInstant,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Error>> + Send + 'a>> {
    Box::pin(async move {
        match criterion {
            Criterion::Direct(filter) => {
                if matches!(filter.target, Target::Collection) {
                    *filter = lowered(store, filter, now).await?;
                }
                Ok(())
            }
            Criterion::Linked(chain) => walk(store, &mut chain.next, now).await,
        }
    })
}

async fn lowered(
    store: &dyn ResourceStore,
    filter: &Filter,
    now: &FhirInstant,
) -> Result<Filter, Error> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    for value in &filter.values {
        let SearchValue::Reference(reference) = value else {
            continue;
        };
        let (kind, id) = match reference.split_once('/') {
            Some((kind, id)) => (Some(kind), id),
            None => (None, reference.as_str()),
        };
        let types = match kind.map(|name| name.parse::<ResourceType>()) {
            Some(Ok(kind)) if is_collection(&kind) => vec![kind],
            Some(_) => continue,
            None => collection_types(),
        };
        for member in members(store, &types, id, now).await? {
            found.insert(member);
        }
    }
    let values = found
        .iter()
        .map(|id| SearchValue::parse(ValueType::Token, id))
        .collect::<Result<Vec<SearchValue>, Error>>()?;
    Ok(Filter {
        name: filter.name.clone(),
        target: Target::Id,
        modifier: match filter.modifier {
            Modifier::Not => Modifier::Not,
            _ => Modifier::None,
        },
        values,
        index: filter.index.clone(),
        exempt: filter.exempt.clone(),
    })
}

async fn members(
    store: &dyn ResourceStore,
    types: &[ResourceType],
    id: &str,
    now: &FhirInstant,
) -> Result<Vec<String>, Error> {
    let Some(first) = types.first() else {
        return Ok(Vec::new());
    };
    let wanted = SearchValue::parse(ValueType::Token, id)?;
    let mut probe = SearchQuery::of_type(*first);
    probe.types = types.to_vec();
    probe.filters = vec![Filter::new("_id", Target::Id, vec![wanted])];
    probe.count = types.len().max(1);
    probe.total = TotalMode::None;
    let page = store.search(&probe).await?;
    let mut found: Vec<String> = Vec::new();
    for entry in &page.entries {
        let kind = entry.resource_type();
        if !types.contains(&kind) {
            continue;
        }
        let Ok(body) = serde_json::from_slice::<serde_json::Value>(entry.raw()) else {
            continue;
        };
        for member in active(&kind, &body, now) {
            if !found.iter().any(|kept| kept == &member) {
                found.push(member);
            }
        }
    }
    Ok(found)
}
