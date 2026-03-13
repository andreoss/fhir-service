use crate::outcome::{IssueCode, IssueSeverity};
use crate::validate::Issue;
use crate::Error;
use serde_json::{Map, Value};

const CHOICE: &str = "[x]";

#[derive(Debug, Clone, PartialEq)]
pub struct Constraint {
    pub steps: Vec<String>,
    pub slice: Option<String>,
    pub min: Option<u64>,
    pub max: Option<u64>,
    pub types: Vec<String>,
    pub targets: Vec<String>,
    pub fixed: Option<Value>,
    pub pattern: Option<Value>,
    pub binding: Option<Binding>,
    pub discriminators: Vec<Discriminator>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub strength: String,
    pub value_set: String,
}

impl Binding {
    pub fn is_enforced(&self) -> bool {
        matches!(self.strength.as_str(), "required" | "extensible")
    }

    pub fn is_required(&self) -> bool {
        self.strength == "required"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discriminator {
    pub kind: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    url: String,
    base_type: String,
    constraints: Vec<Constraint>,
}

pub trait CodeSource {
    fn codes(&self, value_set: &str) -> Option<Vec<(Option<String>, String)>>;
}

impl Profile {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn base_type(&self) -> &str {
        &self.base_type
    }

    pub fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    pub fn parse(definition: &Value) -> Result<Profile, Error> {
        let object = definition
            .as_object()
            .ok_or_else(|| Error::InvalidEnvelope("a profile is a resource".to_owned()))?;
        if object.get("resourceType").and_then(Value::as_str) != Some("StructureDefinition") {
            return Err(Error::InvalidEnvelope(
                "a profile is a StructureDefinition".to_owned(),
            ));
        }
        let url = object
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidEnvelope("a profile names no url".to_owned()))?
            .to_owned();
        let base_type = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::InvalidEnvelope(format!("profile {url} names no type it constrains"))
            })?
            .to_owned();
        let elements = object
            .get("snapshot")
            .and_then(|held| held.get("element"))
            .or_else(|| {
                object
                    .get("differential")
                    .and_then(|held| held.get("element"))
            })
            .and_then(Value::as_array)
            .ok_or_else(|| Error::InvalidEnvelope(format!("profile {url} states no elements")))?;
        let mut constraints = Vec::new();
        for element in elements {
            if let Some(held) = constraint(&base_type, element) {
                constraints.push(held);
            }
        }
        Ok(Profile {
            url,
            base_type,
            constraints,
        })
    }

    pub fn judge(&self, body: &Value, codes: &dyn CodeSource) -> Vec<Issue> {
        let mut issues = Vec::new();
        let sliced = self.slices();
        for constraint in &self.constraints {
            if constraint.steps.is_empty() {
                continue;
            }
            if constraint.slice.is_some() {
                continue;
            }
            self.apply(constraint, body, codes, &sliced, &mut issues);
        }
        for constraint in &self.constraints {
            if constraint.slice.is_none() {
                continue;
            }
            self.apply_slice(constraint, body, codes, &mut issues);
        }
        issues
    }

    fn slices(&self) -> Vec<(Vec<String>, Vec<Discriminator>)> {
        self.constraints
            .iter()
            .filter(|held| !held.discriminators.is_empty())
            .map(|held| (held.steps.clone(), held.discriminators.clone()))
            .collect()
    }

    fn apply(
        &self,
        constraint: &Constraint,
        body: &Value,
        codes: &dyn CodeSource,
        sliced: &[(Vec<String>, Vec<Discriminator>)],
        issues: &mut Vec<Issue>,
    ) {
        let _ = sliced;
        for (expression, found) in gathered(&self.base_type, body, &constraint.steps) {
            self.cardinality(constraint, &expression, &found, issues);
            for (index, value) in found.iter().enumerate() {
                let at = numbered(&expression, index, found.len());
                self.value_rules(constraint, &at, value, codes, issues);
            }
        }
    }

    fn apply_slice(
        &self,
        constraint: &Constraint,
        body: &Value,
        codes: &dyn CodeSource,
        issues: &mut Vec<Issue>,
    ) {
        let Some(name) = constraint.slice.as_deref() else {
            return;
        };
        let discriminators = self
            .constraints
            .iter()
            .find(|held| held.steps == constraint.steps && !held.discriminators.is_empty())
            .map(|held| held.discriminators.clone())
            .unwrap_or_default();
        if discriminators.is_empty() {
            return;
        }
        for (expression, found) in gathered(&self.base_type, body, &constraint.steps) {
            let mut matched = Vec::new();
            for (index, value) in found.iter().enumerate() {
                if self.in_slice(constraint, &discriminators, value) {
                    matched.push((numbered(&expression, index, found.len()), value.clone()));
                }
            }
            let held: Vec<Value> = matched.iter().map(|(_, value)| value.clone()).collect();
            self.cardinality(
                constraint,
                &format!("{expression} (slice {name})"),
                &held,
                issues,
            );
            for (at, value) in &matched {
                self.value_rules(constraint, at, value, codes, issues);
            }
        }
    }

    fn in_slice(
        &self,
        constraint: &Constraint,
        discriminators: &[Discriminator],
        value: &Value,
    ) -> bool {
        discriminators.iter().all(|discriminator| {
            if !matches!(discriminator.kind.as_str(), "value" | "pattern") {
                return false;
            }
            let Some(wanted) = self.slice_value(constraint, &discriminator.path) else {
                return false;
            };
            let steps: Vec<String> = match discriminator.path.as_str() {
                "$this" => Vec::new(),
                path => path.split('.').map(str::to_owned).collect(),
            };
            let found = within(value, &steps);
            found.iter().any(|held| matches(&wanted, held))
        })
    }

    fn slice_value(&self, constraint: &Constraint, path: &str) -> Option<Value> {
        let mut wanted: Vec<String> = constraint.steps.clone();
        if path != "$this" {
            wanted.extend(path.split('.').map(str::to_owned));
        }
        self.constraints
            .iter()
            .filter(|held| held.slice.as_deref() == constraint.slice.as_deref())
            .find(|held| held.steps == wanted)
            .and_then(|held| held.fixed.clone().or_else(|| held.pattern.clone()))
    }

    fn cardinality(
        &self,
        constraint: &Constraint,
        expression: &str,
        found: &[Value],
        issues: &mut Vec<Issue>,
    ) {
        if let Some(min) = constraint.min {
            if (found.len() as u64) < min {
                issues.push(refused(
                    format!(
                        "profile {}: {expression} carries {} of the {min} it requires",
                        self.url,
                        found.len()
                    ),
                    expression,
                ));
            }
        }
        if let Some(max) = constraint.max {
            if (found.len() as u64) > max {
                issues.push(refused(
                    format!(
                        "profile {}: {expression} carries {} where at most {max} is allowed",
                        self.url,
                        found.len()
                    ),
                    expression,
                ));
            }
        }
    }

    fn value_rules(
        &self,
        constraint: &Constraint,
        at: &str,
        value: &Value,
        codes: &dyn CodeSource,
        issues: &mut Vec<Issue>,
    ) {
        if let Some(fixed) = &constraint.fixed {
            if fixed != value {
                issues.push(refused(
                    format!(
                        "profile {}: {at} is not the value the profile fixes",
                        self.url
                    ),
                    at,
                ));
            }
        }
        if let Some(pattern) = &constraint.pattern {
            if !matches(pattern, value) {
                issues.push(refused(
                    format!(
                        "profile {}: {at} does not carry the pattern the profile states",
                        self.url
                    ),
                    at,
                ));
            }
        }
        if !constraint.targets.is_empty() {
            self.reference_target(constraint, at, value, issues);
        }
        if let Some(binding) = &constraint.binding {
            self.binding(binding, at, value, codes, issues);
        }
    }

    fn reference_target(
        &self,
        constraint: &Constraint,
        at: &str,
        value: &Value,
        issues: &mut Vec<Issue>,
    ) {
        let Some(reference) = value.get("reference").and_then(Value::as_str) else {
            return;
        };
        let named = reference
            .rsplit_once('/')
            .map(|(head, _)| head.rsplit('/').next().unwrap_or(head))
            .unwrap_or(reference);
        let allowed = constraint.targets.iter().any(|target| {
            target
                .rsplit('/')
                .next()
                .is_some_and(|tail| tail == named || target == named)
        });
        if !allowed {
            issues.push(refused(
                format!(
                    "profile {}: {at} points at a {named}, which the profile does not allow",
                    self.url
                ),
                at,
            ));
        }
    }

    fn binding(
        &self,
        binding: &Binding,
        at: &str,
        value: &Value,
        codes: &dyn CodeSource,
        issues: &mut Vec<Issue>,
    ) {
        if !binding.is_enforced() {
            return;
        }
        let carried = codings(value);
        if carried.is_empty() {
            return;
        }
        let Some(held) = codes.codes(&binding.value_set) else {
            issues.push(Issue {
                severity: IssueSeverity::Warning,
                code: IssueCode::NotSupported,
                diagnostics: format!(
                    "profile {}: {at} is bound to {}, which this instance does not hold, so the code was not judged",
                    self.url, binding.value_set
                ),
                expression: Some(at.to_owned()),
            });
            return;
        };
        let found = carried.iter().any(|(system, code)| {
            held.iter().any(|(held_system, held_code)| {
                held_code == code
                    && match (held_system, system) {
                        (Some(one), Some(other)) => one == other,
                        (None, _) | (_, None) => true,
                    }
            })
        });
        if found {
            return;
        }
        let severity = match binding.is_required() {
            true => IssueSeverity::Error,
            false => IssueSeverity::Warning,
        };
        issues.push(Issue {
            severity,
            code: IssueCode::BusinessRule,
            diagnostics: format!(
                "profile {}: {at} carries a code {} does not hold",
                self.url, binding.value_set
            ),
            expression: Some(at.to_owned()),
        });
    }
}

