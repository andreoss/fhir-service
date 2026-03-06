use fhir_core::search::{
    Chain, ChainDirection, Comparator, Compartment, Criterion, Filter, Grant, IndexKey, Modifier,
    SearchValue, Target, Token, TokenSystem, ValueType,
};
use fhir_core::{Error, InstantPeriod};
use fhir_store::index::{
    IDENTIFIER, LEFT, MAIN, NARRATIVE, OF_TYPE, PLAIN, PRESENCE, RIGHT, WORDS,
};
use mongodb::bson::{doc, Bson, Document};

use crate::record::{high_key, low_key};

const DAY: i64 = 86_400;

pub const RESOURCES: &str = "resource";

pub fn all(parts: Vec<Bson>) -> Bson {
    match parts.len() {
        0 => Bson::Boolean(true),
        1 => parts.into_iter().next().unwrap_or(Bson::Boolean(true)),
        _ => Bson::Document(doc! {"$and": parts}),
    }
}

pub fn any(parts: Vec<Bson>) -> Bson {
    match parts.len() {
        0 => Bson::Boolean(false),
        1 => parts.into_iter().next().unwrap_or(Bson::Boolean(false)),
        _ => Bson::Document(doc! {"$or": parts}),
    }
}

fn negate(condition: Bson) -> Bson {
    Bson::Document(doc! {"$not": [condition]})
}

fn equals(left: Bson, right: Bson) -> Bson {
    Bson::Document(doc! {"$eq": [left, right]})
}

fn field(variable: &str, name: &str) -> Bson {
    Bson::String(format!("$${variable}.{name}"))
}

fn optional(variable: &str, name: &str) -> Bson {
    Bson::Document(doc! {"$ifNull": [field(variable, name), Bson::Null]})
}

fn escaped(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for letter in raw.chars() {
        if "\\^$.|?*+()[]{}".contains(letter) {
            out.push('\\');
        }
        out.push(letter);
    }
    out
}

fn matching(input: Bson, pattern: String) -> Bson {
    Bson::Document(doc! {"$regexMatch": {"input": {"$ifNull": [input, ""]}, "regex": pattern}})
}

fn wanted(value: &SearchValue) -> Option<&str> {
    match value {
        SearchValue::Text(text) | SearchValue::Uri(text) | SearchValue::Reference(text) => {
            Some(text)
        }
        SearchValue::Token(token) => token.code.as_deref(),
        _ => None,
    }
}

fn param_key(filter: &Filter) -> String {
    filter.index.clone().unwrap_or_else(|| filter.name.clone())
}

fn value_type_of(filter: &Filter) -> ValueType {
    match filter.values.first() {
        Some(SearchValue::Token(_)) => ValueType::Token,
        Some(SearchValue::Text(_)) => ValueType::String,
        Some(SearchValue::Number { .. }) => ValueType::Number,
        Some(SearchValue::Date { .. }) => ValueType::Date,
        Some(SearchValue::Quantity { .. }) => ValueType::Quantity,
        Some(SearchValue::Reference(_)) => ValueType::Reference,
        Some(SearchValue::Uri(_)) => ValueType::Uri,
        Some(SearchValue::Composite { .. }) => ValueType::Composite,
        Some(SearchValue::OfType { .. }) => ValueType::Token,
        Some(SearchValue::Missing(_)) | None => ValueType::Token,
    }
}

fn array_of(value_type: ValueType) -> Result<&'static str, Error> {
    match value_type {
        ValueType::Token => Ok("token"),
        ValueType::String => Ok("text"),
        ValueType::Number => Ok("number"),
        ValueType::Date => Ok("date"),
        ValueType::Quantity => Ok("quantity"),
        ValueType::Reference => Ok("reference"),
        ValueType::Uri => Ok("uri"),
        ValueType::Composite => Err(Error::UnsupportedParameter(
            "a composite cannot be a component of a composite".to_owned(),
        )),
    }
}

pub fn array_for(filter: &Filter) -> Option<&'static str> {
    array_of(value_type_of(filter)).ok()
}

fn prefixes(text: &str) -> Vec<Bson> {
    let mut found = vec![Bson::String(text.to_owned())];
    for (at, letter) in text.char_indices() {
        if letter == '.' || letter == '/' {
            found.push(Bson::String(text[..at].to_owned()));
        }
    }
    found
}

struct Component {
    array: &'static str,
    condition: Bson,
    variable: String,
}

pub struct Compiler {
    variables: usize,
    lookups: usize,
}

impl Default for Compiler {
    fn default() -> Compiler {
        Compiler::new()
    }
}

impl Compiler {
    pub fn new() -> Compiler {
        Compiler {
            variables: 0,
            lookups: 0,
        }
    }

