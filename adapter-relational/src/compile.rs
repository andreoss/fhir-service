use crate::extract::{IDENTIFIER, MAIN, NARRATIVE, OF_TYPE, PLAIN, PRESENCE, WORDS};
use crate::store::RelationalStore;
use fhir_core::search::Comparator;
use fhir_core::search::{
    Chain, ChainDirection, Compartment, Criterion, Filter, Grant, IndexKey, Modifier, SearchValue,
    Target, Token, TokenSystem, ValueType,
};
use fhir_core::{Error, InstantPeriod};

const DAY: i64 = 86_400;

#[derive(Debug, Clone, PartialEq)]
pub enum Bind {
    Text(String),
    Nullable(Option<String>),
    Int(i32),
    Big(i64),
    Real(f64),
    Texts(Vec<String>),
}

pub struct Compiler<'a> {
    store: &'a RelationalStore,
    binds: Vec<Bind>,
    aliases: usize,
}

fn or_of(parts: Vec<String>) -> String {
    match parts.is_empty() {
        true => "false".to_owned(),
        false => format!("({})", parts.join(" or ")),
    }
}

fn and_of(parts: Vec<String>) -> String {
    match parts.is_empty() {
        true => "true".to_owned(),
        false => format!("({})", parts.join(" and ")),
    }
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

impl<'a> Compiler<'a> {
    pub fn new(store: &'a RelationalStore) -> Compiler<'a> {
        Compiler {
            store,
            binds: Vec::new(),
            aliases: 0,
        }
    }

    pub fn binds(&self) -> &[Bind] {
        &self.binds
    }

    pub fn into_binds(self) -> Vec<Bind> {
        self.binds
    }

    pub fn place(&mut self, bind: Bind) -> String {
        self.binds.push(bind);
        format!("${}", self.binds.len())
    }

    fn text(&mut self, value: &str) -> String {
        self.place(Bind::Text(value.to_owned()))
    }

    fn texts(&mut self, values: Vec<String>) -> String {
        self.place(Bind::Texts(values))
    }

    fn alias(&mut self) -> String {
        self.aliases += 1;
        format!("x{}", self.aliases)
    }

    fn exists(
        &mut self,
        table: &str,
        outer: &str,
        param: &str,
        slot: &str,
        build: impl FnOnce(&mut Compiler<'a>, &str) -> String,
    ) -> String {
        let inner = self.alias();
        let key = self.text(param);
        let slot = self.text(slot);
        let condition = build(self, &inner);
        format!(
            "exists (select 1 from {} {inner} where {inner}.surrogate_id = {outer}.surrogate_id \
             and {inner}.param = {key} and {inner}.slot = {slot} and ({condition}))",
            self.store.table(table)
        )
    }

    fn token_condition(&mut self, token: &Token, alias: &str) -> String {
        let system = match &token.system {
            TokenSystem::Any => "true".to_owned(),
            TokenSystem::Absent => format!("{alias}.system is null"),
            TokenSystem::Exact(name) => {
                let bound = self.text(name);
                format!("{alias}.system = {bound}")
            }
        };
        let code = match &token.code {
            None => "true".to_owned(),
            Some(text) => {
                let key = IndexKey::of(text);
                let head = self.text(key.key());
                let tail = self.place(Bind::Nullable(key.overflow().map(str::to_owned)));
                format!("{alias}.code = {head} and {alias}.code_tail is not distinct from {tail}")
            }
        };
        and_of(vec![system, code])
    }

    fn number_condition(
        &mut self,
        comparator: Comparator,
        value: f64,
        tolerance: f64,
        column: &str,
    ) -> String {
        let bound = self.place(Bind::Real(value));
        match comparator {
            Comparator::Eq | Comparator::Ne => {
                let span = self.place(Bind::Real(tolerance));
                format!("abs({column} - {bound}) <= {span}")
            }
            Comparator::Gt | Comparator::Sa => format!("{column} > {bound}"),
            Comparator::Lt | Comparator::Eb => format!("{column} < {bound}"),
            Comparator::Ge => format!("{column} >= {bound}"),
            Comparator::Le => format!("{column} <= {bound}"),
            Comparator::Ap => {
                let reach = self.place(Bind::Real(value.abs() * 0.1 + tolerance));
                format!("abs({column} - {bound}) <= {reach}")
            }
        }
    }

    fn bound_key(&mut self, seconds: i64, nanos: u32) -> String {
        let secs = self.place(Bind::Big(seconds));
        let nanos = self.place(Bind::Int(nanos as i32));
        format!("({secs}, {nanos})")
    }

    fn date_condition(
        &mut self,
        comparator: Comparator,
        period: &InstantPeriod,
        low: &str,
        high: &str,
    ) -> String {
        let query_low = self.bound_key(period.low().seconds(), period.low().nanos());
        let query_high = self.bound_key(period.high().seconds(), period.high().nanos());
        match comparator {
            Comparator::Eq | Comparator::Ne => {
                format!("{low} >= {query_low} and {high} <= {query_high}")
            }
            Comparator::Gt => format!("{high} > {query_high}"),
            Comparator::Lt => format!("{low} < {query_low}"),
            Comparator::Ge => format!("{high} >= {query_low}"),
            Comparator::Le => format!("{low} <= {query_high}"),
            Comparator::Sa => format!("{low} > {query_high}"),
            Comparator::Eb => format!("{high} < {query_low}"),
            Comparator::Ap => match period.widened(DAY) {
                None => "false".to_owned(),
                Some(near) => {
                    let near_low = self.bound_key(near.low().seconds(), near.low().nanos());
                    let near_high = self.bound_key(near.high().seconds(), near.high().nanos());
                    format!("{low} <= {near_high} and {high} >= {near_low}")
                }
            },
        }
    }

    fn quantity_condition(
        &mut self,
        number: &SearchValue,
        system: &TokenSystem,
        code: Option<&str>,
        alias: &str,
    ) -> String {
        let measure = match number {
            SearchValue::Number {
                comparator,
                value,
                tolerance,
            } => self.number_condition(*comparator, *value, *tolerance, &format!("{alias}.value")),
            _ => "false".to_owned(),
        };
        let system = match system {
            TokenSystem::Any => "true".to_owned(),
            TokenSystem::Absent => format!("{alias}.system is null and {alias}.structured"),
            TokenSystem::Exact(name) => {
                let bound = self.text(name);
                format!("{alias}.system = {bound}")
            }
        };
        let unit = match code {
            None => "true".to_owned(),
            Some(text) => {
                let bound = self.text(text);
                format!("{alias}.code = {bound}")
            }
        };
        and_of(vec![measure, system, unit])
    }

    fn reference_condition(&mut self, text: &str, alias: &str) -> String {
        let bound = self.text(text);
        match text.contains('/') {
            true => format!("{alias}.ref_full = {bound}"),
            false => format!("({alias}.ref_full = {bound} or {alias}.ref_id = {bound})"),
        }
    }

    fn hierarchy_condition(&mut self, text: &str, alias: &str, below: bool) -> String {
        let bound = self.text(text);
        match below {
            true => format!(
                "({alias}.value = {bound} or (starts_with({alias}.value, {bound}) \
                 and substr({alias}.value, length({bound}) + 1, 1) in ('.', '/')))"
            ),
            false => format!(
                "({alias}.value = {bound} or (starts_with({bound}, {alias}.value) \
                 and substr({bound}, length({alias}.value) + 1, 1) in ('.', '/')))"
            ),
        }
    }

    fn typed_reference_condition(&mut self, text: &str, kind: &str, alias: &str) -> String {
        let bound = self.text(text);
        let wanted_type = self.text(kind);
        format!(
            "{alias}.ref_type = {wanted_type} and ({alias}.ref_id = {bound} \
             or {alias}.ref_full = {bound} \
             or {alias}.ref_type || '/' || {alias}.ref_id = {bound})"
        )
    }

    fn declared(
        &mut self,
        value_type: ValueType,
        value: &SearchValue,
        param: &str,
        outer: &str,
    ) -> Result<String, Error> {
        let table = match value_type {
            ValueType::Token => "index_token",
            ValueType::String => "index_text",
            ValueType::Number => "index_number",
            ValueType::Date => "index_date",
            ValueType::Quantity => "index_quantity",
            ValueType::Reference => "index_reference",
            ValueType::Uri => "index_uri",
            ValueType::Composite => {
                return Err(Error::InvalidParameter(
                    "a composite value needs both of its components".to_owned(),
                ))
            }
        };
        let value = value.clone();
        Ok(self.exists(
            table,
            outer,
            param,
            MAIN,
            move |compiler, alias| match &value {
                SearchValue::Token(token) => compiler.token_condition(token, alias),
                SearchValue::Text(text) => {
                    let bound = compiler.text(&text.to_lowercase());
                    format!("starts_with({alias}.folded, {bound})")
                }
                SearchValue::Number {
                    comparator,
                    value,
                    tolerance,
                } => compiler.number_condition(
                    *comparator,
                    *value,
                    *tolerance,
                    &format!("{alias}.value"),
                ),
                SearchValue::Date { comparator, period } => compiler.date_condition(
                    *comparator,
                    period,
                    &format!("({alias}.low_secs, {alias}.low_nanos)"),
                    &format!("({alias}.high_secs, {alias}.high_nanos)"),
                ),
                SearchValue::Quantity {
                    number,
                    system,
                    code,
                } => compiler.quantity_condition(number, system, code.as_deref(), alias),
                SearchValue::Reference(text) => compiler.reference_condition(text, alias),
                SearchValue::Uri(text) => {
                    let bound = compiler.text(text);
                    format!("{alias}.value = {bound}")
                }
                SearchValue::OfType { .. }
                | SearchValue::Composite { .. }
                | SearchValue::Missing(_) => "false".to_owned(),
            },
        ))
    }

    fn of_type(&mut self, value: &SearchValue, param: &str, outer: &str) -> String {
        let SearchValue::OfType {
            system,
            code,
            value: identifier,
        } = value
        else {
            return "false".to_owned();
        };
        let token = Token {
            system: system.clone(),
            code: code.clone(),
        };
        let identifier = identifier.clone();
        let key = param.to_owned();
        self.exists(
            "index_text",
            outer,
            param,
            OF_TYPE,
            move |compiler, alias| {
                let bound = compiler.text(&identifier);
                let inner = compiler.alias();
                let param = compiler.text(&key);
                let slot = compiler.text(OF_TYPE);
                let qualifier = compiler.token_condition(&token, &inner);
                let table = compiler.store.table("index_token");
                format!(
                "{alias}.value = {bound} and exists (select 1 from {table} {inner} \
                 where {inner}.surrogate_id = {alias}.surrogate_id and {inner}.param = {param} \
                 and {inner}.slot = {slot} and {inner}.ordinal = {alias}.ordinal and ({qualifier}))"
            )
            },
        )
    }

    fn qualified(
        &mut self,
        filter: &Filter,
        value: &SearchValue,
        outer: &str,
    ) -> Result<String, Error> {
        let param = param_key(filter);
        if filter.modifier == Modifier::None && param == "_text" {
            return Ok(self.text_search(value, &param, outer));
        }
        let text = wanted(value).map(str::to_owned);
        Ok(match &filter.modifier {
            Modifier::Exact => match text {
                None => "false".to_owned(),
                Some(text) => self.exists(
                    "index_text",
                    outer,
                    &param,
                    PLAIN,
                    move |compiler, alias| {
                        let bound = compiler.text(&text);
                        format!("{alias}.value = {bound}")
                    },
                ),
            },
            Modifier::Contains => match text {
                None => "false".to_owned(),
                Some(text) => self.exists(
                    "index_text",
                    outer,
                    &param,
                    PLAIN,
                    move |compiler, alias| {
                        let bound = compiler.text(&text.to_lowercase());
                        format!("position({bound} in {alias}.folded) > 0")
                    },
                ),
            },
            Modifier::Text => match text {
                None => "false".to_owned(),
                Some(text) => self.exists(
                    "index_text",
                    outer,
                    &param,
                    NARRATIVE,
                    move |compiler, alias| {
                        let bound = compiler.text(&text.to_lowercase());
                        format!("position({bound} in {alias}.folded) > 0")
                    },
                ),
            },
            Modifier::Below | Modifier::Above => {
                let below = matches!(filter.modifier, Modifier::Below);
                match text {
                    None => "false".to_owned(),
                    Some(text) => self.exists(
                        "index_text",
                        outer,
                        &param,
                        WORDS,
                        move |compiler, alias| compiler.hierarchy_condition(&text, alias, below),
                    ),
                }
            }
            Modifier::Type(kind) => {
                let kind = kind.as_str().to_owned();
                match text {
                    None => "false".to_owned(),
                    Some(text) => self.exists(
                        "index_reference",
                        outer,
                        &param,
                        MAIN,
                        move |compiler, alias| {
                            compiler.typed_reference_condition(&text, &kind, alias)
                        },
                    ),
                }
            }
            Modifier::Identifier => match value {
                SearchValue::Token(token) => {
                    let token = token.clone();
                    self.exists(
                        "index_token",
                        outer,
                        &param,
                        IDENTIFIER,
                        move |compiler, alias| compiler.token_condition(&token, alias),
                    )
                }
                _ => "false".to_owned(),
            },
            Modifier::OfType => self.of_type(value, &param, outer),
            Modifier::Missing => "false".to_owned(),
            Modifier::None | Modifier::Not | Modifier::In | Modifier::NotIn => {
                self.declared(value_type_of(filter), value, &param, outer)?
            }
        })
    }

    fn text_search(&mut self, value: &SearchValue, param: &str, outer: &str) -> String {
        let fhir_core::search::SearchValue::Text(raw) = value else {
            return "false".to_owned();
        };
        let Ok(query) = fhir_core::search::text::text_query(raw) else {
            return "false".to_owned();
        };
        let inner = self.alias();
        let key = self.text(param);
        let slot = self.text(crate::extract::NARRATIVE);
        let condition = self.text_expression(&query.expr, &inner);
        format!(
            "exists (select 1 from {} {inner} where {inner}.surrogate_id = {outer}.surrogate_id \
             and {inner}.param = {key} and {inner}.slot = {slot} and {condition})",
            self.store.table("index_text")
        )
    }

    fn text_expression(&mut self, expr: &fhir_core::search::text::Expr, inner: &str) -> String {
        use fhir_core::search::text::Expr;
        match expr {
            Expr::Term(term) => {
                let bound = self.text(term);
                format!("strpos(' ' || {inner}.folded || ' ', ' ' || {bound} || ' ') > 0")
            }
            Expr::All(parts) => and_of(
                parts
                    .iter()
                    .map(|part| self.text_expression(part, inner))
                    .collect(),
            ),
            Expr::Any(parts) => or_of(
                parts
                    .iter()
                    .map(|part| self.text_expression(part, inner))
                    .collect(),
            ),
        }
    }

    fn scalar(&mut self, filter: &Filter, value: &SearchValue, column: &str) -> String {
        let text = wanted(value).map(str::to_owned);
        match (&filter.modifier, text) {
            (_, None) => "false".to_owned(),
            (Modifier::Exact, Some(text)) => {
                let bound = self.text(&text);
                format!("{column} = {bound}")
            }
            (Modifier::Contains | Modifier::Text, Some(text)) => {
                let bound = self.text(&text.to_lowercase());
                format!("position({bound} in lower({column})) > 0")
            }
            (Modifier::Below, Some(text)) => {
                let bound = self.text(&text);
                format!(
                    "({column} = {bound} or (starts_with({column}, {bound}) \
                     and substr({column}, length({bound}) + 1, 1) in ('.', '/')))"
                )
            }
            (Modifier::Above, Some(text)) => {
                let bound = self.text(&text);
                format!(
                    "({column} = {bound} or (starts_with({bound}, {column}) \
                     and substr({bound}, length({column}) + 1, 1) in ('.', '/')))"
                )
            }
            (_, Some(text)) => match value {
                SearchValue::Token(token) => match token.system {
                    TokenSystem::Exact(_) => "false".to_owned(),
                    TokenSystem::Any | TokenSystem::Absent => {
                        let bound = self.text(&text);
                        format!("{column} = {bound}")
                    }
                },
                SearchValue::Text(_) => {
                    let bound = self.text(&text.to_lowercase());
                    format!("starts_with(lower({column}), {bound})")
                }
                _ => {
                    let bound = self.text(&text);
                    format!("{column} = {bound}")
                }
            },
        }
    }

    fn hit(&mut self, filter: &Filter, value: &SearchValue, outer: &str) -> Result<String, Error> {
        match &filter.target {
            Target::Id => Ok(self.scalar(filter, value, &format!("{outer}.resource_id"))),
            Target::LastUpdated => Ok(match value {
                SearchValue::Date { comparator, period } => self.date_condition(
                    *comparator,
                    period,
                    &format!("({outer}.updated_secs, {outer}.updated_nanos)"),
                    &format!("({outer}.updated_secs, {outer}.updated_nanos)"),
                ),
                other => self.scalar(filter, other, &format!("{outer}.last_updated")),
            }),
            Target::Path(_) => self.qualified(filter, value, outer),
            Target::Collection => Ok("false".to_owned()),
            Target::Composite(definition) => {
                let Some((left, right)) = value.components() else {
                    return Ok("false".to_owned());
                };
                let param = param_key(filter);
                let left_table = table_of(definition.left.value_type)?;
                let right_table = table_of(definition.right.value_type)?;
                let outer_alias = self.alias();
                let inner_alias = self.alias();
                let left_key = self.text(&param);
                let left_slot = self.text(crate::extract::LEFT);
                let left_condition = self.condition(left, definition.left.value_type, &outer_alias);
                let right_key = self.text(&param);
                let right_slot = self.text(crate::extract::RIGHT);
                let right_condition =
                    self.condition(right, definition.right.value_type, &inner_alias);
                let left_table = self.store.table(left_table);
                let right_table = self.store.table(right_table);
                Ok(format!(
                    "exists (select 1 from {left_table} {outer_alias} \
                     where {outer_alias}.surrogate_id = {outer}.surrogate_id \
                     and {outer_alias}.param = {left_key} and {outer_alias}.slot = {left_slot} \
                     and ({left_condition}) and exists (select 1 from {right_table} {inner_alias} \
                     where {inner_alias}.surrogate_id = {outer}.surrogate_id \
                     and {inner_alias}.param = {right_key} and {inner_alias}.slot = {right_slot} \
                     and {inner_alias}.ordinal = {outer_alias}.ordinal and ({right_condition})))"
                ))
            }
        }
    }

    pub fn value_condition(&mut self, value: &SearchValue, alias: &str) -> String {
        self.condition(value, ValueType::Token, alias)
    }

    fn condition(&mut self, value: &SearchValue, value_type: ValueType, alias: &str) -> String {
        match value {
            SearchValue::Token(token) => self.token_condition(token, alias),
            SearchValue::Text(text) => {
                let bound = self.text(&text.to_lowercase());
                format!("starts_with({alias}.folded, {bound})")
            }
            SearchValue::Number {
                comparator,
                value,
                tolerance,
            } => self.number_condition(*comparator, *value, *tolerance, &format!("{alias}.value")),
            SearchValue::Date { comparator, period } => self.date_condition(
                *comparator,
                period,
                &format!("({alias}.low_secs, {alias}.low_nanos)"),
                &format!("({alias}.high_secs, {alias}.high_nanos)"),
            ),
            SearchValue::Quantity {
                number,
                system,
                code,
            } => self.quantity_condition(number, system, code.as_deref(), alias),
            SearchValue::Reference(text) => self.reference_condition(text, alias),
            SearchValue::Uri(text) => {
                let bound = self.text(text);
                format!("{alias}.value = {bound}")
            }
            SearchValue::OfType { .. }
            | SearchValue::Composite { .. }
            | SearchValue::Missing(_) => {
                let _ = value_type;
                "false".to_owned()
            }
        }
    }

    fn presence(&mut self, filter: &Filter, outer: &str) -> String {
        let param = param_key(filter);
        self.exists("index_text", outer, &param, PRESENCE, |_, _| {
            "true".to_owned()
        })
    }

    pub fn filter(&mut self, filter: &Filter, outer: &str) -> Result<String, Error> {
        if matches!(filter.modifier, Modifier::Missing) {
            let wanted = matches!(filter.values.first(), Some(SearchValue::Missing(true)));
            return Ok(match &filter.target {
                Target::Id | Target::LastUpdated => match wanted {
                    true => "false".to_owned(),
                    false => "true".to_owned(),
                },
                _ => {
                    let present = self.presence(filter, outer);
                    match wanted {
                        true => format!("not {present}"),
                        false => present,
                    }
                }
            });
        }
        let mut parts = Vec::new();
        for value in &filter.values {
            let hit = self.hit(filter, value, outer)?;
            parts.push(
                match filter.modifier.is_exclusive() || !value.is_negated() {
                    true => hit,
                    false => format!("not ({hit})"),
                },
            );
        }
        let any = or_of(parts);
        Ok(match filter.modifier.is_exclusive() {
            true => format!("not {any}"),
            false => any,
        })
    }

    pub fn criterion(
        &mut self,
        criterion: &Criterion,
        outer: &str,
        grant: Option<&Grant>,
    ) -> Result<String, Error> {
        match criterion {
            Criterion::Direct(filter) => self.filter(filter, outer),
            Criterion::Linked(chain) => self.chain(chain, outer, grant),
        }
    }

    fn chain(
        &mut self,
        chain: &Chain,
        outer: &str,
        grant: Option<&Grant>,
    ) -> Result<String, Error> {
        let far = self.alias();
        let link = self.alias();
        let inner = self.criterion(&chain.next, &far, grant)?;
        let allowed = match grant {
            None => "true".to_owned(),
            Some(grant) => self.grant(grant, &far)?,
        };
        let kinds = match chain.types.is_empty() {
            true => "true".to_owned(),
            false => {
                let names = chain
                    .types
                    .iter()
                    .map(|kind| kind.as_str().to_owned())
                    .collect();
                let bound = self.texts(names);
                format!("{far}.resource_type = any({bound})")
            }
        };
        let name = self.text(&chain.name);
        let slot = self.text(MAIN);
        let resource = self.store.table("resource");
        let references = self.store.table("index_reference");
        let (owner, pointed) = match chain.direction {
            ChainDirection::Forward => (outer.to_owned(), far.clone()),
            ChainDirection::Reverse => (far.clone(), outer.to_owned()),
        };
        Ok(format!(
            "exists (select 1 from {resource} {far} join {references} {link} \
             on {link}.surrogate_id = {owner}.surrogate_id and {link}.param = {name} \
             and {link}.slot = {slot} \
             where {far}.is_current and not {far}.is_deleted and {kinds} and {allowed} \
             and ({inner}) and ({link}.ref_full = {pointed}.resource_type || '/' || \
             {pointed}.resource_id or {link}.ref_id = {pointed}.resource_id))"
        ))
    }

    pub fn compartment(&mut self, compartment: &Compartment, outer: &str) -> String {
        let Some(definition) =
            fhir_core::search::compartment::definition(compartment.kind.as_str())
        else {
            return "false".to_owned();
        };
        let root = format!("{}/{}", compartment.kind.as_str(), compartment.id.as_str());
        let mut parts = Vec::new();
        for member in definition.members {
            let kind = self.text(member.resource_type);
            let owner = format!("{outer}.resource_type = {kind}");
            let mut links = Vec::new();
            if member.root {
                let id = self.text(compartment.id.as_str());
                links.push(format!("{outer}.resource_id = {id}"));
            }
            for name in member.params {
                let full = root.clone();
                let bare = compartment.id.as_str().to_owned();
                links.push(self.exists(
                    "index_reference",
                    outer,
                    name,
                    MAIN,
                    move |compiler, alias| {
                        let full = compiler.text(&full);
                        let bare = compiler.text(&bare);
                        format!("{alias}.ref_full = {full} or {alias}.ref_id = {bare}")
                    },
                ));
            }
            parts.push(format!("({owner} and {})", or_of(links)));
        }
        or_of(parts)
    }

    pub fn grant(&mut self, grant: &Grant, outer: &str) -> Result<String, Error> {
        let mut parts = Vec::new();
        for held in &grant.filters {
            let kind = self.text(held.resource_type.as_str());
            let narrowed = self.filter(&held.filter, outer)?;
            parts.push(format!("({outer}.resource_type <> {kind} or ({narrowed}))"));
        }
        if !grant.types.is_empty() {
            let names = grant
                .types
                .iter()
                .map(|kind| kind.as_str().to_owned())
                .collect();
            let bound = self.texts(names);
            parts.push(format!("{outer}.resource_type = any({bound})"));
        }
        if !grant.is_open() {
            let reached = grant
                .compartments
                .iter()
                .map(|compartment| self.compartment(compartment, outer))
                .collect();
            parts.push(or_of(reached));
        }
        Ok(and_of(parts))
    }
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

fn table_of(value_type: ValueType) -> Result<&'static str, Error> {
    match value_type {
        ValueType::Token => Ok("index_token"),
        ValueType::String => Ok("index_text"),
        ValueType::Number => Ok("index_number"),
        ValueType::Date => Ok("index_date"),
        ValueType::Quantity => Ok("index_quantity"),
        ValueType::Reference => Ok("index_reference"),
        ValueType::Uri => Ok("index_uri"),
        ValueType::Composite => Err(Error::UnsupportedParameter(
            "a composite cannot be a component of a composite".to_owned(),
        )),
    }
}

pub fn table_for(filter: &Filter) -> Option<&'static str> {
    table_of(value_type_of(filter)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::search::{lookup, ParamDef, SearchValue};
    use fhir_core::ResourceType;
    use fhir_store::Namespace;
    use std::sync::Arc;

    fn store() -> RelationalStore {
        RelationalStore::connect_later("postgres://", Namespace::default())
    }

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("a known type")
    }

    fn def(resource_type: &str, name: &str) -> Arc<ParamDef> {
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

    fn sql(filter: &Filter) -> (String, usize) {
        let store = store();
        let mut compiler = Compiler::new(&store);
        let text = compiler.filter(filter, "r").expect("the filter compiles");
        (text, compiler.binds().len())
    }

    #[tokio::test]
    async fn a_coded_value_selects_on_the_code_and_its_system() {
        let (text, binds) = sql(&built("Observation", "code", Modifier::None, "urn:s|c1"));
        assert!(text.contains("index_token"), "{text}");
        assert!(text.contains(".system = $"), "{text}");
        assert!(text.contains(".code = $"), "{text}");
        assert!(text.contains("code_tail is not distinct from"), "{text}");
        assert!(binds >= 5);
    }

    #[tokio::test]
    async fn a_text_search_looks_for_whole_words_in_the_narrative() {
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
        let (text, binds) = sql(&filter);
        assert!(text.contains("index_text"), "{text}");
        assert!(text.contains("x1.param = $1"), "{text}");
        assert!(text.contains("x1.slot = $2"), "{text}");
        assert!(
            text.contains("strpos(' ' || x1.folded || ' ', ' ' || $3 || ' ') > 0"),
            "{text}"
        );
        assert!(text.contains(" or "), "{text}");
        assert!(text.contains("and strpos(' '"), "{text}");
        assert!(binds >= 3, "{binds}");
    }

    #[tokio::test]
    async fn a_code_without_a_system_places_no_bound_on_the_system() {
        let (text, _) = sql(&built("Observation", "code", Modifier::None, "c1"));
        assert!(!text.contains(".system = $"), "{text}");
        let (absent, _) = sql(&built("Observation", "code", Modifier::None, "|c1"));
        assert!(absent.contains(".system is null"), "{absent}");
    }

    #[tokio::test]
    async fn every_comparator_of_a_decimal_reaches_the_statement() {
        for (raw, fragment) in [
            ("4.5", "abs("),
            ("gt4.5", "> $"),
            ("lt4.5", "< $"),
            ("ge4.5", ">= $"),
            ("le4.5", "<= $"),
            ("sa4.5", "> $"),
            ("eb4.5", "< $"),
            ("ap4.5", "abs("),
        ] {
            let (text, _) = sql(&built("Observation", "value-quantity", Modifier::None, raw));
            assert!(text.contains(fragment), "{raw}: {text}");
            assert!(text.contains("index_quantity"), "{raw}: {text}");
        }
    }

    #[tokio::test]
    async fn every_comparator_of_a_span_reaches_the_statement() {
        for raw in [
            "1980", "gt1980", "lt1980", "ge1980", "le1980", "sa1980", "eb1980", "ap1980",
        ] {
            let (text, binds) = sql(&built("Patient", "birthdate", Modifier::None, raw));
            assert!(text.contains("index_date"), "{raw}: {text}");
            assert!(binds >= 4, "{raw}: {binds}");
        }
    }

    #[tokio::test]
    async fn a_qualifier_chooses_the_projection_it_reads() {
        for (resource_type, name, modifier, slot) in [
            ("Patient", "name", Modifier::Exact, PLAIN),
            ("Patient", "name", Modifier::Contains, PLAIN),
            ("Observation", "code", Modifier::Text, NARRATIVE),
            ("Observation", "code", Modifier::Below, WORDS),
            ("Observation", "code", Modifier::Above, WORDS),
        ] {
            let filter = built(resource_type, name, modifier.clone(), "Stone");
            let store = store();
            let mut compiler = Compiler::new(&store);
            let text = compiler.filter(&filter, "r").expect("the filter compiles");
            assert!(text.contains("index_text"), "{modifier:?}: {text}");
            assert!(
                compiler.binds().contains(&Bind::Text(slot.to_owned())),
                "{modifier:?} should read {slot}"
            );
        }
    }

    #[tokio::test]
    async fn a_hierarchy_qualifier_walks_the_value_in_one_direction() {
        let (below, _) = sql(&built("Observation", "code", Modifier::Below, "a.b"));
        assert!(below.contains("starts_with(x1.value, $"), "{below}");
        let (above, _) = sql(&built("Observation", "code", Modifier::Above, "a.b"));
        assert!(above.contains("starts_with($"), "{above}");
    }

    #[tokio::test]
    async fn a_typed_pointer_selects_on_the_type_the_pointer_names() {
        let filter = built(
            "Observation",
            "subject",
            Modifier::Type(kind("Patient")),
            "p1",
        );
        let (text, _) = sql(&filter);
        assert!(text.contains("index_reference"), "{text}");
        assert!(text.contains(".ref_type = $"), "{text}");
    }

    #[tokio::test]
    async fn an_absent_value_is_asked_for_as_the_lack_of_one() {
        let (missing, _) = sql(&built("Patient", "name", Modifier::Missing, "true"));
        assert!(missing.starts_with("not exists"), "{missing}");
        let (present, _) = sql(&built("Patient", "name", Modifier::Missing, "false"));
        assert!(present.starts_with("exists"), "{present}");
    }

    #[tokio::test]
    async fn an_exclusive_qualifier_refuses_every_alternative_at_once() {
        let (text, _) = sql(&built("Observation", "code", Modifier::Not, "c1,c2"));
        assert!(text.starts_with("not ("), "{text}");
        assert_eq!(text.matches("exists").count(), 2, "{text}");
    }

    #[tokio::test]
    async fn selecting_on_identity_needs_no_index_at_all() {
        let (text, _) = sql(&built("Patient", "_id", Modifier::None, "p1"));
        assert!(text.contains("r.resource_id = $"), "{text}");
        assert!(!text.contains("exists"), "{text}");
    }

    #[tokio::test]
    async fn selecting_on_the_write_time_reads_the_row_it_is_stored_on() {
        let (text, _) = sql(&built("Patient", "_lastUpdated", Modifier::None, "gt2020"));
        assert!(text.contains("r.updated_secs"), "{text}");
        assert!(!text.contains("index_date"), "{text}");
    }

    #[tokio::test]
    async fn a_pointer_written_with_a_type_is_matched_whole() {
        let (full, _) = sql(&built(
            "Observation",
            "subject",
            Modifier::None,
            "Patient/p1",
        ));
        assert!(full.contains(".ref_full = $"), "{full}");
        assert!(!full.contains(".ref_id = $"), "{full}");
        let (bare, _) = sql(&built("Observation", "subject", Modifier::None, "p1"));
        assert!(bare.contains(".ref_id = $"), "{bare}");
    }

    #[tokio::test]
    async fn a_grant_confines_the_types_and_the_compartments_it_names() {
        let store = store();
        let mut compiler = Compiler::new(&store);
        let grant = Grant {
            types: vec![kind("Observation")],
            compartments: vec![Compartment {
                kind: kind("Patient"),
                id: fhir_core::ResourceId::parse("p1").unwrap(),
            }],
            filters: Vec::new(),
        };
        let text = compiler.grant(&grant, "r").expect("the grant compiles");
        assert!(text.contains("r.resource_type = any($"), "{text}");
        assert!(text.contains("index_reference"), "{text}");
    }

    #[tokio::test]
    async fn an_open_grant_places_no_bound_on_the_compartment() {
        let store = store();
        let mut compiler = Compiler::new(&store);
        let grant = Grant {
            types: vec![kind("Patient")],
            compartments: Vec::new(),
            filters: Vec::new(),
        };
        let text = compiler.grant(&grant, "r").expect("the grant compiles");
        assert!(!text.contains("index_reference"), "{text}");
    }

    #[tokio::test]
    async fn an_unknown_compartment_admits_nothing() {
        let store = store();
        let mut compiler = Compiler::new(&store);
        let compartment = Compartment {
            kind: kind("Observation"),
            id: fhir_core::ResourceId::parse("o1").unwrap(),
        };
        let text = compiler.compartment(&compartment, "r");
        assert_eq!(text, "false", "{text}");
    }

    #[tokio::test]
    async fn a_composite_pairs_its_components_on_one_element() {
        let (text, _) = sql(&built(
            "Observation",
            "code-value-quantity",
            Modifier::None,
            "urn:s|c1$4.5",
        ));
        assert!(text.contains("index_token"), "{text}");
        assert!(text.contains("index_quantity"), "{text}");
        assert!(text.contains(".ordinal = "), "{text}");
    }

    #[tokio::test]
    async fn a_value_of_the_wrong_shape_selects_nothing() {
        let store = store();
        let mut compiler = Compiler::new(&store);
        let filter = Filter {
            values: vec![SearchValue::Missing(true)],
            ..Filter::new(
                "code",
                def("Observation", "code").target.clone(),
                Vec::new(),
            )
        };
        let text = compiler.filter(&filter, "r").expect("the filter compiles");
        assert!(text.contains("false"), "{text}");
        assert_eq!(table_for(&filter), Some("index_token"));
    }
}
