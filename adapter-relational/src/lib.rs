
pub(crate) mod compile;
pub use fhir_store::body;
pub use fhir_store::index as extract;

pub mod bulk;
pub mod fault;
pub mod jobs;
pub mod migration;
pub(crate) mod query;
pub mod row;
pub mod store;
pub mod throttle;

pub use migration::{latest, Migration, MIGRATIONS};
pub use fhir_store::Namespace;
pub use fault::{Fault, Policy};
pub use bulk::RelationalBulkStore;
pub use jobs::RelationalJobStore;
pub use store::RelationalStore;
pub use throttle::Throttle;

pub const ENV_URL: &str = "FHIR_DATABASE_URL";

pub const ENV_NAMESPACE: &str = "FHIR_SCHEMA";

pub const DEFAULT_URL: &str = "postgres://fhir:fhir@127.0.0.1:5432/fhir";
