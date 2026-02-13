
use crate::fhir_version::FhirVersion;
use crate::Error;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

const STU3: &str = include_str!("../definitions/stu3.json");
const R4: &str = include_str!("../definitions/r4.json");
const R4B: &str = include_str!("../definitions/r4b.json");
const R5: &str = include_str!("../definitions/r5.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Structure,
    Cardinality,
    Binding,
    Profile,
    Narrative,
}

impl Rule {
    pub fn as_str(&self) -> &'static str {
        match self {
            Rule::Structure => "structure",
            Rule::Cardinality => "cardinality",
            Rule::Binding => "binding",
            Rule::Profile => "profile",
            Rule::Narrative => "narrative",
        }
    }
}

impl std::fmt::Display for Rule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: Rule,
    pub path: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
struct Element {
    name: String,
    types: Vec<String>,
    min: i64,
    max: i64,
    binding: Option<String>,
    reference: Option<String>,
}

impl Element {
    fn base(&self) -> &str {
        self.name.strip_suffix("[x]").unwrap_or(&self.name)
    }

    fn is_choice(&self) -> bool {
        self.name.ends_with("[x]")
    }
}

#[derive(Debug)]
struct Primitive {
    shape: String,
    pattern: Option<Regex>,
}

#[derive(Debug)]
pub struct Model {
    version: FhirVersion,
    release: String,
    resources: BTreeSet<String>,
    primitives: BTreeMap<String, Primitive>,
    nodes: BTreeMap<String, Vec<Element>>,
    bindings: BTreeMap<String, BTreeSet<String>>,
    profiles: BTreeMap<String, String>,
    unresolved: BTreeSet<String>,
}