    fn variable(&mut self) -> String {
        self.variables += 1;
        format!("e{}", self.variables)
    }

    fn alias(&mut self) -> String {
        self.lookups += 1;
        format!("linked{}", self.lookups)
    }

    fn holds(
        &mut self,
        array: &str,
        param: &str,
        slot: &str,
        build: impl FnOnce(&mut Compiler, &str) -> Bson,
    ) -> Bson {
        let variable = self.variable();
        let inner = build(self, &variable);
        let selected = all(vec![
            equals(field(&variable, "param"), Bson::String(param.to_owned())),
            equals(field(&variable, "slot"), Bson::String(slot.to_owned())),
            inner,
        ]);
        Bson::Document(doc! {
            "$gt": [
                {"$size": {"$filter": {
                    "input": {"$ifNull": [format!("${array}"), []]},
                    "as": &variable,
                    "cond": selected,
                }}},
                0
            ]
        })
    }

    fn token_condition(&mut self, token: &Token, variable: &str) -> Bson {
        let system = match &token.system {
            TokenSystem::Any => Bson::Boolean(true),
            TokenSystem::Absent => equals(optional(variable, "system"), Bson::Null),
            TokenSystem::Exact(name) => {
                equals(field(variable, "system"), Bson::String(name.clone()))
            }
        };
        let code = match &token.code {
            None => Bson::Boolean(true),
            Some(text) => {
                let key = IndexKey::of(text);
                let tail = match key.overflow() {
                    Some(rest) => Bson::String(rest.to_owned()),
                    None => Bson::Null,
                };
                all(vec![
                    equals(field(variable, "code"), Bson::String(key.key().to_owned())),
                    equals(optional(variable, "tail"), tail),
                ])
            }
        };
        all(vec![system, code])
    }

    fn number_condition(
        &self,
        comparator: Comparator,
        value: f64,
        tolerance: f64,
        measured: Bson,
    ) -> Bson {
        match comparator {
            Comparator::Eq | Comparator::Ne => all(vec![
                Bson::Document(doc! {"$gte": [measured.clone(), value - tolerance]}),
                Bson::Document(doc! {"$lte": [measured, value + tolerance]}),
            ]),
            Comparator::Gt | Comparator::Sa => Bson::Document(doc! {"$gt": [measured, value]}),
            Comparator::Lt | Comparator::Eb => Bson::Document(doc! {"$lt": [measured, value]}),
            Comparator::Ge => Bson::Document(doc! {"$gte": [measured, value]}),
            Comparator::Le => Bson::Document(doc! {"$lte": [measured, value]}),
            Comparator::Ap => {
                let reach = value.abs() * 0.1 + tolerance;
                all(vec![
                    Bson::Document(doc! {"$gte": [measured.clone(), value - reach]}),
                    Bson::Document(doc! {"$lte": [measured, value + reach]}),
                ])
            }
        }
    }

    fn date_condition(
        &self,
        comparator: Comparator,
        period: &InstantPeriod,
        low: Bson,
        high: Bson,
    ) -> Bson {
        let query_low = Bson::String(low_key(period));
        let query_high = Bson::String(high_key(period));
        match comparator {
            Comparator::Eq | Comparator::Ne => all(vec![
                Bson::Document(doc! {"$gte": [low, query_low]}),
                Bson::Document(doc! {"$lte": [high, query_high]}),
            ]),
            Comparator::Gt => Bson::Document(doc! {"$gt": [high, query_high]}),
            Comparator::Lt => Bson::Document(doc! {"$lt": [low, query_low]}),
            Comparator::Ge => Bson::Document(doc! {"$gte": [high, query_low]}),
            Comparator::Le => Bson::Document(doc! {"$lte": [low, query_high]}),
            Comparator::Sa => Bson::Document(doc! {"$gt": [low, query_high]}),
            Comparator::Eb => Bson::Document(doc! {"$lt": [high, query_low]}),
            Comparator::Ap => match period.widened(DAY) {
                None => Bson::Boolean(false),
                Some(near) => all(vec![
                    Bson::Document(doc! {"$lte": [low, Bson::String(high_key(&near))]}),
                    Bson::Document(doc! {"$gte": [high, Bson::String(low_key(&near))]}),
                ]),
            },
        }
    }

    fn quantity_condition(
        &self,
        number: &SearchValue,
        system: &TokenSystem,
        code: Option<&str>,
        variable: &str,
    ) -> Bson {
        let measure = match number {
            SearchValue::Number {
                comparator,
                value,
                tolerance,
            } => self.number_condition(*comparator, *value, *tolerance, field(variable, "value")),
            _ => Bson::Boolean(false),
        };
        let system = match system {
            TokenSystem::Any => Bson::Boolean(true),
            TokenSystem::Absent => all(vec![
                equals(optional(variable, "system"), Bson::Null),
                equals(field(variable, "structured"), Bson::Boolean(true)),
            ]),
            TokenSystem::Exact(name) => {
                equals(field(variable, "system"), Bson::String(name.clone()))
            }
        };
        let unit = match code {
            None => Bson::Boolean(true),
            Some(text) => equals(field(variable, "code"), Bson::String(text.to_owned())),
        };
        all(vec![measure, system, unit])
    }

