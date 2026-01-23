use fhir_core::{FhirInstant, FhirVersion, ResourceEnvelope, ResourceId, VersionId};

pub const SEED: &str = "2026-09-06T04:00:00Z";

pub fn envelope(resource_type: &str, id: &str, body: &str) -> ResourceEnvelope {
    let separator = if body.trim().is_empty() { "" } else { "," };
    let bytes = format!(
        r#"{{"resourceType":"{resource_type}","id":"{id}","meta":{{"versionId":"0","lastUpdated":"{SEED}"}}{separator}{body}}}"#
    )
    .into_bytes();
    ResourceEnvelope::parse(FhirVersion::R4, &bytes).expect("fixture body is a valid envelope")
}

pub fn patient(id: &str, family: &str, active: bool) -> ResourceEnvelope {
    envelope(
        "Patient",
        id,
        &format!(r#""active":{active},"name":[{{"family":"{family}"}}],"birthDate":"1980-05-06""#),
    )
}

pub fn observation(id: &str, code: &str, value: f64, subject: &str) -> ResourceEnvelope {
    envelope(
        "Observation",
        id,
        &format!(
            r#""status":"final","code":{{"coding":[{{"system":"urn:s","code":"{code}"}}]}},"valueQuantity":{{"value":{value},"system":"urn:u","code":"mg"}},"subject":{{"reference":"{subject}"}}"#
        ),
    )
}

pub fn id(value: &str) -> ResourceId {
    ResourceId::parse(value).expect("fixture id is valid")
}

pub fn version(value: &str) -> VersionId {
    VersionId::parse(value).expect("fixture version is valid")
}

pub fn instant(value: &str) -> FhirInstant {
    FhirInstant::parse(value).expect("fixture instant is valid")
}

pub struct Refusing {
    inner: std::sync::Arc<dyn fhir_store::ResourceStore>,
    refused: String,
}

impl Refusing {
    pub fn new(inner: std::sync::Arc<dyn fhir_store::ResourceStore>, refused: &str) -> Refusing {
        Refusing {
            inner,
            refused: refused.to_owned(),
        }
    }

    fn refuses(&self, id: &ResourceId) -> Result<(), fhir_core::Error> {
        match id.as_str() == self.refused {
            true => Err(fhir_core::Error::Internal(format!(
                "the store refuses {:?}",
                self.refused
            ))),
            false => Ok(()),
        }
    }
}

#[async_trait::async_trait]
impl fhir_store::ResourceStore for Refusing {
    async fn create(&self, envelope: ResourceEnvelope) -> Result<ResourceEnvelope, fhir_core::Error> {
        self.inner.create(envelope).await
    }

    async fn read(&self, id: &ResourceId) -> Result<ResourceEnvelope, fhir_core::Error> {
        self.inner.read(id).await
    }

    async fn vread(
        &self,
        id: &ResourceId,
        version: &VersionId,
    ) -> Result<ResourceEnvelope, fhir_core::Error> {
        self.inner.vread(id, version).await
    }

    async fn update(
        &self,
        envelope: ResourceEnvelope,
        expected_version: Option<&VersionId>,
    ) -> Result<ResourceEnvelope, fhir_core::Error> {
        self.refuses(envelope.id())?;
        self.inner.update(envelope, expected_version).await
    }

    async fn search(
        &self,
        query: &fhir_store::SearchQuery,
    ) -> Result<fhir_store::SearchPage, fhir_core::Error> {
        self.inner.search(query).await
    }

    async fn delete(&self, id: &ResourceId) -> Result<ResourceEnvelope, fhir_core::Error> {
        self.refuses(id)?;
        self.inner.delete(id).await
    }

    async fn hard_delete(&self, id: &ResourceId) -> Result<(), fhir_core::Error> {
        self.refuses(id)?;
        self.inner.hard_delete(id).await
    }

    async fn purge_history(&self, id: &ResourceId) -> Result<usize, fhir_core::Error> {
        self.refuses(id)?;
        self.inner.purge_history(id).await
    }

    async fn history(
        &self,
        scope: &fhir_store::HistoryScope,
        query: &fhir_store::HistoryQuery,
    ) -> Result<fhir_store::HistoryPage, fhir_core::Error> {
        self.inner.history(scope, query).await
    }

    async fn reindex(
        &self,
        specs: &[fhir_core::search::ParameterSpec],
    ) -> Result<Vec<fhir_store::IndexReport>, fhir_core::Error> {
        self.inner.reindex(specs).await
    }

    fn health(&self) -> Result<(), fhir_core::Error> {
        self.inner.health()
    }
}
