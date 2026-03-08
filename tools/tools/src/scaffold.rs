use fhir_core::Error;
use fhir_store::JobKind;
use std::path::Path;
use std::str::FromStr;

fn snake(name: &str) -> String {
    let mut out = String::new();
    for (position, letter) in name.chars().enumerate() {
        if letter.is_ascii_uppercase() && position > 0 {
            out.push('_');
        }
        out.push(letter.to_ascii_lowercase());
    }
    out
}

fn camel(label: &str) -> String {
    label
        .split('-')
        .map(|part| {
            let mut letters = part.chars();
            match letters.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), letters.as_str()),
                None => String::new(),
            }
        })
        .collect()
}

fn named(name: &str) -> Result<(), Error> {
    let usable = name
        .chars()
        .next()
        .map(|first| first.is_ascii_uppercase())
        .unwrap_or_default()
        && name.chars().all(|letter| letter.is_ascii_alphanumeric());
    match usable {
        true => Ok(()),
        false => Err(Error::InvalidParameter(format!(
            "job name {name:?}; expected letters and digits starting with a capital"
        ))),
    }
}

pub struct Scaffold {
    pub module: String,
    pub handler_name: String,
    pub handler: String,
    pub test: String,
    pub registration: String,
}

impl Scaffold {
    pub fn plan(name: &str, kind: &str) -> Result<Scaffold, Error> {
        named(name)?;
        let kind = JobKind::from_str(kind)?;
        let variant = camel(kind.as_str());
        let module = snake(name);
        let handler_name = format!("{name}Job");
        Ok(Scaffold {
            handler: handler_source(&handler_name, &module, &variant),
            test: test_source(&handler_name, &variant),
            registration: registration(&handler_name, &module),
            module,
            handler_name,
        })
    }

    pub fn write(&self, root: &Path) -> Result<Vec<String>, Error> {
        std::fs::create_dir_all(root)
            .map_err(|error| Error::Internal(format!("the directory is not writable: {error}")))?;
        let files = [
            (format!("{}.rs", self.module), &self.handler),
            (format!("{}_test.rs", self.module), &self.test),
        ];
        for (name, _) in &files {
            if root.join(name).exists() {
                return Err(Error::Duplicate(format!("{name} is already there")));
            }
        }
        let mut written = Vec::new();
        for (name, body) in &files {
            std::fs::write(root.join(name), body)
                .map_err(|error| Error::Internal(format!("{name} is not written: {error}")))?;
            written.push(name.clone());
        }
        Ok(written)
    }
}

fn handler_source(handler: &str, module: &str, variant: &str) -> String {
    format!(
        "use crate::handler::{{JobContext, JobHandler, Unit, UnitOutcome}};\n\
         use async_trait::async_trait;\n\
         use fhir_core::Error;\n\
         use fhir_store::{{JobKind, ResourceStore}};\n\
         use std::sync::Arc;\n\
         \n\
         pub struct {handler} {{\n\
         \x20   store: Arc<dyn ResourceStore>,\n\
         }}\n\
         \n\
         impl {handler} {{\n\
         \x20   pub fn new(store: Arc<dyn ResourceStore>) -> {handler} {{\n\
         \x20       {handler} {{ store }}\n\
         \x20   }}\n\
         \n\
         \x20   fn store(&self) -> &dyn ResourceStore {{\n\
         \x20       self.store.as_ref()\n\
         \x20   }}\n\
         }}\n\
         \n\
         #[async_trait]\n\
         impl JobHandler for {handler} {{\n\
         \x20   fn kind(&self) -> JobKind {{\n\
         \x20       JobKind::{variant}\n\
         \x20   }}\n\
         \n\
         \x20   async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error> {{\n\
         \x20       Ok(vec![Unit::new(\"{module}\", job.payload.clone())])\n\
         \x20   }}\n\
         \n\
         \x20   async fn process(&self, _job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {{\n\
         \x20       let _ = (self.store(), unit);\n\
         \x20       Ok(UnitOutcome::handled(0))\n\
         \x20   }}\n\
         }}\n"
    )
}

fn test_source(handler: &str, variant: &str) -> String {
    format!(
        "use fhir_adapter_memory::MemoryStore;\n\
         use fhir_jobs::handler::{{JobContext, JobHandler}};\n\
         use fhir_jobs::{handler};\n\
         use fhir_store::{{JobId, JobKind}};\n\
         use std::sync::Arc;\n\
         \n\
         fn context() -> JobContext {{\n\
         \x20   JobContext::new(JobId::parse(\"one\").expect(\"a valid id\"), \"{{}}\", 0)\n\
         }}\n\
         \n\
         #[tokio::test]\n\
         async fn the_job_plans_one_unit_for_its_payload() {{\n\
         \x20   let job = {handler}::new(Arc::new(MemoryStore::default()));\n\
         \x20   assert_eq!(job.kind(), JobKind::{variant});\n\
         \x20   let units = job.plan(&context()).await.expect(\"the plan is made\");\n\
         \x20   assert_eq!(units.len(), 1);\n\
         }}\n\
         \n\
         #[tokio::test]\n\
         async fn the_job_reports_what_one_unit_achieved() {{\n\
         \x20   let job = {handler}::new(Arc::new(MemoryStore::default()));\n\
         \x20   let units = job.plan(&context()).await.expect(\"the plan is made\");\n\
         \x20   let outcome = job\n\
         \x20       .process(&context(), &units[0])\n\
         \x20       .await\n\
         \x20       .expect(\"the unit runs\");\n\
         \x20   assert!(outcome.failures.is_empty());\n\
         }}\n"
    )
}

fn registration(handler: &str, module: &str) -> String {
    format!(
        "crates/jobs/src/lib.rs: pub mod {module};\n\
         crates/jobs/src/lib.rs: pub use {module}::{handler};\n\
         crates/jobs/tests/{module}.rs: the generated {module}_test.rs\n\
         composition: registry.with(Arc::new({handler}::new(store)))"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_becomes_a_module_and_a_type() {
        let plan = Scaffold::plan("BulkArchive", "bulk-delete").expect("a plan is made");
        assert_eq!(plan.module, "bulk_archive");
        assert_eq!(plan.handler_name, "BulkArchiveJob");
        assert!(plan.handler.contains("JobKind::BulkDelete"));
        assert!(plan.registration.contains("pub mod bulk_archive;"));
    }

    #[test]
    fn an_unusable_name_or_kind_is_refused() {
        assert!(Scaffold::plan("lower", "import").is_err());
        assert!(Scaffold::plan("Has Space", "import").is_err());
        assert!(Scaffold::plan("", "import").is_err());
        assert!(Scaffold::plan("Good", "nonsense").is_err());
    }
}