    fn reference_condition(&self, text: &str, variable: &str) -> Bson {
        let bound = Bson::String(text.to_owned());
        match text.contains('/') {
            true => equals(field(variable, "pointer"), bound),
            false => any(vec![
                equals(field(variable, "pointer"), bound.clone()),
                equals(field(variable, "logical"), bound),
            ]),
        }
    }

    fn typed_reference_condition(&self, text: &str, kind: &str, variable: &str) -> Bson {
        let bound = Bson::String(text.to_owned());
        let joined = Bson::Document(doc! {
            "$concat": [{"$ifNull": [field(variable, "kind"), ""]}, "/", field(variable, "logical")]
        });
        all(vec![
            equals(field(variable, "kind"), Bson::String(kind.to_owned())),
            any(vec![
                equals(field(variable, "logical"), bound.clone()),
                equals(field(variable, "pointer"), bound.clone()),
                equals(joined, bound),
            ]),
        ])
    }

    fn hierarchy_condition(&self, text: &str, variable: &str, below: bool) -> Bson {
        match below {
            true => any(vec![
                equals(field(variable, "value"), Bson::String(text.to_owned())),
                matching(field(variable, "value"), format!("^{}[./]", escaped(text))),
            ]),
            false => Bson::Document(doc! {"$in": [field(variable, "value"), prefixes(text)]}),
        }
    }

    fn condition(&mut self, value: &SearchValue, variable: &str) -> Bson {
        match value {
            SearchValue::Token(token) => self.token_condition(token, variable),
            SearchValue::Text(text) => matching(
                field(variable, "folded"),
                format!("^{}", escaped(&text.to_lowercase())),
            ),
            SearchValue::Number {
                comparator,
                value,
                tolerance,
            } => self.number_condition(*comparator, *value, *tolerance, field(variable, "value")),
            SearchValue::Date { comparator, period } => self.date_condition(
                *comparator,
                period,
                field(variable, "low"),
                field(variable, "high"),
            ),
            SearchValue::Quantity {
                number,
                system,
                code,
            } => self.quantity_condition(number, system, code.as_deref(), variable),
            SearchValue::Reference(text) => self.reference_condition(text, variable),
            SearchValue::Uri(text) => equals(field(variable, "value"), Bson::String(text.clone())),
            SearchValue::OfType { .. }
            | SearchValue::Composite { .. }
            | SearchValue::Missing(_) => Bson::Boolean(false),
        }
    }

    pub fn value_condition(&mut self, value: &SearchValue, variable: &str) -> Bson {
        self.condition(value, variable)
    }

    fn declared(&mut self, filter: &Filter, value: &SearchValue) -> Result<Bson, Error> {
        let array = array_of(value_type_of(filter))?;
        let param = param_key(filter);
        let value = value.clone();
        Ok(self.holds(array, &param, MAIN, move |compiler, variable| {
            compiler.condition(&value, variable)
        }))
    }

    fn paired(&mut self, param: &str, left: Component, right: Component) -> Bson {
        let Component {
            array: left_array,
            condition: left,
            variable: left_variable,
        } = left;
        let Component {
            array: right_array,
            condition: right,
            variable: right_variable,
        } = right;
        let left_variable = left_variable.as_str();
        let right_variable = right_variable.as_str();
        let inner = Bson::Document(doc! {
            "$gt": [
                {"$size": {"$filter": {
                    "input": {"$ifNull": [format!("${right_array}"), []]},
                    "as": right_variable,
                    "cond": all(vec![
                        equals(field(right_variable, "param"), Bson::String(param.to_owned())),
                        equals(field(right_variable, "slot"), Bson::String(RIGHT.to_owned())),
                        equals(field(right_variable, "ordinal"), field(left_variable, "ordinal")),
                        right,
                    ]),
                }}},
                0
            ]
        });
        Bson::Document(doc! {
            "$gt": [
                {"$size": {"$filter": {
                    "input": {"$ifNull": [format!("${left_array}"), []]},
                    "as": left_variable,
                    "cond": all(vec![
                        equals(field(left_variable, "param"), Bson::String(param.to_owned())),
                        equals(field(left_variable, "slot"), Bson::String(LEFT.to_owned())),
                        left,
                        inner,
                    ]),
                }}},
                0
            ]
        })
    }

