use crate::audit::AuditEvent;
use fhir_core::{Error, ResourceType};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Mutex;

pub const ORIGIN: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub enum Seal {
    Keyed(Box<[u8]>),
    Open,
}

impl Seal {
    pub fn keyed(secret: &str) -> Seal {
        Seal::Keyed(secret.as_bytes().to_vec().into_boxed_slice())
    }

    pub fn open() -> Seal {
        Seal::Open
    }

    pub fn is_keyed(&self) -> bool {
        matches!(self, Seal::Keyed(_))
    }

    fn digest(&self, message: &[u8]) -> String {
        let held: Vec<u8> = match self {
            Seal::Open => Sha256::digest(message).to_vec(),
            Seal::Keyed(key) => {
                let mut keyed = <Hmac<Sha256> as Mac>::new_from_slice(key)
                    .unwrap_or_else(|_| Mac::new_from_slice(&[0u8; 32]).expect("a fixed key is accepted"));
                keyed.update(message);
                keyed.finalize().into_bytes().to_vec()
            }
        };
        held.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retention {
    pub through: u64,
    pub anchor: String,
    pub horizon: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Interaction(AuditEvent),
    Retention(Retention),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub sequence: u64,
    pub previous: String,
    pub digest: String,
    pub entry: Entry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub sequence: u64,
    pub digest: String,
}

impl Head {
    pub fn origin() -> Head {
        Head {
            sequence: 0,
            digest: ORIGIN.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tamper {
    Altered { sequence: u64 },
    Broken { sequence: u64 },
    Gap { expected: u64, found: u64 },
    Truncated { expected: u64, found: u64 },
}

impl std::fmt::Display for Tamper {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tamper::Altered { sequence } => write!(out, "record {sequence} was altered"),
            Tamper::Broken { sequence } => write!(out, "record {sequence} links to nothing"),
            Tamper::Gap { expected, found } => {
                write!(out, "record {expected} is missing before {found}")
            }
            Tamper::Truncated { expected, found } => {
                write!(out, "the chain reaches {found} where {expected} was expected")
            }
        }
    }
}

pub struct Chain {
    seal: Seal,
    head: Mutex<Head>,
}

impl Chain {
    pub fn new(seal: Seal) -> Chain {
        Chain::resumed(seal, Head::origin())
    }

    pub fn resumed(seal: Seal, head: Head) -> Chain {
        Chain {
            seal,
            head: Mutex::new(head),
        }
    }

    pub fn seal(&self) -> &Seal {
        &self.seal
    }

    pub fn head(&self) -> Result<Head, Error> {
        self.head
            .lock()
            .map(|held| held.clone())
            .map_err(|_| Error::Internal("the trail head cannot be read".to_owned()))
    }

    pub fn append(&self, entry: Entry) -> Result<Sealed, Error> {
        let mut head = self
            .head
            .lock()
            .map_err(|_| Error::Internal("the trail head cannot be advanced".to_owned()))?;
        let sequence = head.sequence + 1;
        let previous = head.digest.clone();
        let digest = self.seal.digest(&message(sequence, &previous, &entry));
        *head = Head {
            sequence,
            digest: digest.clone(),
        };
        Ok(Sealed {
            sequence,
            previous,
            digest,
            entry,
        })
    }
}

fn field(into: &mut Vec<u8>, value: &[u8]) {
    into.extend_from_slice(&(value.len() as u64).to_be_bytes());
    into.extend_from_slice(value);
}

fn optional(into: &mut Vec<u8>, value: Option<&str>) {
    match value {
        None => into.push(0),
        Some(held) => {
            into.push(1);
            field(into, held.as_bytes());
        }
    }
}

fn summary(entry: &Entry) -> Vec<u8> {
    let mut held = Vec::new();
    match entry {
        Entry::Interaction(event) => {
            field(&mut held, b"interaction");
            field(&mut held, event.actor.as_bytes());
            optional(&mut held, event.client.as_deref());
            field(&mut held, event.action.as_str().as_bytes());
            field(&mut held, event.interaction.as_str().as_bytes());
            optional(&mut held, event.resource_type.as_ref().map(ResourceType::as_str));
            optional(&mut held, event.resource_id.as_ref().map(|id| id.as_str()));
            field(&mut held, match event.granted {
                true => b"granted".as_slice(),
                false => b"refused".as_slice(),
            });
            field(&mut held, event.recorded.as_bytes());
        }
        Entry::Retention(retention) => {
            field(&mut held, b"retention");
            field(&mut held, retention.through.to_string().as_bytes());
            field(&mut held, retention.anchor.as_bytes());
            field(&mut held, retention.horizon.as_bytes());
        }
    }
    held
}

fn message(sequence: u64, previous: &str, entry: &Entry) -> Vec<u8> {
    let mut held = Vec::new();
    field(&mut held, &sequence.to_be_bytes());
    field(&mut held, previous.as_bytes());
    held.extend_from_slice(&summary(entry));
    held
}

pub fn digest_of(seal: &Seal, record: &Sealed) -> String {
    seal.digest(&message(record.sequence, &record.previous, &record.entry))
}

pub fn anchor_of(records: &[Sealed]) -> Head {
    records
        .iter()
        .filter_map(|record| match &record.entry {
            Entry::Retention(retention) => Some(retention),
            Entry::Interaction(_) => None,
        })
        .max_by_key(|retention| retention.through)
        .map(|retention| Head {
            sequence: retention.through,
            digest: retention.anchor.clone(),
        })
        .unwrap_or_else(Head::origin)
}

pub fn verify(seal: &Seal, records: &[Sealed]) -> Result<Head, Tamper> {
    let anchor = anchor_of(records);
    let kept: Vec<Sealed> = records
        .iter()
        .filter(|record| record.sequence > anchor.sequence)
        .cloned()
        .collect();
    verify_from(seal, &kept, &anchor)
}

pub fn verify_from(seal: &Seal, records: &[Sealed], anchor: &Head) -> Result<Head, Tamper> {
    let mut reached = anchor.clone();
    for record in records {
        if record.sequence != reached.sequence + 1 {
            return match reached.sequence == anchor.sequence {
                true => Err(Tamper::Truncated {
                    expected: anchor.sequence + 1,
                    found: record.sequence,
                }),
                false => Err(Tamper::Gap {
                    expected: reached.sequence + 1,
                    found: record.sequence,
                }),
            };
        }
        let recomputed = seal.digest(&message(record.sequence, &record.previous, &record.entry));
        if recomputed != record.digest {
            return Err(Tamper::Altered {
                sequence: record.sequence,
            });
        }
        if record.previous != reached.digest {
            return Err(Tamper::Broken {
                sequence: record.sequence,
            });
        }
        reached = Head {
            sequence: record.sequence,
            digest: record.digest.clone(),
        };
    }
    Ok(reached)
}

pub fn verify_against(seal: &Seal, records: &[Sealed], head: &Head) -> Result<(), Tamper> {
    let reached = verify(seal, records)?;
    match reached == *head {
        true => Ok(()),
        false => Err(Tamper::Truncated {
            expected: head.sequence,
            found: reached.sequence,
        }),
    }
}

pub fn expired(records: &[Sealed], horizon: &str) -> Option<Retention> {
    records
        .iter()
        .filter(|record| recorded_at(&record.entry).is_some_and(|at| at < horizon))
        .max_by_key(|record| record.sequence)
        .map(|record| Retention {
            through: record.sequence,
            anchor: record.digest.clone(),
            horizon: horizon.to_owned(),
        })
}

fn recorded_at(entry: &Entry) -> Option<&str> {
    match entry {
        Entry::Interaction(event) => Some(&event.recorded),
        Entry::Retention(retention) => Some(&retention.horizon),
    }
}

pub fn published(record: &Sealed) -> Value {
    let mut held = json!({
        "sequence": record.sequence,
        "previous": record.previous,
        "digest": record.digest,
    });
    match &record.entry {
        Entry::Interaction(event) => {
            held["kind"] = json!("interaction");
            held["actor"] = json!(event.actor);
            held["action"] = json!(event.action.as_str());
            held["outcome"] = json!(match event.granted {
                true => "granted",
                false => "refused",
            });
            held["recorded"] = json!(event.recorded);
            if let Some(client) = &event.client {
                held["client"] = json!(client);
            }
            if let Some(kind) = event.resource_type {
                held["type"] = json!(kind.as_str());
            }
            if let Some(id) = &event.resource_id {
                held["id"] = json!(id.as_str());
            }
        }
        Entry::Retention(retention) => {
            held["kind"] = json!("retention");
            held["through"] = json!(retention.through);
            held["anchor"] = json!(retention.anchor);
            held["horizon"] = json!(retention.horizon);
        }
    }
    held
}

pub fn export(records: &[Sealed]) -> String {
    records
        .iter()
        .map(|record| published(record).to_string())
        .collect::<Vec<String>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::security::scope::DataAction;
    use fhir_core::ResourceId;

    fn event(actor: &str, at: &str) -> Entry {
        let mut held = AuditEvent::allowed(actor, DataAction::Read).of(
            Some("Patient".parse().unwrap()),
            Some(ResourceId::parse("pt-1").unwrap()),
        );
        held.recorded = at.to_owned();
        Entry::Interaction(held)
    }

    fn trail(seal: Seal, count: u64) -> (Chain, Vec<Sealed>) {
        let chain = Chain::new(seal);
        let records = (1..=count)
            .map(|n| chain.append(event(&format!("actor-{n}"), &format!("2026-09-07T00:00:0{n}Z"))).unwrap())
            .collect();
        (chain, records)
    }

    #[test]
    fn an_untouched_trail_verifies_to_its_head() {
        let (chain, records) = trail(Seal::keyed("k"), 4);
        let head = verify(&Seal::keyed("k"), &records).unwrap();
        assert_eq!(head, chain.head().unwrap());
        assert_eq!(head.sequence, 4);
    }

    #[test]
    fn a_chain_starts_at_the_origin_and_counts_from_one() {
        let (_, records) = trail(Seal::open(), 2);
        assert_eq!(records[0].sequence, 1);
        assert_eq!(records[0].previous, ORIGIN);
        assert_eq!(records[1].previous, records[0].digest);
    }

    #[test]
    fn an_altered_record_is_detected() {
        let (_, mut records) = trail(Seal::keyed("k"), 4);
        records[2].entry = event("someone-else", "2026-09-07T00:00:03Z");
        assert_eq!(
            verify(&Seal::keyed("k"), &records),
            Err(Tamper::Altered { sequence: 3 })
        );
    }

    #[test]
    fn a_removed_record_is_detected_as_a_gap() {
        let (_, mut records) = trail(Seal::keyed("k"), 4);
        records.remove(2);
        assert_eq!(
            verify(&Seal::keyed("k"), &records),
            Err(Tamper::Gap { expected: 3, found: 4 })
        );
    }

    #[test]
    fn a_record_removed_from_the_end_is_detected_against_a_known_head() {
        let (chain, mut records) = trail(Seal::keyed("k"), 4);
        let head = chain.head().unwrap();
        records.pop();
        assert_eq!(
            verify_against(&Seal::keyed("k"), &records, &head),
            Err(Tamper::Truncated { expected: 4, found: 3 })
        );
    }

    #[test]
    fn a_record_removed_from_the_front_is_detected() {
        let (_, mut records) = trail(Seal::keyed("k"), 4);
        records.remove(0);
        assert_eq!(
            verify(&Seal::keyed("k"), &records),
            Err(Tamper::Truncated { expected: 1, found: 2 })
        );
    }

    #[test]
    fn a_relinked_record_is_detected() {
        let (_, mut records) = trail(Seal::keyed("k"), 4);
        records[2].previous = ORIGIN.to_owned();
        assert_eq!(
            verify(&Seal::keyed("k"), &records),
            Err(Tamper::Altered { sequence: 3 })
        );
    }

    #[test]
    fn a_rewrite_without_the_key_does_not_verify() {
        let (_, mut records) = trail(Seal::keyed("k"), 3);
        let forger = Seal::keyed("other");
        for record in records.iter_mut() {
            record.entry = event("forged", "2026-09-07T00:00:09Z");
            record.digest = forger.digest(&message(record.sequence, &record.previous, &record.entry));
        }
        assert!(verify(&Seal::keyed("k"), &records).is_err());
    }

    #[test]
    fn an_intact_trail_verifies_against_its_own_head() {
        let (chain, records) = trail(Seal::keyed("k"), 3);
        assert_eq!(
            verify_against(&Seal::keyed("k"), &records, &chain.head().unwrap()),
            Ok(())
        );
    }

    #[test]
    fn a_retained_trail_still_verifies_from_the_removal_it_names() {
        let (chain, records) = trail(Seal::keyed("k"), 5);
        let removal = expired(&records, "2026-09-07T00:00:03Z").unwrap();
        assert_eq!(removal.through, 2);
        let sealed = chain.append(Entry::Retention(removal.clone())).unwrap();
        let mut kept: Vec<Sealed> = records
            .into_iter()
            .filter(|record| record.sequence > removal.through)
            .collect();
        kept.push(sealed);
        let head = verify(&Seal::keyed("k"), &kept).unwrap();
        assert_eq!(head, chain.head().unwrap());
        assert_eq!(head.sequence, 6);
    }

    #[test]
    fn a_removal_beyond_what_it_named_is_still_a_gap() {
        let (chain, records) = trail(Seal::keyed("k"), 5);
        let removal = expired(&records, "2026-09-07T00:00:03Z").unwrap();
        let sealed = chain.append(Entry::Retention(removal.clone())).unwrap();
        let mut kept: Vec<Sealed> = records
            .into_iter()
            .filter(|record| record.sequence > removal.through + 1)
            .collect();
        kept.push(sealed);
        assert_eq!(
            verify(&Seal::keyed("k"), &kept),
            Err(Tamper::Truncated { expected: 3, found: 4 })
        );
    }

    #[test]
    fn a_horizon_covering_nothing_removes_nothing() {
        let (_, records) = trail(Seal::keyed("k"), 3);
        assert_eq!(expired(&records, "2026-09-07T00:00:00Z"), None);
    }

    #[test]
    fn an_exported_record_carries_the_chain_and_nothing_more() {
        let (_, records) = trail(Seal::keyed("k"), 1);
        let line = published(&records[0]);
        let mut names: Vec<&String> = line.as_object().unwrap().keys().collect();
        names.sort();
        assert_eq!(
            names,
            vec!["action", "actor", "digest", "id", "kind", "outcome", "previous", "recorded", "sequence", "type"]
        );
    }

    #[test]
    fn an_export_holds_one_line_for_each_record() {
        let (_, records) = trail(Seal::open(), 3);
        let text = export(&records);
        assert_eq!(text.lines().count(), 3);
        assert!(text.lines().all(|line| line.contains("\"digest\"")));
    }

    #[test]
    fn a_keyed_chain_and_an_open_chain_do_not_agree() {
        let (_, keyed) = trail(Seal::keyed("k"), 1);
        let (_, open) = trail(Seal::open(), 1);
        assert_ne!(keyed[0].digest, open[0].digest);
        assert!(Seal::keyed("k").is_keyed());
        assert!(!Seal::open().is_keyed());
    }

    #[test]
    fn a_removal_is_published_as_a_removal() {
        let removal = Retention {
            through: 7,
            anchor: ORIGIN.to_owned(),
            horizon: "2026-09-07T00:00:00Z".to_owned(),
        };
        let line = published(&Chain::new(Seal::open()).append(Entry::Retention(removal)).unwrap());
        assert_eq!(line["kind"], "retention");
        assert_eq!(line["through"], 7);
        assert!(line.get("actor").is_none());
    }

    #[test]
    fn a_chain_resumes_from_a_head_it_is_given() {
        let (chain, records) = trail(Seal::keyed("k"), 2);
        let resumed = Chain::resumed(Seal::keyed("k"), chain.head().unwrap());
        let next = resumed.append(event("actor-3", "2026-09-07T00:00:03Z")).unwrap();
        assert_eq!(next.sequence, 3);
        assert_eq!(next.previous, records[1].digest);
    }

    #[test]
    fn every_tamper_reads_as_a_sentence() {
        let reported = [
            Tamper::Altered { sequence: 1 },
            Tamper::Broken { sequence: 2 },
            Tamper::Gap { expected: 3, found: 4 },
            Tamper::Truncated { expected: 5, found: 4 },
        ];
        assert!(reported.iter().all(|held| held.to_string().len() > 8));
    }

    #[test]
    fn a_record_naming_no_resource_still_seals() {
        let chain = Chain::new(Seal::keyed("k"));
        let bare = AuditEvent::allowed("actor", DataAction::Export).of(None, None).refused();
        let sealed = chain.append(Entry::Interaction(bare)).unwrap();
        assert_eq!(verify(&Seal::keyed("k"), std::slice::from_ref(&sealed)).unwrap().sequence, 1);
        assert_eq!(published(&sealed)["outcome"], "refused");
    }

    #[test]
    fn a_broken_link_that_still_hashes_is_detected() {
        let seal = Seal::keyed("k");
        let chain = Chain::new(Seal::keyed("k"));
        let first = chain.append(event("a", "2026-09-07T00:00:01Z")).unwrap();
        let mut second = chain.append(event("b", "2026-09-07T00:00:02Z")).unwrap();
        second.previous = ORIGIN.to_owned();
        second.digest = seal.digest(&message(second.sequence, &second.previous, &second.entry));
        assert_eq!(
            verify(&seal, &[first, second]),
            Err(Tamper::Broken { sequence: 2 })
        );
    }
}
