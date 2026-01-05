use crate::search::value::{SearchValue, ValueType};
use crate::{Error, ResourceType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubDef {
    pub value_type: ValueType,
    pub paths: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositeDef {
    pub base: &'static [&'static str],
    pub left: SubDef,
    pub right: SubDef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Id,
    LastUpdated,
    Path(&'static [&'static str]),
    Composite(&'static CompositeDef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamDef {
    pub name: &'static str,
    pub value_type: ValueType,
    pub target: Target,
    pub sortable: bool,
}

impl ParamDef {
    pub fn value(&self, raw: &str) -> Result<SearchValue, Error> {
        match self.target {
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
}


const COMMON: &[ParamDef] = &[
    ParamDef {
        name: "_id",
        value_type: ValueType::Token,
        target: Target::Id,
        sortable: true,
    },
    ParamDef {
        name: "_lastUpdated",
        value_type: ValueType::Date,
        target: Target::LastUpdated,
        sortable: true,
    },
    ParamDef {
        name: "_profile",
        value_type: ValueType::Uri,
        target: Target::Path(&["meta.profile"]),
        sortable: false,
    },
    ParamDef {
        name: "_tag",
        value_type: ValueType::Token,
        target: Target::Path(&["meta.tag"]),
        sortable: false,
    },
    ParamDef {
        name: "_security",
        value_type: ValueType::Token,
        target: Target::Path(&["meta.security"]),
        sortable: false,
    },
];

const fn def(name: &'static str, value_type: ValueType, paths: &'static [&'static str]) -> ParamDef {
    ParamDef {
        name,
        value_type,
        target: Target::Path(paths),
        sortable: false,
    }
}

const fn sorted(
    name: &'static str,
    value_type: ValueType,
    paths: &'static [&'static str],
) -> ParamDef {
    ParamDef {
        name,
        value_type,
        target: Target::Path(paths),
        sortable: true,
    }
}

const CODE_VALUE_QUANTITY: CompositeDef = CompositeDef {
    base: &[""],
    left: SubDef {
        value_type: ValueType::Token,
        paths: &["code"],
    },
    right: SubDef {
        value_type: ValueType::Quantity,
        paths: &["valueQuantity"],
    },
};

const COMPONENT_CODE_VALUE_QUANTITY: CompositeDef = CompositeDef {
    base: &["component"],
    left: SubDef {
        value_type: ValueType::Token,
        paths: &["code"],
    },
    right: SubDef {
        value_type: ValueType::Quantity,
        paths: &["valueQuantity"],
    },
};

const PATIENT: &[ParamDef] = &[
    def("active", ValueType::Token, &["active"]),
    def("address", ValueType::String, &["address"]),
    sorted("birthdate", ValueType::Date, &["birthDate"]),
    def("death-date", ValueType::Date, &["deceasedDateTime"]),
    sorted("family", ValueType::String, &["name.family"]),
    sorted("gender", ValueType::Token, &["gender"]),
    def("general-practitioner", ValueType::Reference, &["generalPractitioner"]),
    def("given", ValueType::String, &["name.given"]),
    def("identifier", ValueType::Token, &["identifier"]),
    sorted("name", ValueType::String, &["name"]),
    def("organization", ValueType::Reference, &["managingOrganization"]),
    def("telecom", ValueType::Token, &["telecom"]),
];

const OBSERVATION: &[ParamDef] = &[
    def("category", ValueType::Token, &["category"]),
    def("code", ValueType::Token, &["code"]),
    ParamDef {
        name: "code-value-quantity",
        value_type: ValueType::Composite,
        target: Target::Composite(&CODE_VALUE_QUANTITY),
        sortable: false,
    },
    def("component-code", ValueType::Token, &["component.code"]),
    ParamDef {
        name: "component-code-value-quantity",
        value_type: ValueType::Composite,
        target: Target::Composite(&COMPONENT_CODE_VALUE_QUANTITY),
        sortable: false,
    },
    def("component-value-quantity", ValueType::Quantity, &["component.valueQuantity"]),
    sorted("date", ValueType::Date, &["effectiveDateTime", "effectivePeriod"]),
    def("encounter", ValueType::Reference, &["encounter"]),
    def("identifier", ValueType::Token, &["identifier"]),
    def("patient", ValueType::Reference, &["subject"]),
    sorted("status", ValueType::Token, &["status"]),
    def("subject", ValueType::Reference, &["subject"]),
    def("value-quantity", ValueType::Quantity, &["valueQuantity"]),
    def("value-string", ValueType::String, &["valueString"]),
];

const ENCOUNTER: &[ParamDef] = &[
    def("class", ValueType::Token, &["class"]),
    sorted("date", ValueType::Date, &["period"]),
    def("identifier", ValueType::Token, &["identifier"]),
    def("patient", ValueType::Reference, &["subject"]),
    def("service-provider", ValueType::Reference, &["serviceProvider"]),
    sorted("status", ValueType::Token, &["status"]),
    def("subject", ValueType::Reference, &["subject"]),
];

const LIST: &[ParamDef] = &[
    sorted("date", ValueType::Date, &["date"]),
    def("identifier", ValueType::Token, &["identifier"]),
    def("item", ValueType::Reference, &["entry.item"]),
    def("patient", ValueType::Reference, &["subject"]),
    sorted("status", ValueType::Token, &["status"]),
    def("subject", ValueType::Reference, &["subject"]),
    sorted("title", ValueType::String, &["title"]),
];

const ORGANIZATION: &[ParamDef] = &[
    def("active", ValueType::Token, &["active"]),
    def("address", ValueType::String, &["address"]),
    def("identifier", ValueType::Token, &["identifier"]),
    sorted("name", ValueType::String, &["name"]),
];

const PRACTITIONER: &[ParamDef] = &[
    def("active", ValueType::Token, &["active"]),
    def("family", ValueType::String, &["name.family"]),
    def("given", ValueType::String, &["name.given"]),
    def("identifier", ValueType::Token, &["identifier"]),
    sorted("name", ValueType::String, &["name"]),
];

const RISK_ASSESSMENT: &[ParamDef] = &[
    def("identifier", ValueType::Token, &["identifier"]),
    def("patient", ValueType::Reference, &["subject"]),
    def("probability", ValueType::Number, &["prediction.probabilityDecimal"]),
    def("subject", ValueType::Reference, &["subject"]),
];

const VALUE_SET: &[ParamDef] = &[
    def("identifier", ValueType::Token, &["identifier"]),
    sorted("name", ValueType::String, &["name"]),
    sorted("status", ValueType::Token, &["status"]),
    def("url", ValueType::Uri, &["url"]),
    def("version", ValueType::Token, &["version"]),
];

fn per_type(resource_type: ResourceType) -> &'static [ParamDef] {
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

pub fn lookup(resource_type: Option<ResourceType>, name: &str) -> Option<&'static ParamDef> {
    COMMON
        .iter()
        .chain(resource_type.map(per_type).unwrap_or_default())
        .find(|def| def.name == name)
}

pub fn common() -> &'static [ParamDef] {
    COMMON
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_common_parameter_resolves_without_a_type() {
        for def in common() {
            assert_eq!(lookup(None, def.name).map(|found| found.name), Some(def.name));
        }
        assert!(lookup(None, "name").is_none());
    }
}
