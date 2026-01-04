use crate::search::value::ValueType;
use crate::ResourceType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Id,
    LastUpdated,
    Path(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamDef {
    pub name: &'static str,
    pub value_type: ValueType,
    pub target: Target,
    pub sortable: bool,
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

const PATIENT: &[ParamDef] = &[
    ParamDef {
        name: "active",
        value_type: ValueType::Token,
        target: Target::Path(&["active"]),
        sortable: false,
    },
    ParamDef {
        name: "gender",
        value_type: ValueType::Token,
        target: Target::Path(&["gender"]),
        sortable: true,
    },
    ParamDef {
        name: "identifier",
        value_type: ValueType::Token,
        target: Target::Path(&["identifier"]),
        sortable: false,
    },
];

fn per_type(resource_type: ResourceType) -> &'static [ParamDef] {
    match resource_type.as_str() {
        "Patient" => PATIENT,
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
