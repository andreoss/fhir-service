use async_trait::async_trait;
use axum::extract::{RawQuery, State};
use axum::http::header::{self, HeaderMap};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fhir_core::security::scope::DataAction;
use fhir_core::{Error, FhirVersion, ResourceEnvelope, ResourceId, ResourceType};
use fhir_store::trail::{self, Chain, Entry, Head, Retention, Seal, Sealed, Tamper};
use fhir_store::{Audit, AuditEvent, Interaction, ResourceStore, SearchQuery};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::app::AppState;
use crate::handlers::AppError;

const FHIR_JSON: &str = "application/fhir+json";
const BEFORE: &str = "_before";
const AUDIT_EVENT: &str = "AuditEvent";
const AGENT_TYPE: &str = "http://terminology.hl7.org/CodeSystem/extra-security-role-type";
const ACTION_SYSTEM: &str = "urn:fhir-service:data-action";
const ENTRY_SYSTEM: &str = "urn:fhir-service:audit-entry";
const SEQUENCE_URL: &str = "urn:fhir-service:audit-sequence";
const PREVIOUS_URL: &str = "urn:fhir-service:audit-previous";
const DIGEST_URL: &str = "urn:fhir-service:audit-digest";
const THROUGH_URL: &str = "urn:fhir-service:audit-through";
const ANCHOR_URL: &str = "urn:fhir-service:audit-anchor";
const ACTOR_URL: &str = "urn:fhir-service:audit-actor";
const CLIENT_URL: &str = "urn:fhir-service:audit-client";
const ACTION_URL: &str = "urn:fhir-service:audit-action";
const GRANTED_URL: &str = "urn:fhir-service:audit-granted";
const TYPE_URL: &str = "urn:fhir-service:audit-resource-type";
const SUBJECT_URL: &str = "urn:fhir-service:audit-resource-id";
const OUTCOME_SYSTEM: &str = "http://terminology.hl7.org/CodeSystem/audit-event-outcome";
const RETENTION_CODE: &str = "retention";
const RETENTION_ACTOR: &str = "retention";
const OBSERVER: &str = "audit trail";
const OUTCOME_GRANTED: &str = "0";
const OUTCOME_REFUSED: &str = "8";

pub const ENV_TRAIL_KEY: &str = "FHIR_AUDIT_KEY";

pub struct StoredTrail {
    store: Arc<dyn ResourceStore>,
    version: FhirVersion,
    chain: Chain,
}

pub fn configured_seal() -> Seal {
    match std::env::var(ENV_TRAIL_KEY) {
        Ok(secret) if !secret.trim().is_empty() => Seal::keyed(secret.trim()),
        _ => Seal::open(),
    }
}

impl StoredTrail {
    pub fn new(store: Arc<dyn ResourceStore>, version: FhirVersion) -> StoredTrail {
        StoredTrail {
            store,
            version,
            chain: Chain::new(configured_seal()),
        }
    }

    pub async fn resumed(
        store: Arc<dyn ResourceStore>,
        version: FhirVersion,
        seal: Seal,
    ) -> Result<StoredTrail, Error> {
        let records = collected(store.as_ref()).await?;
        let head = trail::verify(&seal, &records).unwrap_or_else(|_| last_of(&records));
        Ok(StoredTrail {
            store,
            version,
            chain: Chain::resumed(seal, head),
        })
    }

    pub fn sealed_with(self, seal: Seal) -> StoredTrail {
        StoredTrail {
            chain: Chain::new(seal),
            ..self
        }
    }

    pub fn head(&self) -> Result<Head, Error> {
        self.chain.head()
    }

    pub fn seal(&self) -> &Seal {
        self.chain.seal()
    }

    pub async fn collected(&self) -> Result<Vec<Sealed>, Error> {
        collected(self.store.as_ref()).await
    }

    pub async fn read(&self) -> Result<(Vec<Sealed>, Option<u64>), Error> {
        read(self.store.as_ref(), Some(self.version)).await
    }

