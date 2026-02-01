
pub mod expr;
pub mod fault;
pub mod query;
pub mod range;
pub mod record;
pub mod store;

pub use fault::{Fault, Policy};
pub use range::{FeedRange, PARTITIONS};
pub use store::{DocumentScope, DocumentStore};

pub const ENV_URL: &str = "FHIR_DOCUMENT_URL";

pub const ENV_NAMESPACE: &str = "FHIR_DOCUMENT_NAMESPACE";

pub const DEFAULT_URL: &str = "mongodb://127.0.0.1:27017/?directConnection=true";