    fn of_type(&mut self, value: &SearchValue, param: &str) -> Bson {
        let SearchValue::OfType {
            system,
            code,
            value: identifier,
        } = value
        else {
            return Bson::Boolean(false);
        };
        let token = Token {
            system: system.clone(),
            code: code.clone(),
        };
        let outer = self.variable();
        let inner = self.variable();
        let qualifier = self.token_condition(&token, &inner);
        let held = Bson::Document(doc! {
            "$gt": [
                {"$size": {"$filter": {
                    "input": {"$ifNull": ["$token", []]},
                    "as": &inner,
                    "cond": all(vec![
                        equals(field(&inner, "param"), Bson::String(param.to_owned())),
                        equals(field(&inner, "slot"), Bson::String(OF_TYPE.to_owned())),
                        equals(field(&inner, "ordinal"), field(&outer, "ordinal")),
                        qualifier,
                    ]),
                }}},
                0
            ]
        });
        Bson::Document(doc! {
            "$gt": [
                {"$size": {"$filter": {
                    "input": {"$ifNull": ["$text", []]},
                    "as": &outer,
                    "cond": all(vec![
                        equals(field(&outer, "param"), Bson::String(param.to_owned())),
                        equals(field(&outer, "slot"), Bson::String(OF_TYPE.to_owned())),
                        equals(field(&outer, "value"), Bson::String(identifier.clone())),
                        held,
                    ]),
                }}},
                0
            ]
        })
    }

    fn qualified(&mut self, filter: &Filter, value: &SearchValue) -> Result<Bson, Error> {
        let param = param_key(filter);
        if filter.modifier == Modifier::None && param == "_text" {
            return Ok(self.full_text(value, &param));
        }
        let text = wanted(value).map(str::to_owned);
        Ok(match &filter.modifier {
            Modifier::Exact => match text {
                None => Bson::Boolean(false),
                Some(text) => self.holds("text", &param, PLAIN, move |_, variable| {
                    equals(field(variable, "value"), Bson::String(text))
                }),
            },
            Modifier::Contains => match text {
                None => Bson::Boolean(false),
                Some(text) => self.holds("text", &param, PLAIN, move |_, variable| {
                    matching(field(variable, "folded"), escaped(&text.to_lowercase()))
                }),
            },
            Modifier::Text => match text {
                None => Bson::Boolean(false),
                Some(text) => self.holds("text", &param, NARRATIVE, move |_, variable| {
                    matching(field(variable, "folded"), escaped(&text.to_lowercase()))
                }),
            },
            Modifier::Below | Modifier::Above => {
                let below = matches!(filter.modifier, Modifier::Below);
                match text {
                    None => Bson::Boolean(false),
                    Some(text) => self.holds("text", &param, WORDS, move |compiler, variable| {
                        compiler.hierarchy_condition(&text, variable, below)
                    }),
                }
            }
            Modifier::Type(kind) => {
                let kind = kind.as_str().to_owned();
                match text {
                    None => Bson::Boolean(false),
                    Some(text) => {
                        self.holds("reference", &param, MAIN, move |compiler, variable| {
                            compiler.typed_reference_condition(&text, &kind, variable)
                        })
                    }
                }
            }
            Modifier::Identifier => match value {
                SearchValue::Token(token) => {
                    let token = token.clone();
                    self.holds("token", &param, IDENTIFIER, move |compiler, variable| {
                        compiler.token_condition(&token, variable)
                    })
                }
                _ => Bson::Boolean(false),
            },
            Modifier::OfType => self.of_type(value, &param),
            Modifier::Missing => Bson::Boolean(false),
            Modifier::None | Modifier::Not | Modifier::In | Modifier::NotIn => {
                self.declared(filter, value)?
            }
        })
    }

    fn full_text(&mut self, value: &SearchValue, param: &str) -> Bson {
        let SearchValue::Text(raw) = value else {
            return Bson::Boolean(false);
        };
        let Ok(query) = fhir_core::search::text::text_query(raw) else {
            return Bson::Boolean(false);
        };
        self.text_expression(&query.expr, param)
    }

    fn text_expression(&mut self, expr: &fhir_core::search::text::Expr, param: &str) -> Bson {
        use fhir_core::search::text::Expr;
        match expr {
            Expr::Term(term) => {
                let term = term.clone();
                let param = param.to_owned();
                self.holds("text", &param, NARRATIVE, move |_, variable| {
                    matching(
                        field(variable, "folded"),
                        format!("(^| ){}($| )", escaped(&term)),
                    )
                })
            }
            Expr::All(parts) => all(parts
                .iter()
                .map(|part| self.text_expression(part, param))
                .collect()),
            Expr::Any(parts) => any(parts
                .iter()
                .map(|part| self.text_expression(part, param))
                .collect()),
        }
    }