    pub async fn retain(&self, horizon: &str) -> Result<usize, Error> {
        let records = self.collected().await?;
        let Some(removal) = trail::expired(&records, horizon) else {
            return Ok(0);
        };
        let sealed = self.chain.append(Entry::Retention(removal.clone()))?;
        self.write(&sealed).await?;
        let mut removed = 0;
        for record in records
            .iter()
            .filter(|held| held.sequence <= removal.through)
        {
            let id = identifier(record.sequence).parse::<ResourceId>()?;
            let key = fhir_core::ResourceKey::new(AUDIT_EVENT.parse()?, id);
            match self.store.hard_delete(&key).await {
                Ok(()) => removed += 1,
                Err(Error::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(removed)
    }

    async fn write(&self, sealed: &Sealed) -> Result<(), Error> {
        let mut body = record(sealed, self.version);
        fhir_core::with_assigned_meta(&mut body)?;
        let bytes =
            serde_json::to_vec(&body).map_err(|error| Error::Internal(error.to_string()))?;
        let envelope = ResourceEnvelope::parse(self.version, &bytes)?;
        self.store.create(envelope).await?;
        Ok(())
    }
}

fn last_of(records: &[Sealed]) -> Head {
    records
        .iter()
        .max_by_key(|record| record.sequence)
        .map(|record| Head {
            sequence: record.sequence,
            digest: record.digest.clone(),
        })
        .unwrap_or_else(Head::origin)
}

pub fn identifier(sequence: u64) -> String {
    format!("au-{sequence:012}")
}

pub fn record(sealed: &Sealed, version: FhirVersion) -> Value {
    let mut held = match &sealed.entry {
        Entry::Interaction(event) => interaction(event, version),
        Entry::Retention(retention) => removal(retention, version),
    };
    held["id"] = json!(identifier(sealed.sequence));
    held["source"] = source_of(version);
    let mut extensions = vec![
        json!({"url": SEQUENCE_URL, "valueUnsignedInt": sealed.sequence}),
        json!({"url": PREVIOUS_URL, "valueString": sealed.previous}),
        json!({"url": DIGEST_URL, "valueString": sealed.digest}),
    ];
    match &sealed.entry {
        Entry::Retention(retention) => {
            extensions.push(json!({"url": THROUGH_URL, "valueUnsignedInt": retention.through}));
            extensions.push(json!({"url": ANCHOR_URL, "valueString": retention.anchor}));
        }
        Entry::Interaction(event) => {
            extensions.push(json!({"url": ACTOR_URL, "valueString": event.actor}));
            extensions.push(json!({"url": ACTION_URL, "valueString": event.action.as_str()}));
            extensions.push(json!({"url": GRANTED_URL, "valueBoolean": event.granted}));
            if let Some(client) = &event.client {
                extensions.push(json!({"url": CLIENT_URL, "valueString": client}));
            }
            if let Some(kind) = event.resource_type {
                extensions.push(json!({"url": TYPE_URL, "valueString": kind.as_str()}));
            }
            if let Some(id) = &event.resource_id {
                extensions.push(json!({"url": SUBJECT_URL, "valueString": id.as_str()}));
            }
        }
    }
    held["extension"] = json!(extensions);
    held
}

fn source_of(version: FhirVersion) -> Value {
    match version {
        FhirVersion::Stu3 => json!({"identifier": {"value": OBSERVER}}),
        _ => json!({"observer": {"display": OBSERVER}}),
    }
}

fn coded(version: FhirVersion, system: &str, code: &str) -> Value {
    match version {
        FhirVersion::R5 => json!({"coding": [{"system": system, "code": code}]}),
        _ => json!({"system": system, "code": code}),
    }
}

fn kinded(version: FhirVersion) -> &'static str {
    match version {
        FhirVersion::R5 => "code",
        _ => "type",
    }
}

fn outcome_of(version: FhirVersion, granted: bool) -> Value {
    let code = match granted {
        true => OUTCOME_GRANTED,
        false => OUTCOME_REFUSED,
    };
    match version {
        FhirVersion::R5 => json!({"code": {"system": OUTCOME_SYSTEM, "code": code}}),
        _ => json!(code),
    }
}

fn actor_of(version: FhirVersion, actor: &str, client: Option<&str>, requestor: bool) -> Value {
    let mut named = json!({"value": actor});
    if let Some(client) = client {
        named["system"] = json!(client);
    }
    match version {
        FhirVersion::Stu3 => json!({"userId": named, "requestor": requestor}),
        _ => json!({
            "type": {"coding": [{"system": AGENT_TYPE, "code": "humanuser"}]},
            "who": {"identifier": named},
            "requestor": requestor,
        }),
    }
}

fn subject_entity(version: FhirVersion, reference: &str) -> Value {
    match version {
        FhirVersion::Stu3 => json!([{"reference": {"reference": reference}}]),
        _ => json!([{"what": {"reference": reference}}]),
    }
}

fn interaction(event: &AuditEvent, version: FhirVersion) -> Value {
    let mut entry = json!({
        "resourceType": AUDIT_EVENT,
        "recorded": event.recorded,
        "outcome": outcome_of(version, event.granted),
        "agent": [actor_of(version, &event.actor, event.client.as_deref(), true)],
    });
    entry[kinded(version)] = coded(version, ACTION_SYSTEM, event.action.as_str());
    entry["action"] = json!(event.interaction.as_str());
    if let (Some(kind), Some(id)) = (event.resource_type, &event.resource_id) {
        entry["entity"] = subject_entity(version, &format!("{}/{}", kind.as_str(), id.as_str()));
    }
    entry
}

fn removal(retention: &Retention, version: FhirVersion) -> Value {
    let mut entry = json!({
        "resourceType": AUDIT_EVENT,
        "recorded": retention.horizon,
        "outcome": outcome_of(version, true),
        "agent": [actor_of(version, RETENTION_ACTOR, None, false)],
    });
    entry[kinded(version)] = coded(version, ENTRY_SYSTEM, RETENTION_CODE);
    entry
}

fn extension(body: &Value, url: &str) -> Option<Value> {
    body.get("extension")?
        .as_array()?
        .iter()
        .find(|held| held.get("url").and_then(Value::as_str) == Some(url))
        .cloned()
}

fn text_at(body: &Value, url: &str) -> Option<String> {
    extension(body, url)?
        .get("valueString")?
        .as_str()
        .map(str::to_owned)
}

fn number_at(body: &Value, url: &str) -> Option<u64> {
    extension(body, url)?.get("valueUnsignedInt")?.as_u64()
}

pub fn sealed_of(body: &Value) -> Result<Sealed, Error> {
    let unchained = || Error::Internal("a trail record carries no chain".to_owned());
    let sequence = number_at(body, SEQUENCE_URL).ok_or_else(unchained)?;
    let previous = text_at(body, PREVIOUS_URL).ok_or_else(unchained)?;
    let digest = text_at(body, DIGEST_URL).ok_or_else(unchained)?;
    Ok(Sealed {
        sequence,
        previous,
        digest,
        entry: entry_of(body)?,
    })
}

fn truth_at(body: &Value, url: &str) -> Option<bool> {
    extension(body, url)?.get("valueBoolean")?.as_bool()
}

fn entry_of(body: &Value) -> Result<Entry, Error> {
    let unreadable = || Error::Internal("a trail record cannot be read".to_owned());
    let recorded = body
        .get("recorded")
        .and_then(Value::as_str)
        .ok_or_else(unreadable)?
        .to_owned();
    if let Some(through) = number_at(body, THROUGH_URL) {
        return Ok(Entry::Retention(Retention {
            through,
            anchor: text_at(body, ANCHOR_URL).ok_or_else(unreadable)?,
            horizon: recorded,
        }));
    }
    let action = text_at(body, ACTION_URL)
        .and_then(|named| DataAction::named(&named))
        .ok_or_else(unreadable)?;
    let interaction = body
        .get("action")
        .and_then(Value::as_str)
        .and_then(Interaction::named)
        .ok_or_else(unreadable)?;
    let resource_type = match text_at(body, TYPE_URL) {
        Some(named) => Some(named.parse::<ResourceType>()?),
        None => None,
    };
    let resource_id = match text_at(body, SUBJECT_URL) {
        Some(named) => Some(ResourceId::parse(&named)?),
        None => None,
    };
    Ok(Entry::Interaction(AuditEvent {
        actor: text_at(body, ACTOR_URL).ok_or_else(unreadable)?,
        client: text_at(body, CLIENT_URL),
        action,
        interaction,
        resource_type,
        resource_id,
        granted: truth_at(body, GRANTED_URL).ok_or_else(unreadable)?,
        recorded,
    }))
}

pub async fn collected(store: &dyn ResourceStore) -> Result<Vec<Sealed>, Error> {
    Ok(read(store, None).await?.0)
}

pub async fn read(
    store: &dyn ResourceStore,
    version: Option<FhirVersion>,
) -> Result<(Vec<Sealed>, Option<u64>), Error> {
    let kind = AUDIT_EVENT.parse::<ResourceType>()?;
    let page = store.search(&SearchQuery::of_type(kind)).await?;
    let mut records = Vec::new();
    let mut rewritten = Vec::new();
    for envelope in &page.entries {
        let body: Value = serde_json::from_slice(envelope.raw())
            .map_err(|error| Error::Internal(error.to_string()))?;
        let sealed = sealed_of(&body)?;
        if let Some(version) = version {
            if !faithful(&sealed, &body, version) {
                rewritten.push(sealed.sequence);
            }
        }
        records.push(sealed);
    }
    records.sort_by_key(|record| record.sequence);
    rewritten.sort_unstable();
    Ok((records, rewritten.first().copied()))
}

fn faithful(sealed: &Sealed, body: &Value, version: FhirVersion) -> bool {
    let mut rendered = record(sealed, version);
    let mut held = body.clone();
    if let Some(object) = rendered.as_object_mut() {
        object.remove("meta");
    }
    if let Some(object) = held.as_object_mut() {
        object.remove("meta");
    }
    rendered == held
}

pub fn verdict(seal: &Seal, records: &[Sealed], rewritten: Option<u64>) -> Value {
    if let Some(sequence) = rewritten {
        return json!({
            "resourceType": "Parameters",
            "parameter": [
                {"name": "verified", "valueBoolean": false},
                {"name": "records", "valueUnsignedInt": records.len()},
                {"name": "fault", "valueCode": "rewritten"},
                {"name": "detail", "valueString": format!("record {sequence} is not what it seals")},
                {"name": "keyed", "valueBoolean": seal.is_keyed()},
            ],
        });
    }
    match trail::verify(seal, records) {
        Ok(head) => json!({
            "resourceType": "Parameters",
            "parameter": [
                {"name": "verified", "valueBoolean": true},
                {"name": "records", "valueUnsignedInt": records.len()},
                {"name": "sequence", "valueUnsignedInt": head.sequence},
                {"name": "digest", "valueString": head.digest},
                {"name": "keyed", "valueBoolean": seal.is_keyed()},
            ],
        }),
        Err(found) => json!({
            "resourceType": "Parameters",
            "parameter": [
                {"name": "verified", "valueBoolean": false},
                {"name": "records", "valueUnsignedInt": records.len()},
                {"name": "fault", "valueCode": fault_of(&found)},
                {"name": "detail", "valueString": found.to_string()},
                {"name": "keyed", "valueBoolean": seal.is_keyed()},
            ],
        }),
    }
}

fn fault_of(found: &Tamper) -> &'static str {
    match found {
        Tamper::Altered { .. } => "altered",
        Tamper::Broken { .. } => "broken",
        Tamper::Gap { .. } => "gap",
        Tamper::Truncated { .. } => "truncated",
    }
}

async fn held(state: &AppState) -> Result<Arc<StoredTrail>, Error> {
    state.trail.clone().ok_or(Error::NotFound)
}

pub async fn verified(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let kind = AUDIT_EVENT.parse::<ResourceType>()?;
    crate::handlers::allowed(&state, &headers, DataAction::Read, Some(kind), None).await?;
    let trail = held(&state).await?;
    let (records, rewritten) = trail.read().await?;
    let report = verdict(trail.seal(), &records, rewritten);
    Ok(reported(report))
}

pub async fn exported(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let kind = AUDIT_EVENT.parse::<ResourceType>()?;
    crate::handlers::allowed(&state, &headers, DataAction::Export, Some(kind), None).await?;
    let trail = held(&state).await?;
    let body = trail::export(&trail.collected().await?);
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, fhir_store::NDJSON),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response())
}

