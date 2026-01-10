
pub(crate) mod compile;
pub mod body;
pub mod extract;
pub mod migration;
pub mod namespace;
pub(crate) mod query;
pub mod row;
pub mod store;

pub use migration::{latest, Migration, MIGRATIONS};
pub use namespace::Namespace;
pub use store::RelationalStore;

pub const ENV_URL: &str = "FHIR_DATABASE_URL";

pub const ENV_NAMESPACE: &str = "FHIR_SCHEMA";

pub const DEFAULT_URL: &str = "postgres://fhir:fhir@127.0.0.1:5432/fhir";
