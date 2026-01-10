use crate::search::modifier::{value_of, Modifier};
use crate::search::value::{SearchValue, ValueType};
use crate::{Error, ResourceType};
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubDef {
    pub value_type: ValueType,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeDef {
    pub base: Vec<String>,
    pub left: SubDef,
    pub right: SubDef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Id,
    LastUpdated,
    Path(Vec<String>),
    Composite(Box<CompositeDef>),
}

impl Target {
    pub fn path<I, S>(paths: I) -> Target
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Target::Path(paths.into_iter().map(|path| path.as_ref().to_owned()).collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamStatus {
    Supported,
    Searchable,
    PendingDisable,
    Disabled,
}

impl ParamStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ParamStatus::Supported => "supported",
            ParamStatus::Searchable => "searchable",
            ParamStatus::PendingDisable => "pending-disable",
            ParamStatus::Disabled => "disabled",
        }
    }

    pub fn is_searchable(&self) -> bool {
        matches!(self, ParamStatus::Searchable)
    }
}

impl std::str::FromStr for ParamStatus {
    type Err = Error;

    fn from_str(text: &str) -> Result<ParamStatus, Error> {
        match text {
            "supported" => Ok(ParamStatus::Supported),
            "searchable" => Ok(ParamStatus::Searchable),
            "pending-disable" => Ok(ParamStatus::PendingDisable),
            "disabled" => Ok(ParamStatus::Disabled),
            other => Err(Error::UnsupportedParameter(format!("status {other:?}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamDef {
    pub name: String,
    pub value_type: ValueType,
    pub target: Target,
    pub targets: Vec<String>,
    pub sortable: bool,
    pub url: Option<String>,
}

impl ParamDef {
    pub fn value(&self, raw: &str) -> Result<SearchValue, Error> {
        match &self.target {
            Target::Composite(def) => {
                let (left, right) = raw.split_once('$').ok_or_else(|| {
                    Error::InvalidParameter(format!("composite {raw:?} needs two components"))
                })?;
                Ok(SearchValue::composite(
                    SearchValue::parse(def.left.value_type, left)?,
                    SearchValue::parse(def.right.value_type, right)?,
                ))
            }
            Target::Id | Target::LastUpdated | Target::Path(_) => {
                SearchValue::parse(self.value_type, raw)
            }
        }
    }

    pub fn value_with(&self, modifier: &Modifier, raw: &str) -> Result<SearchValue, Error> {
        match modifier {
            Modifier::None => self.value(raw),
            other => value_of(other, self.value_type, raw),
        }
    }

    pub fn paths(&self) -> Vec<String> {
        match &self.target {
            Target::Path(paths) => paths.clone(),
            Target::Id | Target::LastUpdated | Target::Composite(_) => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct StaticSub {
    value_type: ValueType,
    paths: &'static [&'static str],
}

#[derive(Debug, Clone, Copy)]
struct StaticComposite {
    base: &'static [&'static str],
    left: StaticSub,
    right: StaticSub,
}

#[derive(Debug, Clone, Copy)]
enum StaticTarget {
    Id,
    LastUpdated,
    Path(&'static [&'static str]),
    Composite(&'static StaticComposite),
}

#[derive(Debug, Clone, Copy)]
struct StaticDef {
    name: &'static str,
    value_type: ValueType,
    target: StaticTarget,
    targets: &'static [&'static str],
    sortable: bool,
}

fn owned(paths: &'static [&'static str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

fn sub(def: StaticSub) -> SubDef {
    SubDef {
        value_type: def.value_type,
        paths: owned(def.paths),
    }
}

impl From<&StaticDef> for ParamDef {
    fn from(def: &StaticDef) -> ParamDef {
        ParamDef {
            name: def.name.to_owned(),
            value_type: def.value_type,
            target: match def.target {
                StaticTarget::Id => Target::Id,
                StaticTarget::LastUpdated => Target::LastUpdated,
                StaticTarget::Path(paths) => Target::Path(owned(paths)),
                StaticTarget::Composite(composite) => Target::Composite(Box::new(CompositeDef {
                    base: owned(composite.base),
                    left: sub(composite.left),
                    right: sub(composite.right),
                })),
            },
            targets: owned(def.targets),
            sortable: def.sortable,
            url: None,
        }
    }
}

const COMMON: &[StaticDef] = &[
    StaticDef {
        name: "_id",
        value_type: ValueType::Token,
        target: StaticTarget::Id,
        targets: &[],
        sortable: true,
    },
    StaticDef {
        name: "_lastUpdated",
        value_type: ValueType::Date,
        target: StaticTarget::LastUpdated,
        targets: &[],
        sortable: true,
    },
    StaticDef {
        name: "_profile",
        value_type: ValueType::Uri,
        target: StaticTarget::Path(&["meta.profile"]),
        targets: &[],
        sortable: false,
    },
    StaticDef {
        name: "_tag",
        value_type: ValueType::Token,
        target: StaticTarget::Path(&["meta.tag"]),
        targets: &[],
        sortable: false,
    },
    StaticDef {
        name: "_security",
        value_type: ValueType::Token,
        target: StaticTarget::Path(&["meta.security"]),
        targets: &[],
        sortable: false,
    },
];

const fn def(name: &'static str, value_type: ValueType, paths: &'static [&'static str]) -> StaticDef {
    StaticDef {
        name,
        value_type,
        target: StaticTarget::Path(paths),
        targets: &[],
        sortable: false,
    }
}

const fn refers(
    name: &'static str,
    paths: &'static [&'static str],
    targets: &'static [&'static str],
) -> StaticDef {
    StaticDef {
        name,
        value_type: ValueType::Reference,
        target: StaticTarget::Path(paths),
        targets,
        sortable: false,
    }
}

const fn sorted(
    name: &'static str,
    value_type: ValueType,
    paths: &'static [&'static str],
) -> StaticDef {
    StaticDef {
        name,
        value_type,
        target: StaticTarget::Path(paths),
        targets: &[],
        sortable: true,
    }
}

const CODE_VALUE_QUANTITY: StaticComposite = StaticComposite {
    base: &[""],
    left: StaticSub {
        value_type: ValueType::Token,
        paths: &["code"],
    },
    right: StaticSub {
        value_type: ValueType::Quantity,
        paths: &["valueQuantity"],
    },
};

const COMPONENT_CODE_VALUE_QUANTITY: StaticComposite = StaticComposite {
    base: &["component"],
    left: StaticSub {
        value_type: ValueType::Token,
        paths: &["code"],
    },
    right: StaticSub {
        value_type: ValueType::Quantity,
        paths: &["valueQuantity"],
    },
};

const PATIENT: &[StaticDef] = &[
    def("active", ValueType::Token, &["active"]),
    def("address", ValueType::String, &["address"]),
    sorted("birthdate", ValueType::Date, &["birthDate"]),
    def("death-date", ValueType::Date, &["deceasedDateTime"]),
    sorted("family", ValueType::String, &["name.family"]),
    sorted("gender", ValueType::Token, &["gender"]),
    refers("general-practitioner", &["generalPractitioner"], &["Practitioner", "Organization"]),
    def("given", ValueType::String, &["name.given"]),
    def("identifier", ValueType::Token, &["identifier"]),
    sorted("name", ValueType::String, &["name"]),
    refers("organization", &["managingOrganization"], &["Organization"]),
    def("telecom", ValueType::Token, &["telecom"]),
];

const OBSERVATION: &[StaticDef] = &[
    def("category", ValueType::Token, &["category"]),
    def("code", ValueType::Token, &["code"]),
    StaticDef {
        name: "code-value-quantity",
        value_type: ValueType::Composite,
        target: StaticTarget::Composite(&CODE_VALUE_QUANTITY),
        targets: &[],
        sortable: false,
    },
    def("component-code", ValueType::Token, &["component.code"]),
    StaticDef {
        name: "component-code-value-quantity",
        value_type: ValueType::Composite,
        target: StaticTarget::Composite(&COMPONENT_CODE_VALUE_QUANTITY),
        targets: &[],
        sortable: false,
    },
    def("component-value-quantity", ValueType::Quantity, &["component.valueQuantity"]),
    sorted("date", ValueType::Date, &["effectiveDateTime", "effectivePeriod"]),
    refers("encounter", &["encounter"], &["Encounter"]),
    def("identifier", ValueType::Token, &["identifier"]),
    refers("patient", &["subject"], &["Patient"]),
    sorted("status", ValueType::Token, &["status"]),
    refers("subject", &["subject"], &["Patient", "Group"]),
    def("value-quantity", ValueType::Quantity, &["valueQuantity"]),
    def("value-string", ValueType::String, &["valueString"]),
];

const ENCOUNTER: &[StaticDef] = &[
    def("class", ValueType::Token, &["class"]),
    sorted("date", ValueType::Date, &["period"]),
    def("identifier", ValueType::Token, &["identifier"]),
    refers("patient", &["subject"], &["Patient"]),
    refers("service-provider", &["serviceProvider"], &["Organization"]),
    sorted("status", ValueType::Token, &["status"]),
    refers("subject", &["subject"], &["Patient", "Group"]),
];

const LIST: &[StaticDef] = &[
    sorted("date", ValueType::Date, &["date"]),
    def("identifier", ValueType::Token, &["identifier"]),
    refers("item", &["entry.item"], &[]),
    refers("patient", &["subject"], &["Patient"]),
    sorted("status", ValueType::Token, &["status"]),
    refers("subject", &["subject"], &["Patient", "Group"]),
    sorted("title", ValueType::String, &["title"]),
];

const ORGANIZATION: &[StaticDef] = &[
    def("active", ValueType::Token, &["active"]),
    def("address", ValueType::String, &["address"]),
    def("identifier", ValueType::Token, &["identifier"]),
    sorted("name", ValueType::String, &["name"]),
];

const PRACTITIONER: &[StaticDef] = &[
    def("active", ValueType::Token, &["active"]),
    def("family", ValueType::String, &["name.family"]),
    def("given", ValueType::String, &["name.given"]),
    def("identifier", ValueType::Token, &["identifier"]),
    sorted("name", ValueType::String, &["name"]),
];

const RISK_ASSESSMENT: &[StaticDef] = &[
    def("identifier", ValueType::Token, &["identifier"]),
    refers("patient", &["subject"], &["Patient"]),
    def("probability", ValueType::Number, &["prediction.probabilityDecimal"]),
    refers("subject", &["subject"], &["Patient", "Group"]),
];

const VALUE_SET: &[StaticDef] = &[
    def("identifier", ValueType::Token, &["identifier"]),
    sorted("name", ValueType::String, &["name"]),
    sorted("status", ValueType::Token, &["status"]),
    def("url", ValueType::Uri, &["url"]),
    def("version", ValueType::Token, &["version"]),
];

fn per_type(resource_type: ResourceType) -> &'static [StaticDef] {
    match resource_type.as_str() {
        "Patient" => PATIENT,
        "Observation" => OBSERVATION,
        "Encounter" => ENCOUNTER,
        "List" => LIST,
        "Organization" => ORGANIZATION,
        "Practitioner" => PRACTITIONER,
        "RiskAssessment" => RISK_ASSESSMENT,
        "ValueSet" => VALUE_SET,
        _ => &[],
    }
}

type Definitions = Vec<(Option<&'static str>, Arc<ParamDef>)>;

fn definitions() -> &'static Definitions {
    static DEFINITIONS: OnceLock<Definitions> = OnceLock::new();
    DEFINITIONS.get_or_init(|| {
        const TYPES: [&str; 8] = [
            "Patient",
            "Observation",
            "Encounter",
            "List",
            "Organization",
            "Practitioner",
            "RiskAssessment",
            "ValueSet",
        ];
        let mut all: Definitions = COMMON
            .iter()
            .map(|def| (None, Arc::new(ParamDef::from(def))))
            .collect();
        for name in TYPES {
            let resource_type: ResourceType = name.parse().expect("built-in type is known");
            for def in per_type(resource_type) {
                all.push((Some(name), Arc::new(ParamDef::from(def))));
            }
        }
        all
    })
}

pub fn lookup(resource_type: Option<ResourceType>, name: &str) -> Option<Arc<ParamDef>> {
    definitions()
        .iter()
        .find(|(kind, def)| {
            def.name == name
                && match kind {
                    None => true,
                    Some(text) => resource_type.is_some_and(|wanted| wanted.as_str() == *text),
                }
        })
        .map(|(_, def)| Arc::clone(def))
}

pub fn for_type(resource_type: ResourceType) -> Vec<Arc<ParamDef>> {
    definitions()
        .iter()
        .filter(|(kind, _)| kind.is_none() || *kind == Some(resource_type.as_str()))
        .map(|(_, def)| Arc::clone(def))
        .collect()
}

pub fn references(resource_type: ResourceType) -> Vec<Arc<ParamDef>> {
    definitions()
        .iter()
        .filter(|(kind, def)| {
            *kind == Some(resource_type.as_str()) && def.value_type == ValueType::Reference
        })
        .map(|(_, def)| Arc::clone(def))
        .collect()
}

pub fn common() -> Vec<Arc<ParamDef>> {
    definitions()
        .iter()
        .filter(|(kind, _)| kind.is_none())
        .map(|(_, def)| Arc::clone(def))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredParam {
    pub def: Arc<ParamDef>,
    pub base: Vec<ResourceType>,
    pub url: String,
    pub status: ParamStatus,
}

#[derive(Default)]
struct Inner {
    custom: Vec<RegisteredParam>,
    version: u64,
}

fn accepted(inner: &Inner, entry: &RegisteredParam) -> Result<(), Error> {
    for resource_type in &entry.base {
        if lookup(Some(*resource_type), &entry.def.name).is_some() {
            return Err(Error::Duplicate(format!(
                "{} already implements {:?}",
                resource_type.as_str(),
                entry.def.name
            )));
        }
        let clash = inner.custom.iter().any(|held| {
            held.url != entry.url
                && held.def.name == entry.def.name
                && held.base.contains(resource_type)
        });
        if clash {
            return Err(Error::Duplicate(format!(
                "{:?} is already registered for {}",
                entry.def.name,
                resource_type.as_str()
            )));
        }
    }
    Ok(())
}

#[derive(Default)]
pub struct Registry {
    inner: RwLock<Inner>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    pub fn version(&self) -> u64 {
        self.inner.read().map(|inner| inner.version).unwrap_or_default()
    }

    pub fn lookup(&self, resource_type: Option<ResourceType>, name: &str) -> Option<Arc<ParamDef>> {
        lookup(resource_type, name).or_else(|| self.custom(resource_type, name).map(|found| found.def))
    }

    pub fn searchable(
        &self,
        resource_type: Option<ResourceType>,
        name: &str,
    ) -> Result<Option<Arc<ParamDef>>, Error> {
        if let Some(def) = lookup(resource_type, name) {
            return Ok(Some(def));
        }
        match self.custom(resource_type, name) {
            None => Ok(None),
            Some(found) if found.status.is_searchable() => Ok(Some(found.def)),
            Some(found) => Err(Error::UnsupportedParameter(format!(
                "{name:?} is {}",
                found.status.as_str()
            ))),
        }
    }

    pub fn references(&self, resource_type: ResourceType) -> Vec<Arc<ParamDef>> {
        let mut found = references(resource_type);
        found.extend(
            self.entries()
                .into_iter()
                .filter(|entry| {
                    entry.base.contains(&resource_type)
                        && entry.def.value_type == ValueType::Reference
                })
                .map(|entry| entry.def),
        );
        found
    }

    pub fn register(&self, entry: RegisteredParam) -> Result<(), Error> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| Error::Internal("registry lock poisoned".to_owned()))?;
        accepted(&inner, &entry)?;
        inner.custom.retain(|held| held.url != entry.url);
        inner.custom.push(entry);
        inner.version += 1;
        Ok(())
    }

    pub fn accepts(&self, entry: &RegisteredParam) -> Result<(), Error> {
        let inner = self
            .inner
            .read()
            .map_err(|_| Error::Internal("registry lock poisoned".to_owned()))?;
        accepted(&inner, entry)
    }

    pub fn remove(&self, url: &str) -> bool {
        let Ok(mut inner) = self.inner.write() else {
            return false;
        };
        let before = inner.custom.len();
        inner.custom.retain(|held| held.url != url);
        let removed = inner.custom.len() != before;
        if removed {
            inner.version += 1;
        }
        removed
    }

    pub fn replace(&self, entries: Vec<RegisteredParam>) {
        if let Ok(mut inner) = self.inner.write() {
            if inner.custom != entries {
                inner.custom = entries;
                inner.version += 1;
            }
        }
    }

    pub fn entries(&self) -> Vec<RegisteredParam> {
        self.inner
            .read()
            .map(|inner| inner.custom.clone())
            .unwrap_or_default()
    }

    pub fn entry(&self, url: &str) -> Option<RegisteredParam> {
        self.entries().into_iter().find(|held| held.url == url)
    }

    fn custom(&self, resource_type: Option<ResourceType>, name: &str) -> Option<RegisteredParam> {
        self.entries().into_iter().find(|held| {
            held.def.name == name
                && resource_type.is_some_and(|wanted| held.base.contains(&wanted))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(name: &str, base: &str, status: ParamStatus) -> RegisteredParam {
        let resource_type: ResourceType = base.parse().unwrap();
        RegisteredParam {
            def: Arc::new(ParamDef {
                name: name.to_owned(),
                value_type: ValueType::Token,
                target: Target::path(["extension.valueCode"]),
                targets: Vec::new(),
                sortable: false,
                url: Some(format!("urn:p:{name}")),
            }),
            base: vec![resource_type],
            url: format!("urn:p:{name}"),
            status,
        }
    }

    #[test]
    fn every_common_parameter_resolves_without_a_type() {
        for def in common() {
            assert_eq!(lookup(None, &def.name).map(|found| found.name.clone()), Some(def.name.clone()));
        }
        assert!(lookup(None, "name").is_none());
    }

    #[test]
    fn every_registered_type_resolves_its_own_parameters() {
        for (type_name, name) in [
            ("Patient", "family"),
            ("Observation", "value-quantity"),
            ("Encounter", "class"),
            ("List", "item"),
            ("Organization", "name"),
            ("Practitioner", "given"),
            ("RiskAssessment", "probability"),
            ("ValueSet", "url"),
        ] {
            let resource_type = type_name.parse().unwrap();
            assert!(lookup(Some(resource_type), name).is_some(), "{type_name} {name}");
        }
        let unregistered = "Device".parse().unwrap();
        assert!(lookup(Some(unregistered), "patient").is_none());
        assert!(lookup(Some("Patient".parse().unwrap()), "_id").is_some());
        assert_eq!(references("Patient".parse().unwrap()).len(), 2);
    }

    #[test]
    fn a_composite_value_is_split_on_the_separator() {
        let def = lookup(Some("Observation".parse().unwrap()), "code-value-quantity").unwrap();
        assert!(def.value("http://loinc.org|8867-4$72.5").unwrap().components().is_some());
        assert!(matches!(def.value("8867-4").unwrap_err(), Error::InvalidParameter(_)));
        assert!(def.paths().is_empty());
    }

    #[test]
    fn a_composite_type_alone_cannot_parse_a_value() {
        let error = SearchValue::parse(ValueType::Composite, "a$b").unwrap_err();
        assert!(matches!(error, Error::InvalidParameter(_)));
    }

    #[test]
    fn only_declared_parameters_are_sortable() {
        let patient = Some("Patient".parse().unwrap());
        assert!(lookup(patient, "birthdate").is_some_and(|def| def.sortable));
        assert!(lookup(patient, "identifier").is_some_and(|def| !def.sortable));
    }

    #[test]
    fn a_custom_parameter_answers_only_once_it_is_searchable() {
        let registry = Registry::new();
        registry.register(custom("risk-band", "Patient", ParamStatus::Supported)).unwrap();
        let patient = Some("Patient".parse().unwrap());
        assert!(registry.lookup(patient, "risk-band").is_some());
        assert!(matches!(
            registry.searchable(patient, "risk-band").unwrap_err(),
            Error::UnsupportedParameter(_)
        ));
        registry.register(custom("risk-band", "Patient", ParamStatus::Searchable)).unwrap();
        assert!(registry.searchable(patient, "risk-band").unwrap().is_some());
        assert_eq!(registry.entries().len(), 1);
        assert_eq!(registry.version(), 2);
        assert!(registry.searchable(patient, "nonesuch").unwrap().is_none());
    }

    #[test]
    fn a_custom_parameter_never_shadows_a_built_in_one() {
        let registry = Registry::new();
        let error = registry.register(custom("family", "Patient", ParamStatus::Searchable)).unwrap_err();
        assert!(matches!(error, Error::Duplicate(_)));
        registry.register(custom("risk-band", "Patient", ParamStatus::Searchable)).unwrap();
        let mut other = custom("risk-band", "Patient", ParamStatus::Searchable);
        other.url = "urn:p:other".to_owned();
        assert!(matches!(registry.register(other).unwrap_err(), Error::Duplicate(_)));
    }

    #[test]
    fn definitions_are_removed_and_replaced_wholesale() {
        let registry = Registry::new();
        registry.register(custom("risk-band", "Patient", ParamStatus::Searchable)).unwrap();
        assert!(registry.entry("urn:p:risk-band").is_some());
        assert!(registry.remove("urn:p:risk-band"));
        assert!(!registry.remove("urn:p:risk-band"));
        assert!(registry.entries().is_empty());
        let converged = vec![custom("risk-band", "Patient", ParamStatus::Searchable)];
        registry.replace(converged.clone());
        assert_eq!(registry.entries(), converged);
        let before = registry.version();
        registry.replace(converged);
        assert_eq!(registry.version(), before);
    }

    #[test]
    fn a_custom_reference_parameter_joins_the_built_in_ones() {
        let registry = Registry::new();
        let mut entry = custom("care-team", "Patient", ParamStatus::Searchable);
        entry.def = Arc::new(ParamDef {
            name: "care-team".to_owned(),
            value_type: ValueType::Reference,
            target: Target::path(["extension.valueReference"]),
            targets: vec!["Organization".to_owned()],
            sortable: false,
            url: Some(entry.url.clone()),
        });
        registry.register(entry).unwrap();
        let found = registry.references("Patient".parse().unwrap());
        assert!(found.iter().any(|def| def.name == "care-team"));
        assert_eq!(found.first().map(|def| def.paths().len()), Some(1));
    }
}

#[cfg(test)]
mod type_tests {
    use super::*;

    #[test]
    fn a_type_carries_its_own_parameters_and_the_common_ones() {
        let kind: ResourceType = "Patient".parse().unwrap();
        let defs = for_type(kind);
        assert!(defs.iter().any(|def| def.name == "_id"));
        assert!(defs.iter().any(|def| def.name == "name"));
        assert!(!defs.iter().any(|def| def.name == "status"));
        assert!(defs.len() > common().len());
    }
}