    fn scalar(&mut self, filter: &Filter, value: &SearchValue, held: Bson) -> Bson {
        let text = wanted(value).map(str::to_owned);
        match (&filter.modifier, text) {
            (_, None) => Bson::Boolean(false),
            (Modifier::Exact, Some(text)) => equals(held, Bson::String(text)),
            (Modifier::Contains | Modifier::Text, Some(text)) => {
                let folded = Bson::Document(doc! {"$toLower": held});
                matching(folded, escaped(&text.to_lowercase()))
            }
            (Modifier::Below, Some(text)) => any(vec![
                equals(held.clone(), Bson::String(text.clone())),
                matching(held, format!("^{}[./]", escaped(&text))),
            ]),
            (Modifier::Above, Some(text)) => Bson::Document(doc! {"$in": [held, prefixes(&text)]}),
            (_, Some(text)) => match value {
                SearchValue::Token(token) => match token.system {
                    TokenSystem::Exact(_) => Bson::Boolean(false),
                    TokenSystem::Any | TokenSystem::Absent => equals(held, Bson::String(text)),
                },
                SearchValue::Text(_) => {
                    let folded = Bson::Document(doc! {"$toLower": held});
                    matching(folded, format!("^{}", escaped(&text.to_lowercase())))
                }
                _ => equals(held, Bson::String(text)),
            },
        }
    }

    fn hit(&mut self, filter: &Filter, value: &SearchValue) -> Result<Bson, Error> {
        match &filter.target {
            Target::Id => Ok(self.scalar(filter, value, Bson::String("$resource_id".to_owned()))),
            Target::LastUpdated => Ok(match value {
                SearchValue::Date { comparator, period } => self.date_condition(
                    *comparator,
                    period,
                    Bson::String("$updated_key".to_owned()),
                    Bson::String("$updated_key".to_owned()),
                ),
                other => self.scalar(filter, other, Bson::String("$last_updated".to_owned())),
            }),
            Target::Path(_) => self.qualified(filter, value),
            Target::Collection => Ok(Bson::Boolean(false)),
            Target::Composite(definition) => {
                let Some((left, right)) = value.components() else {
                    return Ok(Bson::Boolean(false));
                };
                let param = param_key(filter);
                let left_array = array_of(definition.left.value_type)?;
                let right_array = array_of(definition.right.value_type)?;
                let left_variable = self.variable();
                let right_variable = self.variable();
                let left = Component {
                    array: left_array,
                    condition: self.condition(left, &left_variable),
                    variable: left_variable,
                };
                let right = Component {
                    array: right_array,
                    condition: self.condition(right, &right_variable),
                    variable: right_variable,
                };
                Ok(self.paired(&param, left, right))
            }
        }
    }

    fn presence(&mut self, filter: &Filter) -> Bson {
        let param = param_key(filter);
        self.holds("text", &param, PRESENCE, |_, _| Bson::Boolean(true))
    }

    pub fn filter(&mut self, filter: &Filter) -> Result<Bson, Error> {
        if matches!(filter.modifier, Modifier::Missing) {
            let absent = matches!(filter.values.first(), Some(SearchValue::Missing(true)));
            return Ok(match &filter.target {
                Target::Id | Target::LastUpdated => Bson::Boolean(!absent),
                _ => {
                    let present = self.presence(filter);
                    match absent {
                        true => negate(present),
                        false => present,
                    }
                }
            });
        }
        let mut parts = Vec::new();
        for value in &filter.values {
            let hit = self.hit(filter, value)?;
            parts.push(
                match filter.modifier.is_exclusive() || !value.is_negated() {
                    true => hit,
                    false => negate(hit),
                },
            );
        }
        let found = any(parts);
        Ok(match filter.modifier.is_exclusive() {
            true => negate(found),
            false => found,
        })
    }

    pub fn compartment(&mut self, compartment: &Compartment) -> Bson {
        let Some(definition) =
            fhir_core::search::compartment::definition(compartment.kind.as_str())
        else {
            return Bson::Boolean(false);
        };
        let root = format!("{}/{}", compartment.kind.as_str(), compartment.id.as_str());
        let mut parts = Vec::new();
        for member in definition.members {
            let owner = equals(
                Bson::String("$resource_type".to_owned()),
                Bson::String(member.resource_type.to_owned()),
            );
            let mut links = Vec::new();
            if member.root {
                links.push(equals(
                    Bson::String("$resource_id".to_owned()),
                    Bson::String(compartment.id.as_str().to_owned()),
                ));
            }
            for name in member.params {
                let full = root.clone();
                let bare = compartment.id.as_str().to_owned();
                links.push(self.holds("reference", name, MAIN, move |_, variable| {
                    any(vec![
                        equals(field(variable, "pointer"), Bson::String(full)),
                        equals(field(variable, "logical"), Bson::String(bare)),
                    ])
                }));
            }
            parts.push(all(vec![owner, any(links)]));
        }
        any(parts)
    }