fn refused(diagnostics: String, expression: &str) -> Issue {
    Issue {
        severity: IssueSeverity::Error,
        code: IssueCode::Invalid,
        diagnostics,
        expression: Some(expression.to_owned()),
    }
}

fn codings(value: &Value) -> Vec<(Option<String>, String)> {
    let owned = |system: Option<&str>, code: &str| (system.map(str::to_owned), code.to_owned());
    if let Some(code) = value.as_str() {
        return vec![owned(None, code)];
    }
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    if let Some(code) = object.get("code").and_then(Value::as_str) {
        return vec![owned(object.get("system").and_then(Value::as_str), code)];
    }
    object
        .get("coding")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let code = item.get("code").and_then(Value::as_str)?;
                    Some(owned(item.get("system").and_then(Value::as_str), code))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn matches(pattern: &Value, value: &Value) -> bool {
    match (pattern, value) {
        (Value::Object(wanted), Value::Object(held)) => wanted
            .iter()
            .all(|(name, inner)| held.get(name).is_some_and(|found| matches(inner, found))),
        (Value::Array(wanted), Value::Array(held)) => wanted
            .iter()
            .all(|inner| held.iter().any(|found| matches(inner, found))),
        (one, other) => one == other,
    }
}

fn numbered(expression: &str, index: usize, total: usize) -> String {
    match total > 1 {
        true => format!("{expression}[{index}]"),
        false => expression.to_owned(),
    }
}

fn gathered(base_type: &str, body: &Value, steps: &[String]) -> Vec<(String, Vec<Value>)> {
    let (parents, last) = match steps.split_last() {
        None => return Vec::new(),
        Some((last, parents)) => (parents, last),
    };
    let mut held = Vec::new();
    for (expression, parent) in expanded(base_type.to_owned(), body, parents) {
        let found = named(&parent, last);
        held.push((format!("{expression}.{}", base_of(last)), found));
    }
    held
}

fn expanded(expression: String, value: &Value, steps: &[String]) -> Vec<(String, Value)> {
    let Some((step, rest)) = steps.split_first() else {
        return vec![(expression, value.clone())];
    };
    let found = named(value, step);
    let total = found.len();
    let mut held = Vec::new();
    for (index, item) in found.into_iter().enumerate() {
        let at = numbered(&format!("{expression}.{}", base_of(step)), index, total);
        held.extend(expanded(at, &item, rest));
    }
    held
}

fn named(value: &Value, step: &str) -> Vec<Value> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let held = match step.ends_with(CHOICE) {
        false => object.get(step).cloned(),
        true => chosen(object, base_of(step)),
    };
    match held {
        None => Vec::new(),
        Some(Value::Array(items)) => items,
        Some(found) => vec![found],
    }
}