pub async fn retained(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let kind = AUDIT_EVENT.parse::<ResourceType>()?;
    crate::handlers::allowed(&state, &headers, DataAction::BulkDelete, Some(kind), None).await?;
    let horizon = crate::query::param(query.as_deref(), BEFORE)
        .ok_or_else(|| Error::InvalidParameter(format!("{BEFORE} is required")))?;
    fhir_core::FhirInstant::parse(&horizon)?;
    let trail = held(&state).await?;
    let removed = trail.retain(&horizon).await?;
    Ok(reported(json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "removed", "valueUnsignedInt": removed},
            {"name": "horizon", "valueString": horizon},
        ],
    })))
}

fn reported(body: Value) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, FHIR_JSON),
            (header::CACHE_CONTROL, "no-store"),
        ],
        serde_json::to_vec(&body).unwrap_or_default(),
    )
        .into_response()
}

#[async_trait]
impl Audit for StoredTrail {
    async fn record(&self, event: AuditEvent) -> Result<(), Error> {
        let sealed = self.chain.append(Entry::Interaction(event))?;
        self.write(&sealed).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::ResourceId;

    fn event() -> AuditEvent {
        AuditEvent::allowed("practitioner-1", DataAction::Read)
            .by(Some("app-1".to_owned()))
            .of(
                Some("Patient".parse().unwrap()),
                Some(ResourceId::parse("pt-1").unwrap()),
            )
    }

    fn sealed(event: AuditEvent) -> Sealed {
        Chain::new(Seal::open())
            .append(Entry::Interaction(event))
            .expect("a chain appends")
    }

    #[test]
    fn a_record_names_the_actor_the_action_and_the_resource() {
        let written = record(&sealed(event()), FhirVersion::R4);
        assert_eq!(written["resourceType"], AUDIT_EVENT);
        assert_eq!(written["type"]["code"], "read");
        assert_eq!(
            written["agent"][0]["who"]["identifier"]["value"],
            "practitioner-1"
        );
        assert_eq!(written["entity"][0]["what"]["reference"], "Patient/pt-1");
        assert_eq!(written["outcome"], OUTCOME_GRANTED);
        assert!(!written["recorded"].as_str().unwrap_or_default().is_empty());
    }

    #[test]
    fn a_refusal_is_recorded_as_one() {
        let written = record(&sealed(event().refused()), FhirVersion::R4);
        assert_eq!(written["outcome"], OUTCOME_REFUSED);
    }

    #[test]
    fn a_record_without_a_resource_names_none() {
        let bare = AuditEvent::allowed("practitioner-1", DataAction::Export).of(None, None);
        let written = record(&sealed(bare), FhirVersion::R4);
        assert!(written.get("entity").is_none());
        assert_eq!(written["type"]["code"], "export");
    }

    #[test]
    fn a_record_carries_its_place_in_the_chain() {
        let written = record(&sealed(event()), FhirVersion::R4);
        assert_eq!(written["id"], "au-000000000001");
        assert_eq!(written["extension"][0]["valueUnsignedInt"], 1);
        assert_eq!(written["extension"][1]["valueString"], trail::ORIGIN);
        assert!(!written["extension"][2]["valueString"]
            .as_str()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_written_record_reads_back_as_the_record_it_sealed() {
        let held = sealed(event());
        assert_eq!(sealed_of(&record(&held, FhirVersion::R4)).unwrap(), held);
    }

    #[test]
    fn a_refusal_over_a_type_reads_back_as_a_refusal() {
        let held = sealed(
            AuditEvent::allowed("practitioner-1", DataAction::Export)
                .of(Some("Patient".parse().unwrap()), None)
                .refused(),
        );
        assert_eq!(sealed_of(&record(&held, FhirVersion::R4)).unwrap(), held);
    }

    #[test]
    fn a_removal_reads_back_as_a_removal() {
        let removal = Retention {
            through: 4,
            anchor: trail::ORIGIN.to_owned(),
            horizon: "2026-09-07T00:00:00Z".to_owned(),
        };
        let held = Chain::new(Seal::open())
            .append(Entry::Retention(removal))
            .unwrap();
        let written = record(&held, FhirVersion::R4);
        assert_eq!(written["type"]["code"], RETENTION_CODE);
        assert_eq!(sealed_of(&written).unwrap(), held);
    }

    #[test]
    fn a_record_without_a_chain_is_refused() {
        let mut written = record(&sealed(event()), FhirVersion::R4);
        written["extension"] = json!([]);
        assert!(sealed_of(&written).is_err());
    }

    #[test]
    fn a_verdict_reports_a_verified_trail() {
        let chain = Chain::new(Seal::keyed("k"));
        let records: Vec<Sealed> = (0..3)
            .map(|_| chain.append(Entry::Interaction(event())).unwrap())
            .collect();
        let report = verdict(&Seal::keyed("k"), &records, None);
        assert_eq!(report["parameter"][0]["valueBoolean"], true);
        assert_eq!(report["parameter"][2]["valueUnsignedInt"], 3);
        assert_eq!(report["parameter"][4]["valueBoolean"], true);
    }

    #[test]
    fn a_verdict_names_the_fault_it_found() {
        let chain = Chain::new(Seal::open());
        let mut records: Vec<Sealed> = (0..3)
            .map(|_| chain.append(Entry::Interaction(event())).unwrap())
            .collect();
        records.remove(1);
        let report = verdict(&Seal::open(), &records, None);
        assert_eq!(report["parameter"][0]["valueBoolean"], false);
        assert_eq!(report["parameter"][2]["valueCode"], "gap");
        assert_eq!(report["parameter"][4]["valueBoolean"], false);
    }

    #[test]
    fn every_fault_has_a_name() {
        assert_eq!(fault_of(&Tamper::Altered { sequence: 1 }), "altered");
        assert_eq!(fault_of(&Tamper::Broken { sequence: 1 }), "broken");
        assert_eq!(
            fault_of(&Tamper::Gap {
                expected: 1,
                found: 2
            }),
            "gap"
        );
        assert_eq!(
            fault_of(&Tamper::Truncated {
                expected: 2,
                found: 1
            }),
            "truncated"
        );
    }

    #[test]
    fn an_identifier_orders_with_its_sequence() {
        assert!(identifier(9) < identifier(10));
        assert_eq!(identifier(1), "au-000000000001");
    }

    #[test]
    fn a_written_record_is_valid_against_every_version_it_is_written_for() {
        for version in [
            FhirVersion::Stu3,
            FhirVersion::R4,
            FhirVersion::R4b,
            FhirVersion::R5,
        ] {
            for held in [sealed(event()), sealed(event().refused())] {
                let mut body = record(&held, version);
                fhir_core::with_assigned_meta(&mut body).expect("meta is assignable");
                let report = fhir_core::validate::validate(&fhir_core::validate::Request {
                    version,
                    resource_type: Some(AUDIT_EVENT.parse().unwrap()),
                    id: None,
                    profile: None,
                    resolved: None,
                    mode: fhir_core::validate::Mode::Create,
                    body: &body,
                });
                assert!(
                    !report.has_errors(),
                    "{version:?}: {}",
                    report.to_fhir_json_text()
                );
            }
        }
    }

    #[test]
    fn a_removal_is_valid_against_every_version_it_is_written_for() {
        let removal = Retention {
            through: 2,
            anchor: trail::ORIGIN.to_owned(),
            horizon: "2026-09-07T00:00:00Z".to_owned(),
        };
        for version in [
            FhirVersion::Stu3,
            FhirVersion::R4,
            FhirVersion::R4b,
            FhirVersion::R5,
        ] {
            let held = Chain::new(Seal::open())
                .append(Entry::Retention(removal.clone()))
                .unwrap();
            let mut body = record(&held, version);
            fhir_core::with_assigned_meta(&mut body).expect("meta is assignable");
            let report = fhir_core::validate::validate(&fhir_core::validate::Request {
                version,
                resource_type: Some(AUDIT_EVENT.parse().unwrap()),
                id: None,
                profile: None,
                resolved: None,
                mode: fhir_core::validate::Mode::Create,
                body: &body,
            });
            assert!(
                !report.has_errors(),
                "{version:?}: {}",
                report.to_fhir_json_text()
            );
        }
    }

    #[test]
    fn a_configured_key_seals_the_chain() {
        assert!(!configured_seal().is_keyed() || std::env::var(ENV_TRAIL_KEY).is_ok());
    }
}