impl Model {
    pub fn of(version: FhirVersion) -> &'static Model {
        static HELD: OnceLock<BTreeMap<FhirVersion, Model>> = OnceLock::new();
        let held = HELD.get_or_init(|| {
            FhirVersion::ALL
                .into_iter()
                .map(|version| {
                    let parsed = Model::parse(version, generated(version))
                        .expect("a generated definition file is well formed");
                    (version, parsed)
                })
                .collect()
        });
        held.get(&version).expect("every version has a model")
    }

    pub fn parse(version: FhirVersion, text: &str) -> Result<Model, Error> {
        let held: Value =
            serde_json::from_str(text).map_err(|reason| Error::InvalidJson(reason.to_string()))?;
        let release = held
            .get("release")
            .and_then(Value::as_str)
            .unwrap_or(version.release())
            .to_owned();
        let resources = names(held.get("resources"));
        if resources.is_empty() {
            return Err(Error::Config("the definitions name no resource type".to_owned()));
        }
        let mut primitives = BTreeMap::new();
        if let Some(object) = held.get("primitives").and_then(Value::as_object) {
            for (name, described) in object {
                let pattern = match described.get("pattern").and_then(Value::as_str) {
                    None => None,
                    Some(pattern) => Some(
                        Regex::new(&format!("^(?:{pattern})$"))
                            .map_err(|reason| Error::Config(reason.to_string()))?,
                    ),
                };
                primitives.insert(
                    name.clone(),
                    Primitive {
                        shape: described
                            .get("json")
                            .and_then(Value::as_str)
                            .unwrap_or("text")
                            .to_owned(),
                        pattern,
                    },
                );
            }
        }
        let mut nodes = BTreeMap::new();
        if let Some(object) = held.get("nodes").and_then(Value::as_object) {
            for (path, listed) in object {
                let elements = listed
                    .as_array()
                    .map(|items| items.iter().filter_map(element).collect::<Vec<_>>())
                    .unwrap_or_default();
                nodes.insert(path.clone(), elements);
            }
        }
        let mut bindings = BTreeMap::new();
        if let Some(object) = held.get("bindings").and_then(Value::as_object) {
            for (url, listed) in object {
                bindings.insert(url.clone(), names(Some(listed)));
            }
        }
        let mut profiles = BTreeMap::new();
        if let Some(object) = held.get("profiles").and_then(Value::as_object) {
            for (url, name) in object {
                if let Some(name) = name.as_str() {
                    profiles.insert(url.clone(), name.to_owned());
                }
            }
        }
        Ok(Model {
            version,
            release,
            resources,
            primitives,
            nodes,
            bindings,
            profiles,
            unresolved: names(held.get("unresolved")),
        })
    }

    pub fn version(&self) -> FhirVersion {
        self.version
    }

    pub fn release(&self) -> &str {
        &self.release
    }

    pub fn has_resource(&self, name: &str) -> bool {
        self.resources.contains(name)
    }

    pub fn resources(&self) -> impl Iterator<Item = &str> {
        self.resources.iter().map(String::as_str)
    }

    pub fn profiled(&self, url: &str) -> Option<&str> {
        let base = url.split('|').next().unwrap_or(url);
        self.profiles.get(base).map(String::as_str)
    }

    pub fn bound(&self) -> usize {
        self.bindings.len()
    }

    pub fn unenforced(&self) -> impl Iterator<Item = &str> {
        self.unresolved.iter().map(String::as_str)
    }

    pub fn check(&self, body: &Value) -> Vec<Finding> {
        let mut findings = Vec::new();
        let Some(object) = body.as_object() else {
            findings.push(Finding {
                rule: Rule::Structure,
                path: String::new(),
                detail: "the body is not a resource".to_owned(),
            });
            return findings;
        };
        self.resource(object, &mut findings);
        findings
    }

    fn resource(&self, object: &Map<String, Value>, findings: &mut Vec<Finding>) {
        let held = object.get("resourceType").and_then(Value::as_str);
        let Some(name) = held else {
            findings.push(Finding {
                rule: Rule::Structure,
                path: "resourceType".to_owned(),
                detail: "the body carries no resourceType".to_owned(),
            });
            return;
        };
        if !self.resources.contains(name) {
            findings.push(Finding {
                rule: Rule::Structure,
                path: "resourceType".to_owned(),
                detail: format!("{name} is not a resource type of {}", self.version),
            });
            return;
        }
        self.walk(name, name, object, findings);
    }

    fn walk(
        &self,
        node: &str,
        path: &str,
        object: &Map<String, Value>,
        findings: &mut Vec<Finding>,
    ) {
        let Some(elements) = self.nodes.get(node) else { return };
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for (key, value) in object {
            if key == "resourceType" && self.resources.contains(node) {
                continue;
            }
            let named = key.strip_prefix('_').unwrap_or(key);
            let found = elements.iter().find_map(|element| match element.is_choice() {
                false if element.name == named => Some((element, None)),
                true => chosen(element, named).map(|code| (element, Some(code))),
                false => None,
            });
            let Some((element, code)) = found else {
                findings.push(Finding {
                    rule: Rule::Structure,
                    path: format!("{path}.{key}"),
                    detail: format!("{key} is not an element of {node}"),
                });
                continue;
            };
            seen.insert(element.base());
            if key.starts_with('_') {
                continue;
            }
            let at = format!("{path}.{key}");
            let items = self.spread(element, value, &at, findings);
            let picked = code.or_else(|| element.types.first().cloned());
            for item in items {
                self.value(element, picked.as_deref(), node, &at, item, findings);
            }
        }
        for element in elements {
            if element.min >= 1 && !seen.contains(element.base()) {
                findings.push(Finding {
                    rule: Rule::Cardinality,
                    path: format!("{path}.{}", element.base()),
                    detail: format!("{} is required by {node}", element.base()),
                });
            }
        }
    }

    fn spread<'a>(
        &self,
        element: &Element,
        value: &'a Value,
        at: &str,
        findings: &mut Vec<Finding>,
    ) -> Vec<&'a Value> {
        match (element.max, value) {
            (0, _) => {
                findings.push(Finding {
                    rule: Rule::Cardinality,
                    path: at.to_owned(),
                    detail: format!("{} is not allowed here", element.base()),
                });
                Vec::new()
            }
            (1, Value::Array(_)) => {
                findings.push(Finding {
                    rule: Rule::Cardinality,
                    path: at.to_owned(),
                    detail: format!("{} appears at most once and is a list", element.base()),
                });
                Vec::new()
            }
            (1, held) => vec![held],
            (_, Value::Array(items)) => {
                if element.max > 1 && items.len() as i64 > element.max {
                    findings.push(Finding {
                        rule: Rule::Cardinality,
                        path: at.to_owned(),
                        detail: format!(
                            "{} appears {} times where at most {} are allowed",
                            element.base(),
                            items.len(),
                            element.max
                        ),
                    });
                }
                items.iter().collect()
            }
            (_, _) => {
                findings.push(Finding {
                    rule: Rule::Cardinality,
                    path: at.to_owned(),
                    detail: format!("{} repeats and is not a list", element.base()),
                });
                Vec::new()
            }
        }
    }

    fn value(
        &self,
        element: &Element,
        code: Option<&str>,
        node: &str,
        at: &str,
        item: &Value,
        findings: &mut Vec<Finding>,
    ) {
        if let Some(reference) = &element.reference {
            self.nested(reference, at, item, findings);
            return;
        }
        let Some(code) = code else { return };
        if let Some(primitive) = self.primitives.get(code) {
            self.primitive(element, primitive, at, item, findings);
            return;
        }
        if code == "Resource" || code == "DomainResource" {
            match item.as_object() {
                Some(object) => self.resource(object, findings),
                None => findings.push(Finding {
                    rule: Rule::Structure,
                    path: at.to_owned(),
                    detail: format!("{} is not a resource", element.base()),
                }),
            }
            return;
        }
        let backbone = format!("{node}.{}", element.base());
        if self.nodes.contains_key(&backbone) {
            self.nested(&backbone, at, item, findings);
            return;
        }
        if self.nodes.contains_key(code) {
            self.nested(code, at, item, findings);
            self.coded(element, code, at, item, findings);
        }
    }

    fn nested(&self, node: &str, at: &str, item: &Value, findings: &mut Vec<Finding>) {
        match item.as_object() {
            Some(object) => self.walk(node, at, object, findings),
            None => findings.push(Finding {
                rule: Rule::Structure,
                path: at.to_owned(),
                detail: format!("a {node} is written as an object"),
            }),
        }
    }

    fn primitive(
        &self,
        element: &Element,
        primitive: &Primitive,
        at: &str,
        item: &Value,
        findings: &mut Vec<Finding>,
    ) {
        let matched = match primitive.shape.as_str() {
            "boolean" => item.is_boolean(),
            "number" => item.is_number(),
            _ => item.is_string(),
        };
        if !matched {
            findings.push(Finding {
                rule: Rule::Structure,
                path: at.to_owned(),
                detail: format!("{} is written as a {}", element.base(), primitive.shape),
            });
            return;
        }
        if let (Some(pattern), Some(text)) = (&primitive.pattern, item.as_str()) {
            if !pattern.is_match(text) {
                findings.push(Finding {
                    rule: Rule::Structure,
                    path: at.to_owned(),
                    detail: format!("{text:?} is not a value {} accepts", element.base()),
                });
                return;
            }
        }
        if let (Some(url), Some(text)) = (&element.binding, item.as_str()) {
            self.code(url, text, at, findings);
        }
    }

    fn coded(
        &self,
        element: &Element,
        node: &str,
        at: &str,
        item: &Value,
        findings: &mut Vec<Finding>,
    ) {
        let Some(url) = &element.binding else { return };
        match node {
            "Coding" => {
                if let Some(text) = item.get("code").and_then(Value::as_str) {
                    self.code(url, text, &format!("{at}.code"), findings);
                }
            }
            "CodeableConcept" => {
                let codings = item.get("coding").and_then(Value::as_array);
                for coding in codings.map(Vec::as_slice).unwrap_or(&[]) {
                    if let Some(text) = coding.get("code").and_then(Value::as_str) {
                        self.code(url, text, &format!("{at}.coding.code"), findings);
                    }
                }
            }
            _ => {}
        }
    }

    fn code(&self, url: &str, text: &str, at: &str, findings: &mut Vec<Finding>) {
        let Some(codes) = self.bindings.get(url) else { return };
        if !codes.contains(text) {
            findings.push(Finding {
                rule: Rule::Binding,
                path: at.to_owned(),
                detail: format!("{text:?} is not a code of {url}"),
            });
        }
    }
}