    pub fn grant(&mut self, grant: &Grant) -> Result<Bson, Error> {
        let mut parts = Vec::new();
        for held in &grant.filters {
            let narrowed = self.filter(&held.filter)?;
            parts.push(any(vec![
                negate(equals(
                    Bson::String("$resource_type".to_owned()),
                    Bson::String(held.resource_type.as_str().to_owned()),
                )),
                narrowed,
            ]));
        }
        if !grant.types.is_empty() {
            let names: Vec<Bson> = grant
                .types
                .iter()
                .map(|kind| Bson::String(kind.as_str().to_owned()))
                .collect();
            parts.push(Bson::Document(doc! {"$in": ["$resource_type", names]}));
        }
        if !grant.is_open() {
            let reached = grant
                .compartments
                .iter()
                .map(|compartment| self.compartment(compartment))
                .collect();
            parts.push(any(reached));
        }
        Ok(all(parts))
    }

    pub fn criterion(
        &mut self,
        criterion: &Criterion,
        stages: &mut Vec<Document>,
        grant: Option<&Grant>,
    ) -> Result<Bson, Error> {
        match criterion {
            Criterion::Direct(filter) => self.filter(filter),
            Criterion::Linked(chain) => self.chain(chain, stages, grant),
        }
    }

    fn chain(
        &mut self,
        chain: &Chain,
        stages: &mut Vec<Document>,
        grant: Option<&Grant>,
    ) -> Result<Bson, Error> {
        let alias = self.alias();
        let mut inner_stages: Vec<Document> = Vec::new();
        let inner = self.criterion(&chain.next, &mut inner_stages, grant)?;
        let allowed = match grant {
            None => Bson::Boolean(true),
            Some(grant) => self.grant(grant)?,
        };
        let kinds = match chain.types.is_empty() {
            true => Bson::Boolean(true),
            false => {
                let names: Vec<Bson> = chain
                    .types
                    .iter()
                    .map(|kind| Bson::String(kind.as_str().to_owned()))
                    .collect();
                Bson::Document(doc! {"$in": ["$resource_type", names]})
            }
        };
        let link = self.variable();
        let links = |name: &str, variable: &str| {
            Bson::Document(doc! {"$filter": {
                "input": {"$ifNull": ["$reference", []]},
                "as": variable,
                "cond": all(vec![
                    equals(field(variable, "param"), Bson::String(name.to_owned())),
                    equals(field(variable, "slot"), Bson::String(MAIN.to_owned())),
                ]),
            }})
        };
        let (variables, membership) = match chain.direction {
            ChainDirection::Forward => {
                let held = links(&chain.name, &link);
                let reached = Bson::Document(doc! {"$anyElementTrue": {"$map": {
                    "input": "$$links",
                    "as": &link,
                    "in": any(vec![
                        equals(
                            field(&link, "pointer"),
                            Bson::Document(doc! {"$concat": ["$resource_type", "/", "$resource_id"]}),
                        ),
                        equals(field(&link, "logical"), Bson::String("$resource_id".to_owned())),
                    ]),
                }}});
                (doc! {"links": held}, reached)
            }
            ChainDirection::Reverse => {
                let held = links(&chain.name, &link);
                let reached = Bson::Document(doc! {"$anyElementTrue": {"$map": {
                    "input": held,
                    "as": &link,
                    "in": any(vec![
                        equals(
                            field(&link, "pointer"),
                            Bson::Document(doc! {"$concat": ["$$owner_type", "/", "$$owner_id"]}),
                        ),
                        equals(field(&link, "logical"), Bson::String("$$owner_id".to_owned())),
                    ]),
                }}});
                (
                    doc! {"owner_id": "$resource_id", "owner_type": "$resource_type"},
                    reached,
                )
            }
        };
        let condition = all(vec![
            Bson::String("$is_current".to_owned()),
            negate(Bson::String("$is_deleted".to_owned())),
            kinds,
            allowed,
            inner,
            membership,
        ]);
        inner_stages.push(doc! {"$match": {"$expr": condition}});
        inner_stages.push(doc! {"$limit": 1});
        inner_stages.push(doc! {"$project": {"_id": 1}});
        stages.push(doc! {"$lookup": {
            "from": RESOURCES,
            "let": variables,
            "pipeline": inner_stages,
            "as": &alias,
        }});
        Ok(Bson::Document(
            doc! {"$gt": [{"$size": {"$ifNull": [format!("${alias}"), []]}}, 0]},
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::search::lookup;
    use fhir_core::ResourceType;
    use std::sync::Arc;

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("a known type")
    }

    fn def(resource_type: &str, name: &str) -> Arc<fhir_core::search::ParamDef> {
        lookup(Some(kind(resource_type)), name).expect("a built-in parameter")
    }

    fn built(resource_type: &str, name: &str, modifier: Modifier, raw: &str) -> Filter {
        let def = def(resource_type, name);
        let values = raw
            .split(',')
            .map(|part| def.value_with(&modifier, part).expect("a valid value"))
            .collect();
        Filter {
            modifier,
            ..Filter::new(name, def.target.clone(), values)
        }
    }

    fn compiled(filter: &Filter) -> String {
        let mut compiler = Compiler::new();
        let condition = compiler.filter(filter).expect("the filter compiles");
        condition.to_string()
    }

    #[test]
    fn a_coded_value_selects_on_the_code_and_its_system() {
        let text = compiled(&built("Observation", "code", Modifier::None, "urn:s|c1"));
        assert!(text.contains("$token"), "{text}");
        assert!(text.contains("system"), "{text}");
        assert!(text.contains("tail"), "{text}");
    }

    #[test]
    fn a_text_search_matches_whole_words_in_the_narrative() {
        let def = def("Patient", "_text");
        let filter = Filter {
            modifier: Modifier::None,
            ..Filter::new(
                "_text",
                def.target.clone(),
                vec![
                    SearchValue::parse(ValueType::String, "(bone OR liver) AND metastases")
                        .expect("a valid value"),
                ],
            )
        };
        let text = compiled(&filter);
        assert!(text.contains("\"$text\""), "{text}");
        assert!(text.contains("\"_text\""), "{text}");
        assert!(text.contains("\"narrative\""), "{text}");
        assert!(text.contains("$regexMatch"), "{text}");
        assert!(text.contains("(^| )metastases($| )"), "{text}");
        assert!(text.contains("$or"), "{text}");
        assert!(text.contains("\"$and\""), "{text}");
    }

    #[test]
    fn a_code_without_a_system_places_no_bound_on_the_system() {
        let bare = compiled(&built("Observation", "code", Modifier::None, "c1"));
        assert!(!bare.contains("\"system\""), "{bare}");
        let absent = compiled(&built("Observation", "code", Modifier::None, "|c1"));
        assert!(absent.contains("system"), "{absent}");
    }

    #[test]
    fn every_comparator_of_a_span_reaches_the_condition() {
        for raw in [
            "1980", "gt1980", "lt1980", "ge1980", "le1980", "sa1980", "eb1980", "ap1980",
        ] {
            let text = compiled(&built("Patient", "birthdate", Modifier::None, raw));
            assert!(text.contains("$date"), "{raw}: {text}");
        }
    }

    #[test]
    fn every_comparator_of_a_decimal_reaches_the_condition() {
        for raw in [
            "4.5", "gt4.5", "lt4.5", "ge4.5", "le4.5", "sa4.5", "eb4.5", "ap4.5",
        ] {
            let text = compiled(&built("Observation", "value-quantity", Modifier::None, raw));
            assert!(text.contains("$quantity"), "{raw}: {text}");
        }
    }

    #[test]
    fn a_qualifier_chooses_the_projection_it_reads() {
        for (resource_type, name, modifier, slot) in [
            ("Patient", "name", Modifier::Exact, PLAIN),
            ("Patient", "name", Modifier::Contains, PLAIN),
            ("Observation", "code", Modifier::Text, NARRATIVE),
            ("Observation", "code", Modifier::Below, WORDS),
            ("Observation", "code", Modifier::Above, WORDS),
        ] {
            let text = compiled(&built(resource_type, name, modifier.clone(), "Stone"));
            assert!(
                text.contains(slot),
                "{modifier:?} should read {slot}: {text}"
            );
        }
    }

    #[test]
    fn an_absent_value_is_asked_for_as_the_lack_of_one() {
        let absent = compiled(&built("Patient", "name", Modifier::Missing, "true"));
        assert!(absent.contains("$not"), "{absent}");
        let present = compiled(&built("Patient", "name", Modifier::Missing, "false"));
        assert!(!present.contains("$not"), "{present}");
        assert!(present.contains(PRESENCE), "{present}");
    }

    #[test]
    fn an_exclusive_qualifier_refuses_every_alternative_at_once() {
        let text = compiled(&built("Observation", "code", Modifier::Not, "c1,c2"));
        assert!(text.starts_with("{ \"$not\""), "{text}");
    }

    #[test]
    fn selecting_on_identity_reads_the_document_and_no_index() {
        let text = compiled(&built("Patient", "_id", Modifier::None, "p1"));
        assert!(text.contains("$resource_id"), "{text}");
        assert!(!text.contains("$filter"), "{text}");
    }

    #[test]
    fn selecting_on_the_write_time_reads_the_key_it_is_stored_under() {
        let text = compiled(&built("Patient", "_lastUpdated", Modifier::None, "gt2020"));
        assert!(text.contains("$updated_key"), "{text}");
    }

    #[test]
    fn a_pointer_written_with_a_type_is_matched_whole() {
        let full = compiled(&built(
            "Observation",
            "subject",
            Modifier::None,
            "Patient/p1",
        ));
        assert!(full.contains("pointer"), "{full}");
        assert!(!full.contains("logical"), "{full}");
        let bare = compiled(&built("Observation", "subject", Modifier::None, "p1"));
        assert!(bare.contains("logical"), "{bare}");
    }

    #[test]
    fn a_composite_pairs_its_components_on_one_element() {
        let text = compiled(&built(
            "Observation",
            "code-value-quantity",
            Modifier::None,
            "urn:s|c1$4.5",
        ));
        assert!(text.contains("$token"), "{text}");
        assert!(text.contains("$quantity"), "{text}");
        assert!(text.contains("ordinal"), "{text}");
        assert!(text.contains(LEFT) && text.contains(RIGHT), "{text}");
    }

    #[test]
    fn a_grant_confines_the_types_and_the_compartments_it_names() {
        let mut compiler = Compiler::new();
        let grant = Grant {
            types: vec![kind("Observation")],
            compartments: vec![Compartment {
                kind: kind("Patient"),
                id: fhir_core::ResourceId::parse("p1").unwrap(),
            }],
            filters: Vec::new(),
        };
        let text = compiler
            .grant(&grant)
            .expect("the grant compiles")
            .to_string();
        assert!(text.contains("$resource_type"), "{text}");
        assert!(text.contains("$reference"), "{text}");
    }

    #[test]
    fn an_unknown_compartment_admits_nothing() {
        let mut compiler = Compiler::new();
        let text = compiler.compartment(&Compartment {
            kind: kind("Observation"),
            id: fhir_core::ResourceId::parse("o1").unwrap(),
        });
        assert_eq!(text, Bson::Boolean(false));
    }

    #[test]
    fn a_chain_becomes_a_join_the_engine_runs() {
        let mut compiler = Compiler::new();
        let mut stages = Vec::new();
        let chain = Chain {
            name: "subject".to_owned(),
            target: def("Observation", "subject").target.clone(),
            types: vec![kind("Patient")],
            direction: ChainDirection::Forward,
            next: Box::new(Criterion::Direct(built(
                "Patient",
                "name",
                Modifier::None,
                "Stone",
            ))),
        };
        let condition = compiler
            .criterion(&Criterion::Linked(chain), &mut stages, None)
            .expect("the chain compiles");
        assert_eq!(stages.len(), 1);
        assert!(stages[0].contains_key("$lookup"), "{:?}", stages[0]);
        assert!(condition.to_string().contains("linked1"));
    }

    #[test]
    fn a_hierarchical_value_walks_the_separators_of_the_value() {
        let below = compiled(&built("Observation", "code", Modifier::Below, "a.b"));
        assert!(below.contains("$regexMatch"), "{below}");
        let above = compiled(&built("Observation", "code", Modifier::Above, "a.b"));
        assert!(above.contains("$in"), "{above}");
        assert_eq!(prefixes("a.b").len(), 2);
        assert_eq!(prefixes("a").len(), 1);
    }

    #[test]
    fn a_value_carrying_a_pattern_is_matched_as_written() {
        assert_eq!(escaped("a.b"), "a\\.b");
        assert_eq!(escaped("a+b"), "a\\+b");
        assert_eq!(escaped("ab"), "ab");
    }

    #[test]
    fn every_value_type_names_the_array_it_is_indexed_in() {
        assert_eq!(array_of(ValueType::Token).unwrap(), "token");
        assert_eq!(array_of(ValueType::String).unwrap(), "text");
        assert_eq!(array_of(ValueType::Uri).unwrap(), "uri");
        assert!(array_of(ValueType::Composite).is_err());
        let filter = built("Observation", "code", Modifier::None, "c1");
        assert_eq!(array_for(&filter), Some("token"));
    }

    #[test]
    fn nothing_and_everything_are_conditions_too() {
        assert_eq!(all(Vec::new()), Bson::Boolean(true));
        assert_eq!(any(Vec::new()), Bson::Boolean(false));
        assert_eq!(all(vec![Bson::Boolean(true)]), Bson::Boolean(true));
        assert_eq!(any(vec![Bson::Boolean(true)]), Bson::Boolean(true));
    }
}
