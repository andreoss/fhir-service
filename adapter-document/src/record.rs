use fhir_core::{Error, FhirInstant, FhirVersion, ResourceEnvelope, ResourceId, ResourceType, VersionId};
use fhir_store::body::{encoded, Encoding, LazyBody};
use fhir_store::index::Rows;
use mongodb::bson::spec::BinarySubtype;
use mongodb::bson::{doc, Binary, Bson, Document};

const KEY_OFFSET: i64 = 1_000_000_000_000;

pub fn key_text(seconds: i64, nanos: u32) -> String {
    format!("{:013}.{:09}", seconds.saturating_add(KEY_OFFSET), nanos)
}

pub fn low_key(period: &fhir_core::InstantPeriod) -> String {
    key_text(period.low().seconds(), period.low().nanos())
}

pub fn high_key(period: &fhir_core::InstantPeriod) -> String {
    key_text(period.high().seconds(), period.high().nanos())
}

fn wrong(reason: &str) -> Error {
    Error::Internal(format!("stored document is malformed: {reason}"))
}

fn text_of(held: &Document, name: &str) -> Result<String, Error> {
    held.get_str(name).map(str::to_owned).map_err(|_| wrong(name))
}

pub fn entries_of(rows: &Rows) -> Document {
    let tokens: Vec<Bson> = rows
        .tokens
        .iter()
        .map(|row| {
            let mut entry = doc! {
                "param": &row.param,
                "slot": &row.slot,
                "ordinal": row.ordinal,
                "code": &row.code,
            };
            if let Some(system) = &row.system {
                entry.insert("system", system);
            }
            if let Some(tail) = &row.code_tail {
                entry.insert("tail", tail);
            }
            Bson::Document(entry)
        })
        .collect();
    let texts: Vec<Bson> = rows
        .texts
        .iter()
        .map(|row| {
            Bson::Document(doc! {
                "param": &row.param,
                "slot": &row.slot,
                "ordinal": row.ordinal,
                "value": &row.value,
                "folded": &row.folded,
            })
        })
        .collect();
    let numbers: Vec<Bson> = rows
        .numbers
        .iter()
        .map(|row| {
            Bson::Document(doc! {
                "param": &row.param,
                "slot": &row.slot,
                "ordinal": row.ordinal,
                "value": row.value,
            })
        })
        .collect();
    let dates: Vec<Bson> = rows
        .dates
        .iter()
        .map(|row| {
            Bson::Document(doc! {
                "param": &row.param,
                "slot": &row.slot,
                "ordinal": row.ordinal,
                "low": key_text(row.low_secs, row.low_nanos.max(0) as u32),
                "high": key_text(row.high_secs, row.high_nanos.max(0) as u32),
            })
        })
        .collect();
    let quantities: Vec<Bson> = rows
        .quantities
        .iter()
        .map(|row| {
            let mut entry = doc! {
                "param": &row.param,
                "slot": &row.slot,
                "ordinal": row.ordinal,
                "value": row.value,
                "structured": row.structured,
            };
            if let Some(system) = &row.system {
                entry.insert("system", system);
            }
            if let Some(code) = &row.code {
                entry.insert("code", code);
            }
            Bson::Document(entry)
        })
        .collect();
    let references: Vec<Bson> = rows
        .references
        .iter()
        .map(|row| {
            let mut entry = doc! {
                "param": &row.param,
                "slot": &row.slot,
                "ordinal": row.ordinal,
                "pointer": &row.ref_full,
                "logical": &row.ref_id,
            };
            if let Some(kind) = &row.ref_type {
                entry.insert("kind", kind);
            }
            Bson::Document(entry)
        })
        .collect();
    let uris: Vec<Bson> = rows
        .uris
        .iter()
        .map(|row| {
            Bson::Document(doc! {
                "param": &row.param,
                "slot": &row.slot,
                "ordinal": row.ordinal,
                "value": &row.value,
            })
        })
        .collect();
    let sorts: Vec<Bson> = rows
        .sorts
        .iter()
        .map(|row| {
            Bson::Document(doc! {
                "param": &row.param,
                "value": sort_text(row.sort_text.as_deref()),
            })
        })
        .collect();
    doc! {
        "token": tokens,
        "text": texts,
        "number": numbers,
        "date": dates,
        "quantity": quantities,
        "reference": references,
        "uri": uris,
        "sort": sorts,
    }
}

pub fn sort_text(value: Option<&str>) -> String {
    match value {
        Some(text) => format!("1{text}"),
        None => "2".to_owned(),
    }
}