fn generated(version: FhirVersion) -> &'static str {
    match version {
        FhirVersion::Stu3 => STU3,
        FhirVersion::R4 => R4,
        FhirVersion::R4b => R4B,
        FhirVersion::R5 => R5,
    }
}

fn chosen(element: &Element, named: &str) -> Option<String> {
    let base = element.base();
    let rest = named.strip_prefix(base)?;
    element
        .types
        .iter()
        .find(|code| capitalized(code) == rest)
        .cloned()
}

fn capitalized(code: &str) -> String {
    let mut characters = code.chars();
    match characters.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
    }
}

fn element(held: &Value) -> Option<Element> {
    let name = held.get("n").and_then(Value::as_str)?.to_owned();
    Some(Element {
        name,
        types: held
            .get("t")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default(),
        min: held.get("min").and_then(Value::as_i64).unwrap_or(0),
        max: held.get("max").and_then(Value::as_i64).unwrap_or(-1),
        binding: held.get("b").and_then(Value::as_str).map(str::to_owned),
        reference: held.get("r").and_then(Value::as_str).map(str::to_owned),
    })
}

fn names(held: Option<&Value>) -> BTreeSet<String> {
    held.and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFINITIONS: &str = r#"{
      "version":"R4","release":"4.0.1",
      "resources":["Patient","Bundle"],
      "profiles":{"http://example.test/StructureDefinition/Patient":"Patient"},
      "primitives":{
        "boolean":{"json":"boolean"},
        "string":{"json":"text"},
        "integer":{"json":"number"},
        "code":{"json":"text","pattern":"[^\\s]+(\\s[^\\s]+)*"},
        "id":{"json":"text","pattern":"[A-Za-z0-9\\-\\.]{1,64}"}},
      "nodes":{
        "Patient":[
          {"n":"id","t":["id"],"min":0,"max":1},
          {"n":"active","t":["boolean"],"min":0,"max":1},
          {"n":"gender","t":["code"],"min":0,"max":1,"b":"http://example.test/ValueSet/gender"},
          {"n":"status","t":["code"],"min":1,"max":1},
          {"n":"name","t":["HumanName"],"min":0,"max":-1},
          {"n":"contact","t":["BackboneElement"],"min":0,"max":-1},
          {"n":"deceased[x]","t":["boolean","dateTime"],"min":0,"max":1},
          {"n":"contained","t":["Resource"],"min":0,"max":-1},
          {"n":"link","t":["BackboneElement"],"min":0,"max":2}],
        "Patient.contact":[{"n":"name","t":["HumanName"],"min":0,"max":1}],
        "Patient.link":[{"n":"other","t":["string"],"min":0,"max":1}],
        "Bundle":[{"n":"entry","t":["BackboneElement"],"min":0,"max":-1}],
        "Bundle.entry":[{"n":"resource","t":["Resource"],"min":0,"max":1}],
        "HumanName":[
          {"n":"family","t":["string"],"min":0,"max":1},
          {"n":"given","t":["string"],"min":0,"max":-1}]},
      "bindings":{"http://example.test/ValueSet/gender":["male","female"]},
      "unresolved":["http://example.test/ValueSet/open"]
    }"#;

    fn model() -> Model {
        Model::parse(FhirVersion::R4, DEFINITIONS).expect("the definitions parse")
    }

    fn found(body: Value) -> Vec<Finding> {
        model().check(&body)
    }

    fn valid() -> Value {
        serde_json::json!({"resourceType": "Patient", "status": "final"})
    }

    #[test]
    fn a_body_matching_its_type_is_accepted() {
        let body = serde_json::json!({
            "resourceType": "Patient",
            "status": "final",
            "active": true,
            "gender": "male",
            "name": [{"family": "Stone", "given": ["Ada", "May"]}],
            "contact": [{"name": {"family": "Stone"}}],
            "deceasedBoolean": false
        });
        assert_eq!(found(body), Vec::new());
    }

    #[test]
    fn an_element_the_type_does_not_define_is_reported() {
        let mut body = valid();
        body["nonesuch"] = serde_json::json!(1);
        let findings = found(body);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, Rule::Structure);
        assert_eq!(findings[0].path, "Patient.nonesuch");
    }

    #[test]
    fn a_nested_element_the_type_does_not_define_is_reported() {
        let mut body = valid();
        body["name"] = serde_json::json!([{"surname": "Stone"}]);
        let findings = found(body);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, "Patient.name.surname");
    }

    #[test]
    fn a_missing_required_element_is_reported_as_cardinality() {
        let findings = found(serde_json::json!({"resourceType": "Patient"}));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, Rule::Cardinality);
        assert_eq!(findings[0].path, "Patient.status");
    }

    #[test]
    fn a_single_element_written_as_a_list_is_reported() {
        let mut body = valid();
        body["active"] = serde_json::json!([true]);
        let findings = found(body);
        assert_eq!(findings[0].rule, Rule::Cardinality);
    }

    #[test]
    fn a_repeating_element_written_alone_is_reported() {
        let mut body = valid();
        body["name"] = serde_json::json!({"family": "Stone"});
        let findings = found(body);
        assert_eq!(findings[0].rule, Rule::Cardinality);
    }

    #[test]
    fn more_repeats_than_the_definitions_allow_are_reported() {
        let mut body = valid();
        body["link"] = serde_json::json!([{}, {}, {}]);
        let findings = found(body);
        assert_eq!(findings[0].rule, Rule::Cardinality);
        assert!(findings[0].detail.contains("3 times"));
    }

    #[test]
    fn a_primitive_of_the_wrong_shape_is_reported() {
        let mut body = valid();
        body["active"] = serde_json::json!("yes");
        let findings = found(body);
        assert_eq!(findings[0].rule, Rule::Structure);
        assert!(findings[0].detail.contains("boolean"));
    }

    #[test]
    fn a_primitive_outside_its_pattern_is_reported() {
        let mut body = valid();
        body["id"] = serde_json::json!("not a valid id");
        let findings = found(body);
        assert_eq!(findings[0].rule, Rule::Structure);
        assert_eq!(findings[0].path, "Patient.id");
    }

    #[test]
    fn a_code_outside_a_bound_value_set_is_reported() {
        let mut body = valid();
        body["gender"] = serde_json::json!("mail");
        let findings = found(body);
        assert_eq!(findings[0].rule, Rule::Binding);
        assert!(findings[0].detail.contains("gender"));
    }

    #[test]
    fn a_choice_element_takes_only_a_defined_type() {
        let mut body = valid();
        body["deceasedQuantity"] = serde_json::json!(1);
        let findings = found(body);
        assert_eq!(findings[0].rule, Rule::Structure);
    }

    #[test]
    fn a_primitive_extension_is_accepted_beside_its_element() {
        let mut body = valid();
        body["_active"] = serde_json::json!({"id": "a1"});
        assert_eq!(found(body), Vec::new());
    }

    #[test]
    fn a_contained_resource_is_checked_as_a_resource() {
        let mut body = valid();
        body["contained"] = serde_json::json!([{"resourceType": "Patient", "nonesuch": 1}]);
        let findings = found(body);
        assert!(findings.iter().any(|finding| finding.path.contains("nonesuch")));
    }

    #[test]
    fn a_body_naming_no_type_or_an_unknown_type_is_reported() {
        let findings = found(serde_json::json!({"active": true}));
        assert_eq!(findings[0].path, "resourceType");
        let findings = found(serde_json::json!({"resourceType": "Nonesuch"}));
        assert_eq!(findings[0].rule, Rule::Structure);
        assert!(findings[0].detail.contains("R4"));
        let findings = found(serde_json::json!("text"));
        assert_eq!(findings[0].rule, Rule::Structure);
    }

    #[test]
    fn the_model_reports_what_it_holds() {
        let model = model();
        assert!(model.has_resource("Patient"));
        assert!(!model.has_resource("Nonesuch"));
        assert_eq!(model.version(), FhirVersion::R4);
        assert_eq!(model.release(), "4.0.1");
        assert_eq!(model.resources().count(), 2);
        assert_eq!(model.bound(), 1);
        assert_eq!(model.unenforced().count(), 1);
    }

    #[test]
    fn definitions_naming_no_resource_are_refused() {
        assert!(Model::parse(FhirVersion::R4, r#"{"resources":[]}"#).is_err());
        assert!(Model::parse(FhirVersion::R4, "{").is_err());
    }

    #[test]
    fn every_rule_is_named() {
        for rule in [
            Rule::Structure,
            Rule::Cardinality,
            Rule::Binding,
            Rule::Profile,
            Rule::Narrative,
        ] {
            assert!(!rule.to_string().is_empty());
        }
    }
}
