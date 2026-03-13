use crate::search::select;
use crate::Error;
use serde_json::{Map, Value};
use std::str::FromStr;

pub const DEFAULT_COLLECTION: &str = "urn:template-collection:default";

const CLINICAL_DOCUMENT: &str = "ClinicalDocument";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputType {
    Hl7v2,
    Ccda,
    Json,
    Fhir,
}

impl FromStr for InputType {
    type Err = Error;

    fn from_str(text: &str) -> Result<InputType, Error> {
        match text {
            "hl7v2" => Ok(InputType::Hl7v2),
            "ccda" => Ok(InputType::Ccda),
            "json" => Ok(InputType::Json),
            "fhir" => Ok(InputType::Fhir),
            other => Err(Error::UnsupportedParameter(format!(
                "input data type {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TemplateCollection {
    reference: String,
    templates: Vec<(String, Value)>,
}

impl TemplateCollection {
    pub fn new(reference: &str) -> TemplateCollection {
        TemplateCollection {
            reference: reference.to_owned(),
            templates: Vec::new(),
        }
    }

    pub fn with(mut self, name: &str, template: Value) -> TemplateCollection {
        self.templates.push((name.to_owned(), template));
        self
    }

    pub fn reference(&self) -> &str {
        &self.reference
    }

    pub fn template(&self, name: &str) -> Option<&Value> {
        self.templates
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, template)| template)
    }

    pub fn names(&self) -> Vec<&str> {
        self.templates
            .iter()
            .map(|(name, _)| name.as_str())
            .collect()
    }
}

pub trait Templates: Send + Sync {
    fn collection(&self, reference: &str) -> Option<&TemplateCollection>;

    fn approved(&self) -> Vec<&str>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApprovedTemplates {
    collections: Vec<TemplateCollection>,
}

impl ApprovedTemplates {
    pub fn new(collections: Vec<TemplateCollection>) -> ApprovedTemplates {
        ApprovedTemplates { collections }
    }
}

impl Default for ApprovedTemplates {
    fn default() -> ApprovedTemplates {
        ApprovedTemplates::new(vec![TemplateCollection::new(DEFAULT_COLLECTION)
            .with(
                "Patient",
                serde_json::json!({
                    "resourceType": "Patient",
                    "id": "{{PID.3}}",
                    "name": [{"family": "{{PID.5.1}}", "given": ["{{PID.5.2}}"]}],
                    "birthDate": "{{date(PID.7)}}",
                    "gender": "{{PID.8}}"
                }),
            )
            .with(
                "Observation",
                serde_json::json!({
                    "resourceType": "Observation",
                    "status": "final",
                    "code": {"coding": [{"system": "{{OBX.3.3}}", "code": "{{OBX.3.1}}"}]},
                    "valueString": "{{OBX.5}}"
                }),
            )
            .with(
                "ClinicalDocument",
                serde_json::json!({
                    "resourceType": "Patient",
                    "identifier": [{
                        "system": "{{ClinicalDocument.recordTarget.patientRole.id.root}}",
                        "value": "{{ClinicalDocument.recordTarget.patientRole.id.extension}}"
                    }],
                    "name": [{
                        "family": "{{ClinicalDocument.recordTarget.patientRole.patient.name.family}}",
                        "given": ["{{ClinicalDocument.recordTarget.patientRole.patient.name.given}}"]
                    }],
                    "gender": "{{ClinicalDocument.recordTarget.patientRole.patient.administrativeGenderCode.code}}",
                    "birthDate": "{{date(ClinicalDocument.recordTarget.patientRole.patient.birthTime.value)}}"
                }),
            )])
    }
}

impl Templates for ApprovedTemplates {
    fn collection(&self, reference: &str) -> Option<&TemplateCollection> {
        self.collections
            .iter()
            .find(|held| held.reference == reference)
    }

    fn approved(&self) -> Vec<&str> {
        self.collections
            .iter()
            .map(|held| held.reference.as_str())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion<'a> {
    pub input_type: InputType,
    pub data: &'a str,
    pub collection: &'a str,
    pub root_template: &'a str,
}

pub fn convert(templates: &dyn Templates, request: &Conversion) -> Result<Value, Error> {
    let collection = templates.collection(request.collection).ok_or_else(|| {
        Error::Forbidden(format!(
            "template collection {:?} is not approved",
            request.collection
        ))
    })?;
    let template = collection.template(request.root_template).ok_or_else(|| {
        Error::InvalidParameter(format!("template {:?} is unknown", request.root_template))
    })?;
    let source = source(request.input_type, request.data)?;
    let rendered = render(template, &source);
    let object = rendered
        .as_object()
        .filter(|object| object.get("resourceType").and_then(Value::as_str).is_some());
    match object {
        Some(_) => Ok(rendered),
        None => Err(Error::InvalidEnvelope(
            "the template rendered no resource".to_owned(),
        )),
    }
}

fn source(input_type: InputType, data: &str) -> Result<Value, Error> {
    match input_type {
        InputType::Hl7v2 => Ok(delimited(data)),
        InputType::Ccda => {
            let value = crate::xml::tree(data)?;
            match value.get(CLINICAL_DOCUMENT) {
                Some(_) => Ok(value),
                None => Err(Error::InvalidEnvelope(format!(
                    "the submitted document is no C-CDA: its root is not {CLINICAL_DOCUMENT}"
                ))),
            }
        }
        InputType::Json | InputType::Fhir => {
            let value: Value = serde_json::from_str(data)
                .map_err(|error| Error::InvalidJson(error.to_string()))?;
            if !value.is_object() {
                return Err(Error::InvalidEnvelope(
                    "the submitted data is not an object".to_owned(),
                ));
            }
            if input_type == InputType::Fhir
                && value.get("resourceType").and_then(Value::as_str).is_none()
            {
                return Err(Error::InvalidEnvelope(
                    "the submitted resource has no type".to_owned(),
                ));
            }
            Ok(value)
        }
    }
}

fn delimited(data: &str) -> Value {
    let mut root = Map::new();
    for line in data
        .split(['\r', '\n'])
        .filter(|line| !line.trim().is_empty())
    {
        let mut fields = line.split('|');
        let name = fields.next().unwrap_or_default().trim();
        if name.is_empty() {
            continue;
        }
        let mut segment = Map::new();
        for (index, field) in fields.enumerate() {
            segment.insert((index + 1).to_string(), field_value(field));
        }
        let held = Value::Object(segment);
        match root.remove(name) {
            None => {
                root.insert(name.to_owned(), held);
            }
            Some(Value::Array(mut items)) => {
                items.push(held);
                root.insert(name.to_owned(), Value::Array(items));
            }
            Some(first) => {
                root.insert(name.to_owned(), Value::Array(vec![first, held]));
            }
        }
    }
    Value::Object(root)
}

fn field_value(field: &str) -> Value {
    if !field.contains('^') {
        return Value::String(field.to_owned());
    }
    let mut components = Map::new();
    for (index, component) in field.split('^').enumerate() {
        components.insert((index + 1).to_string(), Value::String(component.to_owned()));
    }
    Value::Object(components)
}

fn render(template: &Value, source: &Value) -> Value {
    match template {
        Value::String(text) => substituted(text, source),
        Value::Array(items) => {
            let kept: Vec<Value> = items
                .iter()
                .map(|item| render(item, source))
                .filter(|item| !item.is_null())
                .collect();
            match kept.is_empty() {
                true => Value::Null,
                false => Value::Array(kept),
            }
        }
        Value::Object(map) => {
            let mut kept = Map::new();
            for (name, held) in map {
                let rendered = render(held, source);
                if !rendered.is_null() {
                    kept.insert(name.clone(), rendered);
                }
            }
            match kept.is_empty() {
                true => Value::Null,
                false => Value::Object(kept),
            }
        }
        other => other.clone(),
    }
}

fn substituted(text: &str, source: &Value) -> Value {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(end) = rest[start..].find("}}") else {
            break;
        };
        let path = &rest[start + 2..start + end];
        let Some(found) = bound(source, path.trim()) else {
            return Value::Null;
        };
        out.push_str(&rest[..start]);
        out.push_str(&found);
        rest = &rest[start + end + 2..];
    }
    out.push_str(rest);
    match out.is_empty() {
        true => Value::Null,
        false => Value::String(out),
    }
}

fn bound(source: &Value, path: &str) -> Option<String> {
    if let Some(inner) = path
        .strip_prefix("date(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return dated(&bound(source, inner.trim())?);
    }
    if let Some(found) = scalar(source, path) {
        return Some(found);
    }
    path.strip_suffix(".1")
        .and_then(|field| scalar(source, field))
}

fn scalar(source: &Value, path: &str) -> Option<String> {
    select(source, path)
        .into_iter()
        .find_map(|found| match found {
            Value::String(text) if !text.is_empty() => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        })
}

fn dated(field: &str) -> Option<String> {
    let digits: &str = field.split(['-', '+']).next().unwrap_or_default();
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    match digits.len() {
        4 => Some(digits.to_owned()),
        6 => Some(format!("{}-{}", &digits[..4], &digits[4..6])),
        length if length >= 8 => Some(format!(
            "{}-{}-{}",
            &digits[..4],
            &digits[4..6],
            &digits[6..8]
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCUMENT: &str = r#"<?xml version="1.0"?>
<ClinicalDocument xmlns="urn:hl7-org:v3">
  <recordTarget>
    <patientRole>
      <id root="2.16.840.1.113883.19.5" extension="pt-77"/>
      <patient>
        <name><given>Ada</given><family>Stone</family></name>
        <administrativeGenderCode code="female"/>
        <birthTime value="19800506"/>
      </patient>
    </patientRole>
  </recordTarget>
</ClinicalDocument>"#;

    #[test]
    fn a_ccda_document_is_read_as_a_tree_a_template_selects_over() {
        let held = source(InputType::Ccda, DOCUMENT).unwrap();
        assert_eq!(
            held["ClinicalDocument"]["recordTarget"]["patientRole"]["id"]["extension"],
            "pt-77"
        );
        assert_eq!(
            held["ClinicalDocument"]["recordTarget"]["patientRole"]["patient"]["name"]["family"],
            "Stone"
        );
    }

    #[test]
    fn a_ccda_document_converts_element_by_element() {
        let templates = ApprovedTemplates::default();
        let rendered = convert(
            &templates,
            &Conversion {
                input_type: InputType::Ccda,
                data: DOCUMENT,
                collection: DEFAULT_COLLECTION,
                root_template: "ClinicalDocument",
            },
        )
        .unwrap();
        assert_eq!(rendered["resourceType"], "Patient");
        assert_eq!(rendered["identifier"][0]["value"], "pt-77");
        assert_eq!(
            rendered["identifier"][0]["system"],
            "2.16.840.1.113883.19.5"
        );
        assert_eq!(rendered["name"][0]["family"], "Stone");
        assert_eq!(rendered["name"][0]["given"][0], "Ada");
        assert_eq!(rendered["gender"], "female");
        assert_eq!(rendered["birthDate"], "1980-05-06");
    }

    #[test]
    fn a_body_that_is_no_ccda_document_is_refused() {
        let error = source(InputType::Ccda, "<Bundle><entry/></Bundle>").unwrap_err();
        assert!(error.to_string().contains("ClinicalDocument"), "{error}");
    }

    #[test]
    fn a_body_that_is_not_xml_at_all_is_refused() {
        let error = source(InputType::Ccda, "MSH|^~\\&|").unwrap_err();
        assert!(matches!(error, Error::InvalidXml(_)), "{error:?}");
    }

    #[test]
    fn a_root_template_the_collection_does_not_hold_is_refused() {
        let templates = ApprovedTemplates::default();
        let error = convert(
            &templates,
            &Conversion {
                input_type: InputType::Ccda,
                data: DOCUMENT,
                collection: DEFAULT_COLLECTION,
                root_template: "Nonesuch",
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("Nonesuch"), "{error}");
    }

    #[test]
    fn the_four_input_types_the_reference_registers_are_read() {
        for (spelling, expected) in [
            ("hl7v2", InputType::Hl7v2),
            ("ccda", InputType::Ccda),
            ("json", InputType::Json),
            ("fhir", InputType::Fhir),
        ] {
            assert_eq!(spelling.parse::<InputType>().unwrap(), expected);
        }
        assert!("xml".parse::<InputType>().is_err());
    }

    #[test]
    fn a_repeated_segment_becomes_a_list() {
        let value = delimited("OBX|1|a\rOBX|2|b\rOBX|3|c");
        assert_eq!(value["OBX"].as_array().map(Vec::len), Some(3));
        assert_eq!(value["OBX"][1]["2"], "b");
    }

    #[test]
    fn a_registry_reports_what_it_approves() {
        let templates = ApprovedTemplates::default();
        assert_eq!(templates.approved(), vec![DEFAULT_COLLECTION]);
        let collection = templates.collection(DEFAULT_COLLECTION).unwrap();
        assert_eq!(collection.reference(), DEFAULT_COLLECTION);
        assert!(collection.names().contains(&"Observation"));
    }

    #[test]
    fn a_template_rendering_nothing_is_an_error() {
        let templates =
            ApprovedTemplates::new(vec![TemplateCollection::new("urn:c")
                .with("Empty", serde_json::json!({"id": "{{X.1}}"}))]);
        let error = convert(
            &templates,
            &Conversion {
                input_type: InputType::Json,
                data: "{}",
                collection: "urn:c",
                root_template: "Empty",
            },
        )
        .unwrap_err();
        assert!(matches!(error, Error::InvalidEnvelope(_)));
    }

    #[test]
    fn a_placeholder_is_substituted_inside_a_longer_string() {
        let source = serde_json::json!({"a": {"b": "x"}, "n": 2, "f": true});
        assert_eq!(
            substituted("urn:{{a.b}}:{{n}}:{{f}}", &source),
            Value::String("urn:x:2:true".to_owned())
        );
        assert_eq!(
            substituted("{{a.b", &source),
            Value::String("{{a.b".to_owned())
        );
    }
}
