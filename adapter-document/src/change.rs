use crate::store::{faulted, DocumentStore};
use async_trait::async_trait;
use fhir_core::{Error, FhirInstant, ResourceEnvelope, ResourceId, ResourceType, VersionId};
use fhir_store::{ChangeFeed, ChangeKind, ChangePage, ChangeRecord, Continuation, FeedRange};
use mongodb::bson::{doc, Document};
use mongodb::{ClientSession, Collection};

pub const CHANGES: &str = "change";

fn wrong(reason: &str) -> Error {
    Error::Internal(format!("change record is malformed: {reason}"))
}

fn text_of(held: &Document, name: &str) -> Result<String, Error> {
    held.get_str(name)
        .map(str::to_owned)
        .map_err(|_| wrong(name))
}

fn kind_of(raw: &str) -> Result<ChangeKind, Error> {
    match raw {
        "created" => Ok(ChangeKind::Created),
        "updated" => Ok(ChangeKind::Updated),
        "deleted" => Ok(ChangeKind::Deleted),
        other => Err(wrong(other)),
    }
}

pub fn record_of(envelope: &ResourceEnvelope, sequence: i64) -> Document {
    let kind = ChangeKind::of(envelope.version_id(), envelope.is_deleted());
    doc! {
        "_id": sequence,
        "sequence": sequence,
        "partition": fhir_store::partition(envelope.id().as_str()),
        "resource_type": envelope.resource_type().as_str(),
        "resource_id": envelope.id().as_str(),
        "version_number": envelope.version_id().as_str(),
        "kind": kind.as_str(),
        "last_updated": envelope.last_updated().as_str(),
    }
}

fn read(held: &Document) -> Result<ChangeRecord, Error> {
    Ok(ChangeRecord {
        sequence: held.get_i64("sequence").map_err(|_| wrong("sequence"))?,
        resource_type: text_of(held, "resource_type")?.parse::<ResourceType>()?,
        id: ResourceId::parse(&text_of(held, "resource_id")?)?,
        version: VersionId::parse(&text_of(held, "version_number")?)?,
        kind: kind_of(&text_of(held, "kind")?)?,
        last_updated: FhirInstant::parse(&text_of(held, "last_updated")?)?,
    })
}

impl DocumentStore {
    pub fn feed(&self) -> Collection<Document> {
        self.database().collection(CHANGES)
    }

    pub(crate) async fn record(
        &self,
        session: &mut ClientSession,
        envelope: &ResourceEnvelope,
        sequence: i64,
    ) -> Result<(), Error> {
        self.feed()
            .insert_one(record_of(envelope, sequence))
            .session(&mut *session)
            .await
            .map_err(|error| faulted("recording a write", error))?;
        Ok(())
    }
}

#[async_trait]
impl ChangeFeed for DocumentStore {
    async fn changes(
        &self,
        range: FeedRange,
        after: &Continuation,
        count: usize,
    ) -> Result<ChangePage, Error> {
        let filter = doc! {
            "sequence": {"$gt": after.sequence()},
            "partition": {"$gte": range.low(), "$lt": range.high()},
        };
        let mut pipeline = vec![doc! {"$match": filter}, doc! {"$sort": {"sequence": 1}}];
        if count < usize::MAX {
            pipeline.push(doc! {"$limit": count.min(i64::MAX as usize).max(1) as i64});
        }
        let found = self
            .drawn(CHANGES, pipeline, "reading the change feed")
            .await?;
        let records = found
            .iter()
            .map(read)
            .collect::<Result<Vec<ChangeRecord>, Error>>()?;
        let next = match records.last() {
            Some(last) => Continuation::after(last.sequence),
            None => *after,
        };
        Ok(ChangePage { records, next })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::FhirVersion;

    fn envelope(raw: &str) -> ResourceEnvelope {
        ResourceEnvelope::parse(FhirVersion::R4, raw.as_bytes()).expect("a valid envelope")
    }

    fn patient(version: &str) -> ResourceEnvelope {
        envelope(&format!(
            r#"{{"resourceType":"Patient","id":"c1","meta":{{"versionId":"{version}","lastUpdated":"2026-09-06T04:00:00.000Z"}}}}"#
        ))
    }

    #[test]
    fn a_first_version_is_recorded_as_a_creation() {
        let held = record_of(&patient("1"), 7);
        assert_eq!(held.get_str("kind"), Ok("created"));
        assert_eq!(held.get_i64("sequence"), Ok(7));
        assert_eq!(held.get_str("resource_id"), Ok("c1"));
        assert_eq!(read(&held).unwrap().kind, ChangeKind::Created);
    }

    #[test]
    fn a_later_version_is_recorded_as_an_update() {
        let held = record_of(&patient("2"), 8);
        assert_eq!(held.get_str("kind"), Ok("updated"));
        assert_eq!(read(&held).unwrap().sequence, 8);
    }

    #[test]
    fn a_delete_marker_is_recorded_as_a_deletion() {
        let marker = ResourceEnvelope::deleted_marker(
            FhirVersion::R4,
            "Patient".parse().expect("a known type"),
            ResourceId::parse("c1").expect("an id"),
            VersionId::parse("3").expect("a version"),
            FhirInstant::parse("2026-09-06T04:00:00.000Z").expect("an instant"),
        );
        let held = record_of(&marker, 9);
        assert_eq!(held.get_str("kind"), Ok("deleted"));
        assert_eq!(read(&held).unwrap().kind, ChangeKind::Deleted);
    }

    #[test]
    fn a_record_of_an_unknown_shape_is_refused() {
        assert!(kind_of("nothing").is_err());
        assert!(read(&doc! {}).is_err());
    }
}
