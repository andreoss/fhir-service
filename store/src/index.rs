use fhir_core::search::registry::{ParamDef, SubDef, Target};
use fhir_core::search::{select, sort_value, IndexKey, ParameterSpec, SortValue, ValueType};
use fhir_core::{InstantPeriod, ResourceEnvelope};
use serde_json::Value;
use std::sync::Arc;

pub const MAIN: &str = "main";
pub const IDENTIFIER: &str = "identifier";
pub const LEFT: &str = "left";
pub const RIGHT: &str = "right";
pub const PLAIN: &str = "plain";
pub const WORDS: &str = "words";
pub const NARRATIVE: &str = "narrative";
pub const PRESENCE: &str = "presence";
pub const OF_TYPE: &str = "of_type";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRow {
    pub param: String,
    pub slot: String,
    pub ordinal: i32,
    pub system: Option<String>,
    pub code: String,
    pub code_tail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRow {
    pub param: String,
    pub slot: String,
    pub ordinal: i32,
    pub value: String,
    pub folded: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NumberRow {
    pub param: String,
    pub slot: String,
    pub ordinal: i32,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateRow {
    pub param: String,
    pub slot: String,
    pub ordinal: i32,
    pub low_secs: i64,
    pub low_nanos: i32,
    pub high_secs: i64,
    pub high_nanos: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuantityRow {
    pub param: String,
    pub slot: String,
    pub ordinal: i32,
    pub value: f64,
    pub system: Option<String>,
    pub code: Option<String>,
    pub structured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceRow {
    pub param: String,
    pub slot: String,
    pub ordinal: i32,
    pub ref_full: String,
    pub ref_id: String,
    pub ref_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UriRow {
    pub param: String,
    pub slot: String,
    pub ordinal: i32,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortRow {
    pub param: String,
    pub sort_text: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rows {
    pub tokens: Vec<TokenRow>,
    pub texts: Vec<TextRow>,
    pub numbers: Vec<NumberRow>,
    pub dates: Vec<DateRow>,
    pub quantities: Vec<QuantityRow>,
    pub references: Vec<ReferenceRow>,
    pub uris: Vec<UriRow>,
    pub sorts: Vec<SortRow>,
}

impl Rows {
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
            && self.texts.is_empty()
            && self.numbers.is_empty()
            && self.dates.is_empty()
            && self.quantities.is_empty()
            && self.references.is_empty()
            && self.uris.is_empty()
            && self.sorts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
            + self.texts.len()
            + self.numbers.len()
            + self.dates.len()
            + self.quantities.len()
            + self.references.len()
            + self.uris.len()
    }
}

fn tokens_of(element: &Value, out: &mut Vec<(Option<String>, String)>) {
    match element {
        Value::Array(items) => items.iter().for_each(|item| tokens_of(item, out)),
        Value::String(text) => out.push((None, text.clone())),
        Value::Bool(flag) => out.push((None, flag.to_string())),
        Value::Number(number) => out.push((None, number.to_string())),
        Value::Object(map) => {
            if let Some(codings) = map.get("coding") {
                tokens_of(codings, out);
            }
            let system = map.get("system").and_then(Value::as_str).map(str::to_owned);
            let code = map
                .get("code")
                .and_then(Value::as_str)
                .or_else(|| map.get("value").and_then(Value::as_str));
            if let Some(code) = code {
                out.push((system, code.to_owned()));
            }
        }
        Value::Null => {}
    }
}

fn numbers_of(element: &Value, out: &mut Vec<f64>) {
    match element {
        Value::Array(items) => items.iter().for_each(|item| numbers_of(item, out)),
        Value::Number(number) => out.extend(number.as_f64()),
        Value::String(text) => out.extend(text.parse::<f64>().ok()),
        Value::Object(map) => {
            if let Some(nested) = map.get("value") {
                numbers_of(nested, out);
            }
        }
        Value::Bool(_) | Value::Null => {}
    }
}

fn span(start: Option<&str>, end: Option<&str>) -> Option<InstantPeriod> {
    let low = start.and_then(|text| InstantPeriod::parse(text).ok());
    let high = end.and_then(|text| InstantPeriod::parse(text).ok());
    match (low, high) {
        (Some(low), Some(high)) => InstantPeriod::between(low.low(), high.high()),
        (Some(low), None) => Some(low),
        (None, Some(high)) => Some(high),
        (None, None) => None,
    }
}

fn dates_of(element: &Value, out: &mut Vec<InstantPeriod>) {
    match element {
        Value::Array(items) => items.iter().for_each(|item| dates_of(item, out)),
        Value::String(text) => out.extend(InstantPeriod::parse(text).ok()),
        Value::Object(map) => out.extend(span(
            map.get("start").and_then(Value::as_str),
            map.get("end").and_then(Value::as_str),
        )),
        Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

type Measured = (f64, Option<String>, Option<String>, bool);

fn quantities_of(element: &Value, out: &mut Vec<Measured>) {
    match element {
        Value::Array(items) => items.iter().for_each(|item| quantities_of(item, out)),
        Value::Object(map) => {
            let system = map.get("system").and_then(Value::as_str).map(str::to_owned);
            let code = map
                .get("code")
                .and_then(Value::as_str)
                .or_else(|| map.get("unit").and_then(Value::as_str))
                .map(str::to_owned);
            let mut values = Vec::new();
            if let Some(nested) = map.get("value") {
                numbers_of(nested, &mut values);
            }
            out.extend(
                values
                    .into_iter()
                    .map(|value| (value, system.clone(), code.clone(), true)),
            );
        }
        Value::Number(_) | Value::String(_) => {
            let mut values = Vec::new();
            numbers_of(element, &mut values);
            out.extend(values.into_iter().map(|value| (value, None, None, false)));
        }
        Value::Bool(_) | Value::Null => {}
    }
}

fn references_of(element: &Value, out: &mut Vec<String>) {
    match element {
        Value::Array(items) => items.iter().for_each(|item| references_of(item, out)),
        Value::String(text) => out.push(text.clone()),
        Value::Object(map) => out.extend(
            map.get("reference")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ),
        Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

fn plain_of(element: &Value, out: &mut Vec<String>) {
    match element {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| plain_of(item, out)),
        Value::Object(map) => map.values().for_each(|nested| plain_of(nested, out)),
        Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

fn words_of(element: &Value, out: &mut Vec<String>) {
    match element {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| words_of(item, out)),
        Value::Object(map) => {
            for name in ["coding", "code", "value", "reference", "url"] {
                if let Some(found) = map.get(name) {
                    words_of(found, out);
                }
            }
        }
        Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

fn narratives_of(element: &Value, out: &mut Vec<String>) {
    match element {
        Value::Array(items) => items.iter().for_each(|item| narratives_of(item, out)),
        Value::Object(map) => {
            for name in ["text", "display"] {
                if let Some(Value::String(found)) = map.get(name) {
                    out.push(found.clone());
                }
            }
            if let Some(found) = map.get("coding") {
                narratives_of(found, out);
            }
        }
        Value::String(text) => out.push(text.clone()),
        Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

type Qualified = (String, Vec<(Option<String>, String)>);

fn qualified(element: &Value, out: &mut Vec<Qualified>) {
    match element {
        Value::Array(items) => items.iter().for_each(|item| qualified(item, out)),
        Value::Object(map) => {
            if let Some(value) = map.get("value").and_then(Value::as_str) {
                let mut kinds = Vec::new();
                if let Some(found) = map.get("type") {
                    tokens_of(found, &mut kinds);
                }
                out.push((value.to_owned(), kinds));
            }
        }
        Value::String(_) | Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

fn split_reference(text: &str) -> (String, Option<String>) {
    let mut parts = text.rsplit('/');
    let id = parts.next().unwrap_or_default().to_owned();
    (id, parts.next().map(str::to_owned))
}

struct Sink<'a> {
    rows: &'a mut Rows,
    param: String,
}

impl Sink<'_> {
    fn typed(&mut self, value_type: ValueType, slot: &str, ordinal: i32, element: &Value) {
        let param = self.param.clone();
        match value_type {
            ValueType::Token => {
                let mut found = Vec::new();
                tokens_of(element, &mut found);
                for (system, code) in found {
                    let key = IndexKey::of(&code);
                    self.rows.tokens.push(TokenRow {
                        param: param.clone(),
                        slot: slot.to_owned(),
                        ordinal,
                        system,
                        code: key.key().to_owned(),
                        code_tail: key.overflow().map(str::to_owned),
                    });
                }
            }
            ValueType::String => {
                let mut found = Vec::new();
                plain_of(element, &mut found);
                for value in found {
                    self.rows.texts.push(TextRow {
                        param: param.clone(),
                        slot: slot.to_owned(),
                        ordinal,
                        folded: value.to_lowercase(),
                        value,
                    });
                }
            }
            ValueType::Number => {
                let mut found = Vec::new();
                numbers_of(element, &mut found);
                for value in found {
                    self.rows.numbers.push(NumberRow {
                        param: param.clone(),
                        slot: slot.to_owned(),
                        ordinal,
                        value,
                    });
                }
            }
            ValueType::Date => {
                let mut found = Vec::new();
                dates_of(element, &mut found);
                for period in found {
                    self.rows.dates.push(DateRow {
                        param: param.clone(),
                        slot: slot.to_owned(),
                        ordinal,
                        low_secs: period.low().seconds(),
                        low_nanos: period.low().nanos() as i32,
                        high_secs: period.high().seconds(),
                        high_nanos: period.high().nanos() as i32,
                    });
                }
            }
            ValueType::Quantity => {
                let mut found = Vec::new();
                quantities_of(element, &mut found);
                for (value, system, code, structured) in found {
                    self.rows.quantities.push(QuantityRow {
                        param: param.clone(),
                        slot: slot.to_owned(),
                        ordinal,
                        value,
                        system,
                        code,
                        structured,
                    });
                }
            }
            ValueType::Reference => {
                let mut found = Vec::new();
                references_of(element, &mut found);
                for text in found {
                    let (ref_id, ref_type) = split_reference(&text);
                    self.rows.references.push(ReferenceRow {
                        param: param.clone(),
                        slot: slot.to_owned(),
                        ordinal,
                        ref_full: text,
                        ref_id,
                        ref_type,
                    });
                }
            }
            ValueType::Uri => {
                let mut found = Vec::new();
                references_of(element, &mut found);
                for value in found {
                    self.rows.uris.push(UriRow {
                        param: param.clone(),
                        slot: slot.to_owned(),
                        ordinal,
                        value,
                    });
                }
            }
            ValueType::Composite => {}
        }
    }

    fn projections(&mut self, ordinal: i32, element: &Value) {
        let param = self.param.clone();
        for (slot, gather) in [
            (PLAIN, plain_of as fn(&Value, &mut Vec<String>)),
            (WORDS, words_of),
            (NARRATIVE, narratives_of),
        ] {
            let mut found = Vec::new();
            gather(element, &mut found);
            for value in found {
                self.rows.texts.push(TextRow {
                    param: param.clone(),
                    slot: slot.to_owned(),
                    ordinal,
                    folded: value.to_lowercase(),
                    value,
                });
            }
        }
        if let Some(found) = element.get("identifier") {
            self.typed(ValueType::Token, IDENTIFIER, ordinal, found);
        }
        self.rows.texts.push(TextRow {
            param: param.clone(),
            slot: PRESENCE.to_owned(),
            ordinal,
            value: String::new(),
            folded: String::new(),
        });
    }

    fn qualifiers(&mut self, element: &Value, next: &mut i32) {
        let param = self.param.clone();
        let mut found = Vec::new();
        qualified(element, &mut found);
        for (value, kinds) in found {
            let ordinal = *next;
            *next += 1;
            self.rows.texts.push(TextRow {
                param: param.clone(),
                slot: OF_TYPE.to_owned(),
                ordinal,
                folded: value.to_lowercase(),
                value,
            });
            for (system, code) in kinds {
                let key = IndexKey::of(&code);
                self.rows.tokens.push(TokenRow {
                    param: param.clone(),
                    slot: OF_TYPE.to_owned(),
                    ordinal,
                    system,
                    code: key.key().to_owned(),
                    code_tail: key.overflow().map(str::to_owned),
                });
            }
        }
    }

    fn component(&mut self, sub: &SubDef, slot: &str, ordinal: i32, element: &Value) {
        for path in &sub.paths {
            for found in select(element, path) {
                self.typed(sub.value_type, slot, ordinal, found);
            }
        }
    }
}

pub fn rows_of(envelope: &ResourceEnvelope, body: &Value, defs: &[Arc<ParamDef>]) -> Rows {
    let mut rows = Rows::default();
    for def in defs {
        let mut sink = Sink {
            rows: &mut rows,
            param: def.url.clone().unwrap_or_else(|| def.name.clone()),
        };
        if def.name == "_text" {
            if let Target::Path(paths) = &def.target {
                for element in paths.iter().flat_map(|path| select(body, path)) {
                    let cleaned = fhir_core::search::text::visible(element);
                    if cleaned.is_empty() {
                        continue;
                    }
                    sink.rows.texts.push(TextRow {
                        param: sink.param.clone(),
                        slot: NARRATIVE.to_owned(),
                        ordinal: 0,
                        folded: cleaned.to_lowercase(),
                        value: cleaned,
                    });
                }
            }
            continue;
        }
        match &def.target {
            Target::Id | Target::LastUpdated | Target::Collection => {}
            Target::Path(paths) => {
                let elements: Vec<&Value> =
                    paths.iter().flat_map(|path| select(body, path)).collect();
                let mut qualifier = 0;
                for (ordinal, element) in elements.iter().enumerate() {
                    sink.typed(def.value_type, MAIN, ordinal as i32, element);
                    sink.projections(ordinal as i32, element);
                    sink.qualifiers(element, &mut qualifier);
                }
            }
            Target::Composite(composite) => {
                let elements: Vec<&Value> = composite
                    .base
                    .iter()
                    .flat_map(|path| select(body, path))
                    .collect();
                for (ordinal, element) in elements.iter().enumerate() {
                    sink.component(&composite.left, LEFT, ordinal as i32, element);
                    sink.component(&composite.right, RIGHT, ordinal as i32, element);
                    sink.rows.texts.push(TextRow {
                        param: sink.param.clone(),
                        slot: PRESENCE.to_owned(),
                        ordinal: ordinal as i32,
                        value: String::new(),
                        folded: String::new(),
                    });
                }
            }
        }
        if def.sortable {
            let projected = sort_value(&def.target, envelope.id(), envelope.last_updated(), body);
            rows.sorts.push(SortRow {
                param: def.name.clone(),
                sort_text: match projected {
                    SortValue::Text(text) => Some(text.to_lowercase()),
                    SortValue::Instant(key) => Some(format!(
                        "{:020}.{:09}",
                        key.seconds() + i64::pow(2, 40),
                        key.nanos()
                    )),
                    SortValue::Missing => None,
                },
            });
        }
    }
    rows
}

pub fn reference_of(envelope: &ResourceEnvelope) -> String {
    format!(
        "{}/{}",
        envelope.resource_type().as_str(),
        envelope.id().as_str()
    )
}

pub fn normalized(text: &str) -> String {
    let mut parts = text.rsplit('/');
    let id = parts.next().unwrap_or_default();
    match parts.next() {
        Some(kind) => format!("{kind}/{id}"),
        None => id.to_owned(),
    }
}

pub fn logical(text: &str) -> String {
    text.rsplit('/').next().unwrap_or_default().to_owned()
}

fn flattened(element: &Value, out: &mut Vec<String>) {
    match element {
        Value::String(text) => out.push(text.clone()),
        Value::Number(number) => out.push(number.to_string()),
        Value::Bool(flag) => out.push(flag.to_string()),
        Value::Array(items) => items.iter().for_each(|item| flattened(item, out)),
        Value::Object(_) | Value::Null => {}
    }
}

pub fn parses(spec: &ParameterSpec, body: &Value) -> Result<(), String> {
    for path in spec.def.paths() {
        for element in select(body, &path) {
            let mut found = Vec::new();
            flattened(element, &mut found);
            for text in found {
                fhir_core::search::SearchValue::parse(spec.def.value_type, &text)
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

pub fn declared(rows: &Rows) -> usize {
    rows.tokens.iter().filter(|row| row.slot == MAIN).count()
        + rows.texts.iter().filter(|row| row.slot == MAIN).count()
        + rows.numbers.len()
        + rows.dates.len()
        + rows.quantities.len()
        + rows.references.len()
        + rows.uris.len()
}

pub fn overflowed(rows: &Rows) -> usize {
    rows.tokens
        .iter()
        .filter(|row| row.code_tail.is_some())
        .count()
}
#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::search::lookup;
    use fhir_core::{FhirVersion, ResourceEnvelope};

    fn parsed(body: &str) -> (ResourceEnvelope, Value) {
        let envelope = ResourceEnvelope::parse(FhirVersion::R4, body.as_bytes()).unwrap();
        let value: Value = serde_json::from_str(body).unwrap();
        (envelope, value)
    }

    fn defs(kind: &str, names: &[&str]) -> Vec<Arc<ParamDef>> {
        let resource_type = kind.parse().unwrap();
        names
            .iter()
            .map(|name| lookup(Some(resource_type), name).expect("a built-in parameter"))
            .collect()
    }

    const OBSERVATION: &str = r#"{"resourceType":"Observation","id":"o1","meta":{"versionId":"1","lastUpdated":"2026-09-06T04:00:00Z"},"status":"final","code":{"text":"Mass","coding":[{"system":"urn:s","code":"c1","display":"Coded mass"}]},"valueQuantity":{"value":4.5,"system":"urn:u","code":"mg"},"subject":{"reference":"Patient/p1"}}"#;

    #[test]
    fn a_coded_element_yields_every_coding_and_the_element_itself() {
        let (envelope, body) = parsed(OBSERVATION);
        let rows = rows_of(&envelope, &body, &defs("Observation", &["code"]));
        let codes: Vec<&str> = rows
            .tokens
            .iter()
            .filter(|row| row.slot == MAIN)
            .map(|row| row.code.as_str())
            .collect();
        assert_eq!(codes, vec!["c1"]);
        assert_eq!(rows.tokens[0].system.as_deref(), Some("urn:s"));
    }

    #[test]
    fn the_projections_a_modifier_reads_are_drawn_beside_the_value() {
        let (envelope, body) = parsed(OBSERVATION);
        let rows = rows_of(&envelope, &body, &defs("Observation", &["code"]));
        let folded = |slot: &str| -> Vec<String> {
            rows.texts
                .iter()
                .filter(|row| row.slot == slot)
                .map(|row| row.folded.clone())
                .collect()
        };
        assert!(folded(PLAIN).contains(&"coded mass".to_owned()));
        assert!(folded(WORDS).contains(&"c1".to_owned()));
        assert!(folded(NARRATIVE).contains(&"mass".to_owned()));
        assert!(folded(NARRATIVE).contains(&"coded mass".to_owned()));
    }

    #[test]
    fn a_measured_value_keeps_its_unit_and_its_system() {
        let (envelope, body) = parsed(OBSERVATION);
        let rows = rows_of(&envelope, &body, &defs("Observation", &["value-quantity"]));
        let measured = rows.quantities.first().expect("a measured value");
        assert_eq!(measured.value, 4.5);
        assert_eq!(measured.system.as_deref(), Some("urn:u"));
        assert_eq!(measured.code.as_deref(), Some("mg"));
        assert!(measured.structured);
    }

    #[test]
    fn a_pointer_keeps_the_form_it_was_written_in_and_its_logical_id() {
        let (envelope, body) = parsed(OBSERVATION);
        let rows = rows_of(&envelope, &body, &defs("Observation", &["subject"]));
        let pointer = rows.references.first().expect("a pointer");
        assert_eq!(pointer.ref_full, "Patient/p1");
        assert_eq!(pointer.ref_id, "p1");
        assert_eq!(pointer.ref_type.as_deref(), Some("Patient"));
    }

    #[test]
    fn a_span_of_time_is_drawn_from_a_date_and_from_a_period() {
        let body = r#"{"resourceType":"Patient","id":"p1","meta":{"versionId":"1","lastUpdated":"2026-09-06T04:00:00Z"},"birthDate":"1980-05-06"}"#;
        let (envelope, value) = parsed(body);
        let rows = rows_of(&envelope, &value, &defs("Patient", &["birthdate"]));
        let span = rows.dates.first().expect("a span");
        assert!(span.low_secs < span.high_secs);
        assert_eq!(span.slot, MAIN);
    }

    #[test]
    fn a_long_code_is_split_between_the_key_and_its_overflow() {
        let code = "z".repeat(200);
        let body = format!(
            r#"{{"resourceType":"Patient","id":"p2","meta":{{"versionId":"1","lastUpdated":"2026-09-06T04:00:00Z"}},"identifier":[{{"system":"urn:s","value":"{code}"}}]}}"#
        );
        let (envelope, value) = parsed(&body);
        let rows = rows_of(&envelope, &value, &defs("Patient", &["identifier"]));
        let token = rows
            .tokens
            .iter()
            .find(|row| row.slot == MAIN)
            .expect("an identifier");
        assert_eq!(token.code.chars().count(), 128);
        assert_eq!(
            token.code_tail.as_ref().map(|tail| tail.chars().count()),
            Some(72)
        );
    }

    #[test]
    fn an_ordering_key_is_drawn_only_for_a_sortable_parameter() {
        let (envelope, body) = parsed(OBSERVATION);
        let sortable = defs("Observation", &["_lastUpdated"]);
        let rows = rows_of(&envelope, &body, &sortable);
        assert_eq!(rows.sorts.len(), 1);
        assert!(rows.sorts[0].sort_text.is_some());
        let unsorted = rows_of(&envelope, &body, &defs("Observation", &["subject"]));
        assert!(unsorted.sorts.is_empty());
    }

    #[test]
    fn nothing_is_drawn_from_a_resource_carrying_no_indexed_value() {
        let body = r#"{"resourceType":"Patient","id":"p3","meta":{"versionId":"1","lastUpdated":"2026-09-06T04:00:00Z"}}"#;
        let (envelope, value) = parsed(body);
        let rows = rows_of(&envelope, &value, &defs("Patient", &["name"]));
        assert_eq!(rows.len(), 0);
        assert!(rows.sorts.iter().all(|row| row.sort_text.is_none()));
        assert!(rows_of(&envelope, &value, &defs("Patient", &["identifier"])).is_empty());
    }

    const TEXT_OBSERVATION: &str = r#"{"resourceType":"Observation","id":"o2","meta":{"versionId":"1","lastUpdated":"2026-09-06T04:00:00Z"},"status":"final","text":{"status":"generated","div":"<div><p>The patient had a <b>fever</b> and chills</p></div>"}}"#;

    #[test]
    fn a_text_search_parameter_holds_the_words_of_the_narrative() {
        let (envelope, body) = parsed(TEXT_OBSERVATION);
        let rows = rows_of(&envelope, &body, &defs("Observation", &["_text"]));
        let narrative = rows
            .texts
            .iter()
            .find(|row| row.slot == NARRATIVE)
            .expect("a narrative value");
        assert_eq!(narrative.param, "_text");
        assert_eq!(narrative.folded, "the patient had a fever and chills");
        assert_eq!(narrative.ordinal, 0);
    }

    #[test]
    fn a_resource_without_a_narrative_indexes_no_text() {
        let (envelope, body) = parsed(OBSERVATION);
        let rows = rows_of(&envelope, &body, &defs("Observation", &["_text"]));
        assert!(rows.texts.is_empty());
        assert_eq!(rows.len(), 0);
    }

    #[test]
    fn a_composite_pairs_its_components_on_one_element() {
        let (envelope, body) = parsed(OBSERVATION);
        let rows = rows_of(
            &envelope,
            &body,
            &defs("Observation", &["code-value-quantity"]),
        );
        let left = rows.tokens.iter().find(|row| row.slot == LEFT);
        let right = rows.quantities.iter().find(|row| row.slot == RIGHT);
        assert!(left.is_some(), "{:?}", rows.tokens);
        assert!(right.is_some(), "{:?}", rows.quantities);
        assert_eq!(left.unwrap().ordinal, right.unwrap().ordinal);
    }

    #[test]
    fn a_number_is_drawn_from_every_shape_that_carries_one() {
        let mut found = Vec::new();
        numbers_of(&serde_json::json!(4.5), &mut found);
        numbers_of(&serde_json::json!("6.5"), &mut found);
        numbers_of(&serde_json::json!([1.5, {"value": 2.5}]), &mut found);
        numbers_of(&serde_json::json!({"unit": "mg"}), &mut found);
        numbers_of(&serde_json::json!(true), &mut found);
        numbers_of(&Value::Null, &mut found);
        numbers_of(&serde_json::json!("not a number"), &mut found);
        assert_eq!(found, vec![4.5, 6.5, 1.5, 2.5]);
    }

    #[test]
    fn a_token_is_drawn_from_every_shape_that_carries_one() {
        let mut found = Vec::new();
        tokens_of(&serde_json::json!(7), &mut found);
        tokens_of(&Value::Null, &mut found);
        tokens_of(
            &serde_json::json!({"system": "urn:s", "value": "v1"}),
            &mut found,
        );
        assert_eq!(
            found,
            vec![
                (None, "7".to_owned()),
                (Some("urn:s".to_owned()), "v1".to_owned())
            ]
        );
    }

    #[test]
    fn a_span_is_taken_from_whichever_end_is_given() {
        assert!(span(None, None).is_none());
        assert!(span(Some("2026"), None).is_some());
        assert!(span(None, Some("2026")).is_some());
        assert!(span(Some("2026"), Some("2027")).is_some());
        assert!(span(Some("not a date"), None).is_none());
    }
}
