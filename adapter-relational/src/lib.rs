
pub mod migration;
pub mod namespace;

pub use migration::{latest, Migration, MIGRATIONS};
pub use namespace::Namespace;

pub const ENV_URL: &str = "FHIR_DATABASE_URL";

pub const ENV_NAMESPACE: &str = "FHIR_SCHEMA";

pub const DEFAULT_URL: &str = "postgres://fhir:fhir@127.0.0.1:5432/fhir";
