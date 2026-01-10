use crate::store::RelationalStore;
use fhir_core::search::ParameterSpec;
use fhir_core::Error;
use fhir_store::{IndexReport, SearchPage, SearchQuery};

fn pending() -> Error {
    Error::UnsupportedParameter("the query compiler is not built yet".to_owned())
}

pub async fn run(_store: &RelationalStore, _query: &SearchQuery) -> Result<SearchPage, Error> {
    Err(pending())
}

pub async fn drop_index(_store: &RelationalStore, _url: &str) -> Result<(), Error> {
    Err(pending())
}

pub async fn reindex(
    _store: &RelationalStore,
    _specs: &[ParameterSpec],
) -> Result<Vec<IndexReport>, Error> {
    Err(pending())
}
