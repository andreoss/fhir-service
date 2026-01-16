
pub(crate) mod compile;
pub mod body;
pub mod extract;
pub mod fault;
pub mod jobs;
pub mod migration;
pub mod namespace;
pub(crate) mod query;
pub mod row;
pub mod store;
pub mod throttle;

pub use migration::{latest, Migration, MIGRATIONS};
pub use namespace::Namespace;
pub use fault::{Fault, Policy};
pub use jobs::RelationalJobStore;
pub use store::RelationalStore;
pub use throttle::Throttle;

pub const ENV_URL: &str = "FHIR_DATABASE_URL";

pub const ENV_NAMESPACE: &str = "FHIR_SCHEMA";

pub const DEFAULT_URL: &str = "postgres://fhir:fhir@127.0.0.1:5432/fhir";
