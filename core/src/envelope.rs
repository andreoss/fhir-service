use crate::{Error, FhirInstant, FhirVersion, ResourceId, ResourceType, VersionId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceEnvelope {
    version: FhirVersion,
    resource_type: ResourceType,
    id: ResourceId,
    version_id: VersionId,
    last_updated: FhirInstant,
    raw: Vec<u8>,
}

impl ResourceEnvelope {
    pub fn parse(version: FhirVersion, bytes: &[u8]) -> Result<ResourceEnvelope, Error> {
        let text = std::str::from_utf8(bytes).map_err(|e| Error::InvalidJson(e.to_string()))?;
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| Error::InvalidJson(e.to_string()))?;
        let obj = value
            .as_object()
            .ok_or_else(|| Error::InvalidEnvelope("expected a JSON object".to_owned()))?;
        let resource_type = obj
            .get("resourceType")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::InvalidEnvelope("missing or non-string resourceType".to_owned()))?
            .parse::<ResourceType>()?;
        let id = obj
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::InvalidEnvelope("missing or non-string id".to_owned()))?
            .parse::<ResourceId>()?;
        let meta = obj
            .get("meta")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| Error::InvalidEnvelope("missing or non-object meta".to_owned()))?;
        let version_id = meta
            .get("versionId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::InvalidEnvelope("missing or non-string meta.versionId".to_owned()))?
            .parse::<VersionId>()?;
        let last_updated = meta
            .get("lastUpdated")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::InvalidEnvelope("missing or non-string meta.lastUpdated".to_owned()))?
            .parse::<FhirInstant>()?;
        Ok(ResourceEnvelope {
            version,
            resource_type,
            id,
            version_id,
            last_updated,
            raw: bytes.to_vec(),
        })
    }

    pub fn from_metadata(
        version: FhirVersion,
        resource_type: ResourceType,
        id: ResourceId,
        version_id: VersionId,
        last_updated: FhirInstant,
    ) -> ResourceEnvelope {
        let envelope = ResourceEnvelope {
            version,
            resource_type,
            id,
            version_id,
            last_updated,
            raw: Vec::new(),
        };
        let raw = envelope.render_metadata();
        ResourceEnvelope { raw, ..envelope }
    }

    pub fn version(&self) -> FhirVersion {
        self.version
    }

    pub fn resource_type(&self) -> ResourceType {
        self.resource_type
    }

    pub fn id(&self) -> &ResourceId {
        &self.id
    }

    pub fn version_id(&self) -> &VersionId {
        &self.version_id
    }

    pub fn last_updated(&self) -> &FhirInstant {
        &self.last_updated
    }

    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    pub fn to_json(&self) -> Vec<u8> {
        match self.version {
            FhirVersion::Stu3 => self.render_metadata(),
            FhirVersion::R4 => self.render_metadata(),
            FhirVersion::R4b => self.render_metadata(),
            FhirVersion::R5 => self.render_metadata(),
        }
    }

    pub fn stored_with(&self, version_id: VersionId, last_updated: FhirInstant) -> Result<ResourceEnvelope, Error> {
        let text = std::str::from_utf8(&self.raw).map_err(|e| Error::InvalidJson(e.to_string()))?;
        let mut value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| Error::InvalidJson(e.to_string()))?;
        let meta = value
            .get_mut("meta")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| Error::InvalidEnvelope("missing meta".to_owned()))?;
        meta.insert("versionId".to_owned(), serde_json::Value::String(version_id.as_str().to_owned()));
        meta.insert("lastUpdated".to_owned(), serde_json::Value::String(last_updated.as_str().to_owned()));
        let bytes = serde_json::to_vec(&value).map_err(|e| Error::InvalidJson(e.to_string()))?;
        ResourceEnvelope::parse(self.version, &bytes)
    }

    pub fn content_eq(&self, other: &ResourceEnvelope) -> bool {
        match (self.content_value(), other.content_value()) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    }

    fn render_metadata(&self) -> Vec<u8> {
        let meta = serde_json::json!({
            "versionId": self.version_id.as_str(),
            "lastUpdated": self.last_updated.as_str(),
        });
        let body = serde_json::json!({
            "resourceType": self.resource_type.as_str(),
            "id": self.id.as_str(),
            "meta": meta,
        });
        serde_json::to_vec(&body).expect("envelope metadata is serializable")
    }

    fn content_value(&self) -> Option<serde_json::Value> {
        let text = std::str::from_utf8(&self.raw).ok()?;
        let mut value: serde_json::Value = serde_json::from_str(text).ok()?;
        if let Some(meta) = value.get_mut("meta").and_then(serde_json::Value::as_object_mut) {
            meta.remove("versionId");
            meta.remove("lastUpdated");
            if meta.is_empty() {
                if let Some(object) = value.as_object_mut() {
                    object.remove("meta");
                }
            }
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = br#"{
  "resourceType": "Patient",
  "id": "pt-01",
  "meta": {
    "versionId": "3",
    "lastUpdated": "2026-09-06T04:00:00.000Z"
  },
  "active": true
}
"#;

    const MINIFIED: &[u8] =
        b"{\"resourceType\":\"Observation\",\"id\":\"obs-9\",\"meta\":{\"versionId\":\"2\",\"lastUpdated\":\"2026-09-06T04:00:00Z\"},\"status\":\"final\"}";

    fn sample_metadata() -> (ResourceEnvelope, &'static str, &'static str) {
        let envelope = ResourceEnvelope::parse(FhirVersion::R4, SAMPLE).expect("sample must parse");
        (envelope, "pt-01", "2026-09-06T04:00:00.000Z")
    }

    #[test]
    fn round_trip_preserves_metadata() {
        let (envelope, id, last_updated) = sample_metadata();
        let rendered = envelope.to_json();
        let reparsed = ResourceEnvelope::parse(FhirVersion::R4, &rendered).expect("rendered must reparse");
        assert_eq!(reparsed.version(), envelope.version());
        assert_eq!(reparsed.resource_type(), envelope.resource_type());
        assert_eq!(reparsed.id(), envelope.id());
        assert_eq!(reparsed.version_id(), envelope.version_id());
        assert_eq!(reparsed.last_updated(), envelope.last_updated());
        assert_eq!(envelope.id().as_str(), id);
        assert_eq!(envelope.last_updated().as_str(), last_updated);
    }

    #[test]
    fn rendered_json_contains_metadata_fields() {
        let (envelope, id, _) = sample_metadata();
        let rendered = String::from_utf8(envelope.to_json()).unwrap();
        assert!(rendered.contains("\"resourceType\":\"Patient\""));
        assert!(rendered.contains(&format!("\"id\":\"{id}\"")));
        assert!(rendered.contains("\"versionId\":\"3\""));
        assert!(rendered.contains("\"lastUpdated\":\"2026-09-06T04:00:00.000Z\""));
    }

    #[test]
    fn raw_bytes_are_preserved_verbatim() {
        let envelope = ResourceEnvelope::parse(FhirVersion::R4, SAMPLE).unwrap();
        assert_eq!(envelope.raw(), SAMPLE);
        let envelope = ResourceEnvelope::parse(FhirVersion::Stu3, MINIFIED).unwrap();
        assert_eq!(envelope.raw(), MINIFIED);
    }

    #[test]
    fn from_metadata_builds_round_trippable_envelope() {
        let envelope = ResourceEnvelope::from_metadata(
            FhirVersion::R5,
            "Patient".parse().unwrap(),
            ResourceId::parse("p5-1").unwrap(),
            VersionId::parse("7").unwrap(),
            FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap(),
        );
        let reparsed = ResourceEnvelope::parse(FhirVersion::R5, &envelope.to_json()).unwrap();
        assert_eq!(reparsed.id().as_str(), "p5-1");
        assert_eq!(reparsed.version_id().as_str(), "7");
        assert_eq!(reparsed.resource_type().as_str(), "Patient");
        assert_eq!(reparsed.last_updated().as_str(), "2026-09-06T04:00:00.000Z");
        assert_eq!(reparsed.version(), FhirVersion::R5);
    }

    #[test]
    fn rejects_non_utf8_bytes() {
        let error = ResourceEnvelope::parse(FhirVersion::R4, &[0xff, 0xfe, 0x00, b'{']).unwrap_err();
        assert!(matches!(error, Error::InvalidJson(_)));
    }

    #[test]
    fn rejects_malformed_json() {
        let error = ResourceEnvelope::parse(FhirVersion::R4, b"{ not json").unwrap_err();
        assert!(matches!(error, Error::InvalidJson(_)));
    }

    #[test]
    fn rejects_non_object_json() {
        for bytes in [b"[1,2,3]" as &[u8], b"\"Patient\"" as &[u8], b"42" as &[u8]] {
            let error = ResourceEnvelope::parse(FhirVersion::R4, bytes).unwrap_err();
            assert!(matches!(error, Error::InvalidEnvelope(_)));
        }
    }

    #[test]
    fn rejects_unknown_resource_type() {
        let body = br#"{"resourceType":"PatientX","id":"p","meta":{"versionId":"1","lastUpdated":"2026-09-06T04:00:00Z"}}"#;
        let error = ResourceEnvelope::parse(FhirVersion::R4, body).unwrap_err();
        assert!(matches!(error, Error::InvalidResourceType(_)));
    }

    #[test]
    fn rejects_missing_resource_type() {
        let body = br#"{"id":"p","meta":{"versionId":"1","lastUpdated":"2026-09-06T04:00:00Z"}}"#;
        let error = ResourceEnvelope::parse(FhirVersion::R4, body).unwrap_err();
        assert!(matches!(error, Error::InvalidEnvelope(_)));
    }

    #[test]
    fn rejects_non_string_resource_type() {
        let body = br#"{"resourceType":5,"id":"p","meta":{"versionId":"1","lastUpdated":"2026-09-06T04:00:00Z"}}"#;
        let error = ResourceEnvelope::parse(FhirVersion::R4, body).unwrap_err();
        assert!(matches!(error, Error::InvalidEnvelope(_)));
    }

    #[test]
    fn rejects_missing_id() {
        let body = br#"{"resourceType":"Patient","meta":{"versionId":"1","lastUpdated":"2026-09-06T04:00:00Z"}}"#;
        let error = ResourceEnvelope::parse(FhirVersion::R4, body).unwrap_err();
        assert!(matches!(error, Error::InvalidEnvelope(_)));
    }

    #[test]
    fn rejects_invalid_id() {
        let body = br#"{"resourceType":"Patient","id":"bad id","meta":{"versionId":"1","lastUpdated":"2026-09-06T04:00:00Z"}}"#;
        let error = ResourceEnvelope::parse(FhirVersion::R4, body).unwrap_err();
        assert!(matches!(error, Error::InvalidResourceId(_)));
    }

    #[test]
    fn rejects_missing_meta() {
        let body = br#"{"resourceType":"Patient","id":"p"}"#;
        let error = ResourceEnvelope::parse(FhirVersion::R4, body).unwrap_err();
        assert!(matches!(error, Error::InvalidEnvelope(_)));
    }

    #[test]
    fn rejects_missing_version_id() {
        let body = br#"{"resourceType":"Patient","id":"p","meta":{"lastUpdated":"2026-09-06T04:00:00Z"}}"#;
        let error = ResourceEnvelope::parse(FhirVersion::R4, body).unwrap_err();
        assert!(matches!(error, Error::InvalidEnvelope(_)));
    }

    #[test]
    fn rejects_invalid_version_id() {
        let body = br#"{"resourceType":"Patient","id":"p","meta":{"versionId":"1?","lastUpdated":"2026-09-06T04:00:00Z"}}"#;
        let error = ResourceEnvelope::parse(FhirVersion::R4, body).unwrap_err();
        assert!(matches!(error, Error::InvalidVersion(_)));
    }

    #[test]
    fn rejects_missing_last_updated() {
        let body = br#"{"resourceType":"Patient","id":"p","meta":{"versionId":"1"}}"#;
        let error = ResourceEnvelope::parse(FhirVersion::R4, body).unwrap_err();
        assert!(matches!(error, Error::InvalidEnvelope(_)));
    }

    #[test]
    fn rejects_invalid_last_updated() {
        let body = br#"{"resourceType":"Patient","id":"p","meta":{"versionId":"1","lastUpdated":"whenever"}}"#;
        let error = ResourceEnvelope::parse(FhirVersion::R4, body).unwrap_err();
        assert!(matches!(error, Error::InvalidInstant(_)));
    }

    #[test]
    fn exhaustively_dispatch_over_versions() {
        for version in FhirVersion::ALL {
            let envelope = ResourceEnvelope::parse(version, SAMPLE).expect("sample must parse for every version");
            assert_eq!(envelope.version(), version);
            let rendered = envelope.to_json();
            let reparsed = ResourceEnvelope::parse(version, &rendered).unwrap();
            assert_eq!(reparsed.id(), envelope.id());
        }
    }

    #[test]
    fn stored_with_replaces_server_metadata_and_preserves_body() {
        let envelope = ResourceEnvelope::parse(FhirVersion::R4, SAMPLE).unwrap();
        let stored = envelope
            .stored_with(
                VersionId::parse("9").unwrap(),
                FhirInstant::parse("2026-09-06T05:00:00.000Z").unwrap(),
            )
            .unwrap();
        assert_eq!(stored.version_id().as_str(), "9");
        assert_eq!(stored.last_updated().as_str(), "2026-09-06T05:00:00.000Z");
        assert_eq!(stored.id(), envelope.id());
        assert_eq!(stored.resource_type(), envelope.resource_type());
        let text = std::str::from_utf8(stored.raw()).unwrap();
        assert!(text.contains("\"active\":true"));
        assert!(text.contains("\"resourceType\":\"Patient\""));
    }

    #[test]
    fn content_eq_ignores_server_managed_metadata() {
        let first = ResourceEnvelope::parse(FhirVersion::R4, SAMPLE).unwrap();
        let second = ResourceEnvelope::parse(FhirVersion::R4, br#"{
            "resourceType": "Patient",
            "id": "pt-01",
            "meta": { "versionId": "7", "lastUpdated": "2026-09-06T09:00:00Z" },
            "active": true
        }"#)
        .unwrap();
        assert!(first.content_eq(&second));
        assert!(second.content_eq(&first));

        let changed = ResourceEnvelope::parse(FhirVersion::R4, br#"{
            "resourceType": "Patient",
            "id": "pt-01",
            "meta": { "versionId": "7", "lastUpdated": "2026-09-06T09:00:00Z" },
            "active": false
        }"#)
        .unwrap();
        assert!(!first.content_eq(&changed));
    }

    #[test]
    fn content_eq_is_structural_over_whitespace_and_key_order() {
        let a = ResourceEnvelope::parse(FhirVersion::R4, br#"{"resourceType":"Patient","id":"p1","meta":{"versionId":"1","lastUpdated":"2026-09-06T04:00:00Z"},"active":true,"name":[{"family":"X"}]}"#).unwrap();
        let b = ResourceEnvelope::parse(FhirVersion::R4, br#"{
            "name": [ { "family": "X" } ],
            "id": "p1",
            "meta": { "lastUpdated": "2030-01-01T00:00:00Z", "versionId": "4" },
            "active": true,
            "resourceType": "Patient"
        }"#).unwrap();
        assert!(a.content_eq(&b));
    }
}