fn within(value: &Value, steps: &[String]) -> Vec<Value> {
    let Some((step, rest)) = steps.split_first() else {
        return vec![value.clone()];
    };
    named(value, step)
        .into_iter()
        .flat_map(|held| within(&held, rest))
        .collect()
}

fn chosen(object: &Map<String, Value>, base: &str) -> Option<Value> {
    object
        .iter()
        .find(|(name, _)| {
            name.starts_with(base)
                && name.len() > base.len()
                && name[base.len()..].starts_with(char::is_uppercase)
        })
        .map(|(_, value)| value.clone())
}

fn base_of(step: &str) -> &str {
    step.strip_suffix(CHOICE).unwrap_or(step)
}

fn constraint(base_type: &str, element: &Value) -> Option<Constraint> {
    let path = element.get("path").and_then(Value::as_str)?;
    let steps: Vec<String> = path
        .strip_prefix(base_type)
        .and_then(|rest| rest.strip_prefix('.'))
        .map(|rest| rest.split('.').map(str::to_owned).collect())
        .unwrap_or_default();
    let slice = element
        .get("sliceName")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let min = element.get("min").and_then(Value::as_u64);
    let max = element
        .get("max")
        .and_then(Value::as_str)
        .and_then(|held| match held {
            "*" => None,
            number => number.parse::<u64>().ok(),
        });
    let types: Vec<String> = element
        .get("type")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("code").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let targets: Vec<String> = element
        .get("type")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("targetProfile"))
                .flat_map(|held| match held {
                    Value::String(one) => vec![one.clone()],
                    Value::Array(many) => many
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                    _ => Vec::new(),
                })
                .collect()
        })
        .unwrap_or_default();
    let fixed = prefixed(element, "fixed");
    let pattern = prefixed(element, "pattern");
    let binding = element.get("binding").and_then(|held| {
        Some(Binding {
            strength: held.get("strength").and_then(Value::as_str)?.to_owned(),
            value_set: held
                .get("valueSet")
                .and_then(Value::as_str)?
                .split('|')
                .next()
                .unwrap_or_default()
                .to_owned(),
        })
    });
    let discriminators: Vec<Discriminator> = element
        .get("slicing")
        .and_then(|held| held.get("discriminator"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(Discriminator {
                        kind: item.get("type").and_then(Value::as_str)?.to_owned(),
                        path: item.get("path").and_then(Value::as_str)?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let states = min.is_some()
        || max.is_some()
        || fixed.is_some()
        || pattern.is_some()
        || binding.is_some()
        || !targets.is_empty()
        || !discriminators.is_empty();
    if !states {
        return None;
    }
    Some(Constraint {
        steps,
        slice,
        min,
        max,
        types,
        targets,
        fixed,
        pattern,
        binding,
        discriminators,
    })
}

fn prefixed(element: &Value, prefix: &str) -> Option<Value> {
    element.as_object()?.iter().find_map(|(name, value)| {
        (name.starts_with(prefix)
            && name.len() > prefix.len()
            && name[prefix.len()..].starts_with(char::is_uppercase))
        .then(|| value.clone())
    })
}
