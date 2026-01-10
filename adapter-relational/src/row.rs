use crate::body::{Encoding, LazyBody};
use fhir_core::{Error, FhirInstant, FhirVersion, ResourceEnvelope, ResourceId, ResourceType, VersionId};
use sqlx::postgres::PgRow;
use sqlx::Row;

const NAMES: [&str; 11] = [
    "surrogate_id",
    "resource_type",
    "resource_id",
    "version_number",
    "spec_version",
    "last_updated",
    "updated_secs",
    "updated_nanos",
    "is_deleted",
    "body",
    "body_encoding",
];

pub const COLUMNS: &str = "surrogate_id, resource_type, resource_id, version_number, \
                           spec_version, last_updated, updated_secs, updated_nanos, \
                           is_deleted, body, body_encoding";

pub fn columns(alias: &str) -> String {
    NAMES
        .iter()
        .map(|name| format!("{alias}.{name}"))
        .collect::<Vec<String>>()
        .join(", ")
}

fn wrong(reason: &str) -> Error {
    Error::Internal(format!("stored row is malformed: {reason}"))
}

pub struct Record {
    pub surrogate: i64,
    pub resource_type: ResourceType,
    pub id: ResourceId,
    pub version: VersionId,
    pub spec: FhirVersion,
    pub last_updated: FhirInstant,
    pub deleted: bool,
    pub body: LazyBody,
}

impl Record {
    pub fn of(row: &PgRow) -> Result<Record, Error> {
        let spec: String = row.try_get("spec_version").map_err(|_| wrong("spec version"))?;
        let resource_type: String = row.try_get("resource_type").map_err(|_| wrong("type"))?;
        let id: String = row.try_get("resource_id").map_err(|_| wrong("id"))?;
        let number: i64 = row.try_get("version_number").map_err(|_| wrong("version"))?;
        let updated: String = row.try_get("last_updated").map_err(|_| wrong("write time"))?;
        let encoding: String = row.try_get("body_encoding").map_err(|_| wrong("body encoding"))?;
        let stored: Vec<u8> = row.try_get("body").map_err(|_| wrong("body"))?;
        Ok(Record {
            surrogate: row.try_get("surrogate_id").map_err(|_| wrong("surrogate key"))?,
            resource_type: resource_type.parse::<ResourceType>()?,
            id: ResourceId::parse(&id)?,
            version: VersionId::parse(&number.to_string())?,
            spec: spec.parse::<FhirVersion>()?,
            last_updated: FhirInstant::parse(&updated)?,
            deleted: row.try_get("is_deleted").map_err(|_| wrong("delete flag"))?,
            body: LazyBody::new(stored, Encoding::parse(&encoding)?),
        })
    }

    pub fn envelope(&self) -> Result<ResourceEnvelope, Error> {
        match self.deleted {
            true => Ok(ResourceEnvelope::deleted_marker(
                self.spec,
                self.resource_type,
                self.id.clone(),
                self.version.clone(),
                self.last_updated.clone(),
            )),
            false => ResourceEnvelope::parse(self.spec, self.body.bytes()?),
        }
    }
}

pub fn envelope_of(row: &PgRow) -> Result<ResourceEnvelope, Error> {
    Record::of(row)?.envelope()
}
