use fhir_core::{Error, FhirInstant, FhirVersion, ResourceEnvelope, ResourceId, ResourceType, VersionId};
use sqlx::postgres::PgRow;
use sqlx::Row;

pub const COLUMNS: &str = "surrogate_id, resource_type, resource_id, version_number, \
                           spec_version, last_updated, updated_secs, updated_nanos, \
                           is_deleted, body";

fn wrong(reason: &str) -> Error {
    Error::Internal(format!("stored row is malformed: {reason}"))
}

pub fn envelope_of(row: &PgRow) -> Result<ResourceEnvelope, Error> {
    let spec: String = row.try_get("spec_version").map_err(|_| wrong("spec version"))?;
    let version: FhirVersion = spec.parse()?;
    let deleted: bool = row.try_get("is_deleted").map_err(|_| wrong("delete flag"))?;
    let body: Vec<u8> = row.try_get("body").map_err(|_| wrong("body"))?;
    if !deleted {
        return ResourceEnvelope::parse(version, &crate::body::decoded(&body)?);
    }
    let resource_type: String = row.try_get("resource_type").map_err(|_| wrong("type"))?;
    let id: String = row.try_get("resource_id").map_err(|_| wrong("id"))?;
    let number: i64 = row.try_get("version_number").map_err(|_| wrong("version"))?;
    let updated: String = row.try_get("last_updated").map_err(|_| wrong("write time"))?;
    Ok(ResourceEnvelope::deleted_marker(
        version,
        resource_type.parse::<ResourceType>()?,
        ResourceId::parse(&id)?,
        VersionId::parse(&number.to_string())?,
        FhirInstant::parse(&updated)?,
    ))
}

pub fn surrogate_of(row: &PgRow) -> Result<i64, Error> {
    row.try_get("surrogate_id").map_err(|_| wrong("surrogate key"))
}