pub fn document_of(
    envelope: &ResourceEnvelope,
    rows: &Rows,
    sequence: i64,
    current: bool,
) -> Document {
    let key = envelope.last_updated().key();
    let (packed, encoding) = encoded(envelope.raw());
    let mut held = doc! {
        "sequence": sequence,
        "partition": fhir_store::partition(envelope.id().as_str()),
        "resource_type": envelope.resource_type().as_str(),
        "resource_id": envelope.id().as_str(),
        "version_number": version_number(envelope.version_id()).unwrap_or_default(),
        "spec_version": envelope.version().as_str(),
        "last_updated": envelope.last_updated().as_str(),
        "updated_key": key_text(key.seconds(), key.nanos()),
        "is_deleted": envelope.is_deleted(),
        "is_current": current,
        "body": Binary { subtype: BinarySubtype::Generic, bytes: packed },
        "body_encoding": encoding.as_str(),
    };
    held.extend(entries_of(rows));
    held
}

pub fn version_number(version: &VersionId) -> Result<i64, Error> {
    version
        .as_str()
        .parse::<i64>()
        .map_err(|_| Error::Internal(format!("non-numeric version {:?}", version.as_str())))
}

pub fn next_version(current: &VersionId) -> Result<VersionId, Error> {
    VersionId::parse(&(version_number(current)? + 1).to_string())
}

pub struct Record {
    pub sequence: i64,
    pub resource_type: ResourceType,
    pub id: ResourceId,
    pub version: VersionId,
    pub spec: FhirVersion,
    pub last_updated: FhirInstant,
    pub deleted: bool,
    pub body: LazyBody,
}

impl Record {
    pub fn of(held: &Document) -> Result<Record, Error> {
        let stored = match held.get("body") {
            Some(Bson::Binary(binary)) => binary.bytes.clone(),
            _ => return Err(wrong("body")),
        };
        let number = held.get_i64("version_number").map_err(|_| wrong("version"))?;
        Ok(Record {
            sequence: held.get_i64("sequence").map_err(|_| wrong("sequence"))?,
            resource_type: text_of(held, "resource_type")?.parse::<ResourceType>()?,
            id: ResourceId::parse(&text_of(held, "resource_id")?)?,
            version: VersionId::parse(&number.to_string())?,
            spec: text_of(held, "spec_version")?.parse::<FhirVersion>()?,
            last_updated: FhirInstant::parse(&text_of(held, "last_updated")?)?,
            deleted: held.get_bool("is_deleted").map_err(|_| wrong("delete flag"))?,
            body: LazyBody::new(stored, Encoding::parse(&text_of(held, "body_encoding")?)?),
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

pub fn envelope_of(held: &Document) -> Result<ResourceEnvelope, Error> {
    Record::of(held)?.envelope()
}

pub fn pointers_of(held: &Document, param: &str) -> Vec<String> {
    let Ok(rows) = held.get_array("reference") else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(Bson::as_document)
        .filter(|row| row.get_str("param") == Ok(param))
        .filter(|row| row.get_str("slot") == Ok(fhir_store::index::MAIN))
        .filter_map(|row| row.get_str("pointer").ok().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_of_an_instant_orders_as_the_instant_does() {
        assert!(key_text(-100, 0) < key_text(0, 0));
        assert!(key_text(0, 0) < key_text(0, 1));
        assert!(key_text(1, 0) < key_text(2, 0));
        assert_eq!(key_text(0, 0).len(), key_text(253_402_300_799, 0).len());
    }

    #[test]
    fn a_value_that_is_missing_sorts_after_every_value_that_is_not() {
        assert!(sort_text(Some("zzz")) < sort_text(None));
        assert!(sort_text(Some("a")) < sort_text(Some("b")));
    }

    #[test]
    fn a_version_follows_the_one_before_it() {
        let first = VersionId::parse("1").unwrap();
        assert_eq!(next_version(&first).unwrap().as_str(), "2");
        assert_eq!(version_number(&first).unwrap(), 1);
    }

    #[test]
    fn pointers_are_read_from_the_index_and_not_from_a_body() {
        let held = doc! {
            "reference": [
                doc! {"param": "subject", "slot": fhir_store::index::MAIN, "ordinal": 0,
                      "pointer": "Patient/p1", "logical": "p1"},
                doc! {"param": "performer", "slot": fhir_store::index::MAIN, "ordinal": 0,
                      "pointer": "Patient/p2", "logical": "p2"},
            ]
        };
        assert_eq!(pointers_of(&held, "subject"), vec!["Patient/p1".to_owned()]);
        assert!(pointers_of(&held, "unknown").is_empty());
        assert!(pointers_of(&doc! {}, "subject").is_empty());
    }
}
