use crate::search::index::IndexKey;
use crate::{Error, InstantPeriod};
use serde_json::Value;

const DAY: i64 = 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparator {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    Sa,
    Eb,
    Ap,
}

impl Comparator {
    pub fn split(raw: &str) -> (Comparator, &str) {
        const PREFIXES: [(&str, Comparator); 9] = [
            ("eq", Comparator::Eq),
            ("ne", Comparator::Ne),
            ("gt", Comparator::Gt),
            ("lt", Comparator::Lt),
            ("ge", Comparator::Ge),
            ("le", Comparator::Le),
            ("sa", Comparator::Sa),
            ("eb", Comparator::Eb),
            ("ap", Comparator::Ap),
        ];
        for (prefix, comparator) in PREFIXES {
            if let Some(rest) = raw.strip_prefix(prefix) {
                if !rest.is_empty() {
                    return (comparator, rest);
                }
            }
        }
        (Comparator::Eq, raw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    Number,
    Date,
    String,
    Token,
    Quantity,
    Reference,
    Composite,
    Uri,
}

impl ValueType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ValueType::Number => "number",
            ValueType::Date => "date",
            ValueType::String => "string",
            ValueType::Token => "token",
            ValueType::Quantity => "quantity",
            ValueType::Reference => "reference",
            ValueType::Composite => "composite",
            ValueType::Uri => "uri",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSystem {
    Any,
    Absent,
    Exact(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub system: TokenSystem,
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SearchValue {
    Number {
        comparator: Comparator,
        value: f64,
        tolerance: f64,
    },
    Date {
        comparator: Comparator,
        period: InstantPeriod,
    },
    Text(String),
    Token(Token),
    Quantity {
        number: Box<SearchValue>,
        system: TokenSystem,
        code: Option<String>,
    },
    Reference(String),
    Composite {
        left: Box<SearchValue>,
        right: Box<SearchValue>,
    },
    Uri(String),
    OfType {
        system: TokenSystem,
        code: Option<String>,
        value: String,
    },
    Missing(bool),
}

impl SearchValue {
    pub fn parse(value_type: ValueType, raw: &str) -> Result<SearchValue, Error> {
        match value_type {
            ValueType::Number => number(raw),
            ValueType::Date => {
                let (comparator, rest) = Comparator::split(raw);
                let period = InstantPeriod::parse(rest)
                    .map_err(|_| Error::InvalidParameter(format!("date {raw:?}")))?;
                Ok(SearchValue::Date { comparator, period })
            }
            ValueType::String => Ok(SearchValue::Text(raw.to_owned())),
            ValueType::Token => Ok(SearchValue::Token(parse_token(raw))),
            ValueType::Quantity => quantity(raw),
            ValueType::Reference => Ok(SearchValue::Reference(raw.to_owned())),
            ValueType::Composite => Err(Error::InvalidParameter(format!(
                "composite value {raw:?} needs a parameter definition"
            ))),
            ValueType::Uri => Ok(SearchValue::Uri(raw.to_owned())),
        }
    }

    pub fn of_type(raw: &str) -> Result<SearchValue, Error> {
        let mut parts = raw.splitn(3, '|');
        let system = parts.next().unwrap_or_default();
        match (parts.next(), parts.next()) {
            (Some(code), Some(value)) if !value.is_empty() => Ok(SearchValue::OfType {
                system: match system {
                    "" => TokenSystem::Any,
                    text => TokenSystem::Exact(text.to_owned()),
                },
                code: (!code.is_empty()).then(|| code.to_owned()),
                value: value.to_owned(),
            }),
            _ => Err(Error::InvalidParameter(format!(
                "of-type {raw:?} needs a system, a code and a value"
            ))),
        }
    }

    pub fn composite(left: SearchValue, right: SearchValue) -> SearchValue {
        SearchValue::Composite {
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn components(&self) -> Option<(&SearchValue, &SearchValue)> {
        match self {
            SearchValue::Composite { left, right } => Some((left, right)),
            _ => None,
        }
    }

    pub fn is_negated(&self) -> bool {
        matches!(
            self,
            SearchValue::Date {
                comparator: Comparator::Ne,
                ..
            } | SearchValue::Number {
                comparator: Comparator::Ne,
                ..
            }
        )
    }

    pub fn matches(&self, element: &Value) -> bool {
        match self {
            SearchValue::Number {
                comparator,
                value,
                tolerance,
            } => number_matches(*comparator, *value, *tolerance, element),
            SearchValue::Date { comparator, period } => date_matches(*comparator, period, element),
            SearchValue::Text(text) => text_matches(text, element),
            SearchValue::Token(token) => token_matches(token, element),
            SearchValue::Quantity {
                number,
                system,
                code,
            } => quantity_matches(number, system, code.as_deref(), element),
            SearchValue::Reference(target) => reference_matches(target, element),
            SearchValue::Composite { .. } => false,
            SearchValue::Uri(text) => uri_matches(text, element),
            SearchValue::OfType {
                system,
                code,
                value,
            } => of_type_matches(system, code.as_deref(), value, element),
            SearchValue::Missing(_) => false,
        }
    }
}

fn of_type_matches(system: &TokenSystem, code: Option<&str>, value: &str, element: &Value) -> bool {
    match element {
        Value::Array(items) => items
            .iter()
            .any(|item| of_type_matches(system, code, value, item)),
        Value::Object(map) => {
            let qualifier = Token {
                system: system.clone(),
                code: code.map(str::to_owned),
            };
            map.get("value").and_then(Value::as_str) == Some(value)
                && map
                    .get("type")
                    .is_some_and(|found| token_matches(&qualifier, found))
        }
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Null => false,
    }
}

fn number(raw: &str) -> Result<SearchValue, Error> {
    let (comparator, rest) = Comparator::split(raw);
    let value = rest
        .parse::<f64>()
        .map_err(|_| Error::InvalidParameter(format!("number {raw:?}")))?;
    if !value.is_finite() {
        return Err(Error::InvalidParameter(format!("number {raw:?}")));
    }
    let decimals = rest
        .split_once('.')
        .map(|(_, tail)| tail.len())
        .unwrap_or_default();
    Ok(SearchValue::Number {
        comparator,
        value,
        tolerance: 0.5 * 10f64.powi(-(decimals as i32)),
    })
}

fn quantity(raw: &str) -> Result<SearchValue, Error> {
    let mut parts = raw.splitn(3, '|');
    let head = parts.next().unwrap_or_default();
    let system = match parts.next() {
        None | Some("") => TokenSystem::Any,
        Some(text) => TokenSystem::Exact(text.to_owned()),
    };
    let code = match parts.next() {
        None | Some("") => None,
        Some(text) => Some(text.to_owned()),
    };
    Ok(SearchValue::Quantity {
        number: Box::new(number(head)?),
        system,
        code,
    })
}

fn number_matches(comparator: Comparator, value: f64, tolerance: f64, element: &Value) -> bool {
    match element {
        Value::Array(items) => items
            .iter()
            .any(|item| number_matches(comparator, value, tolerance, item)),
        Value::Number(number) => match number.as_f64() {
            Some(stored) => compare_number(comparator, value, tolerance, stored),
            None => false,
        },
        Value::String(text) => text
            .parse::<f64>()
            .is_ok_and(|stored| compare_number(comparator, value, tolerance, stored)),
        Value::Object(map) => map
            .get("value")
            .is_some_and(|nested| number_matches(comparator, value, tolerance, nested)),
        Value::Bool(_) | Value::Null => false,
    }
}

fn compare_number(comparator: Comparator, value: f64, tolerance: f64, stored: f64) -> bool {
    match comparator {
        Comparator::Eq | Comparator::Ne => (stored - value).abs() <= widened(value, tolerance),
        Comparator::Gt | Comparator::Sa => stored > value,
        Comparator::Lt | Comparator::Eb => stored < value,
        Comparator::Ge => stored >= value,
        Comparator::Le => stored <= value,
        Comparator::Ap => {
            let reach = value.abs() * 0.1 + tolerance;
            (stored - value).abs() <= widened(value, reach)
        }
    }
}

fn widened(value: f64, tolerance: f64) -> f64 {
    tolerance + value.abs().max(tolerance) * f64::EPSILON * 4.0
}

fn text_matches(wanted: &str, element: &Value) -> bool {
    match element {
        Value::Array(items) => items.iter().any(|item| text_matches(wanted, item)),
        Value::String(text) => text.to_lowercase().starts_with(&wanted.to_lowercase()),
        Value::Object(map) => map.values().any(|nested| text_matches(wanted, nested)),
        Value::Number(_) | Value::Bool(_) | Value::Null => false,
    }
}

fn quantity_matches(
    number: &SearchValue,
    system: &TokenSystem,
    code: Option<&str>,
    element: &Value,
) -> bool {
    match element {
        Value::Array(items) => items
            .iter()
            .any(|item| quantity_matches(number, system, code, item)),
        Value::Object(map) => {
            let stored_system = map.get("system").and_then(Value::as_str);
            let stored_code = map
                .get("code")
                .and_then(Value::as_str)
                .or_else(|| map.get("unit").and_then(Value::as_str));
            let system_ok = match system {
                TokenSystem::Any => true,
                TokenSystem::Absent => stored_system.is_none(),
                TokenSystem::Exact(wanted) => stored_system == Some(wanted.as_str()),
            };
            if !system_ok {
                return false;
            }
            let Some(held) = map.get("value") else {
                return false;
            };

            let named_ucum = match system {
                TokenSystem::Exact(held) => held == crate::ucum::UCUM,
                TokenSystem::Any | TokenSystem::Absent => true,
            };
            if let (Some(wanted), Some(stored), Some(number_value)) =
                (code.filter(|_| named_ucum), stored_code, held.as_f64())
            {
                let asked = crate::ucum::canonical(1.0, None, Some(wanted));
                let carried = crate::ucum::canonical(number_value, stored_system, Some(stored));
                if let (Some((factor, one)), Some((value, other))) = (asked, carried) {
                    if one != other {
                        return false;
                    }
                    let scaled = Value::from(value / factor);
                    return number.matches(&scaled);
                }
            }
            let code_ok = code.is_none_or(|wanted| stored_code == Some(wanted));
            code_ok && number.matches(held)
        }
        Value::Number(_) | Value::String(_) => {
            matches!(system, TokenSystem::Any) && code.is_none() && number.matches(element)
        }
        Value::Bool(_) | Value::Null => false,
    }
}

fn reference_matches(wanted: &str, element: &Value) -> bool {
    match element {
        Value::Array(items) => items.iter().any(|item| reference_matches(wanted, item)),
        Value::String(text) => same_reference(wanted, text),
        Value::Object(map) => map
            .get("reference")
            .and_then(Value::as_str)
            .is_some_and(|text| same_reference(wanted, text)),
        Value::Number(_) | Value::Bool(_) | Value::Null => false,
    }
}

fn same_reference(wanted: &str, stored: &str) -> bool {
    if wanted == stored {
        return true;
    }
    !wanted.contains('/') && stored.rsplit('/').next() == Some(wanted)
}

fn parse_token(raw: &str) -> Token {
    match raw.split_once('|') {
        None => Token {
            system: TokenSystem::Any,
            code: Some(raw.to_owned()),
        },
        Some(("", code)) => Token {
            system: TokenSystem::Absent,
            code: Some(code.to_owned()),
        },
        Some((system, "")) => Token {
            system: TokenSystem::Exact(system.to_owned()),
            code: None,
        },
        Some((system, code)) => Token {
            system: TokenSystem::Exact(system.to_owned()),
            code: Some(code.to_owned()),
        },
    }
}

fn token_matches(token: &Token, element: &Value) -> bool {
    match element {
        Value::Array(items) => items.iter().any(|item| token_matches(token, item)),
        Value::String(text) => accepts(token, None, text),
        Value::Bool(flag) => accepts(token, None, &flag.to_string()),
        Value::Number(number) => accepts(token, None, &number.to_string()),
        Value::Object(map) => {
            if map
                .get("coding")
                .is_some_and(|codings| token_matches(token, codings))
            {
                return true;
            }
            let system = map.get("system").and_then(Value::as_str);
            let code = map
                .get("code")
                .and_then(Value::as_str)
                .or_else(|| map.get("value").and_then(Value::as_str));
            code.is_some_and(|code| accepts(token, system, code))
        }
        Value::Null => false,
    }
}

fn accepts(token: &Token, system: Option<&str>, code: &str) -> bool {
    let system_ok = match &token.system {
        TokenSystem::Any => true,
        TokenSystem::Absent => system.is_none(),
        TokenSystem::Exact(wanted) => {
            system.is_some_and(|found| IndexKey::of(wanted).matches(found))
        }
    };
    system_ok
        && token
            .code
            .as_ref()
            .is_none_or(|wanted| IndexKey::of(wanted).matches(code))
}

fn uri_matches(wanted: &str, element: &Value) -> bool {
    match element {
        Value::Array(items) => items.iter().any(|item| uri_matches(wanted, item)),
        Value::String(text) => text == wanted,
        Value::Object(map) => map
            .get("reference")
            .and_then(Value::as_str)
            .is_some_and(|text| text == wanted),
        Value::Bool(_) | Value::Number(_) | Value::Null => false,
    }
}

fn date_matches(comparator: Comparator, query: &InstantPeriod, element: &Value) -> bool {
    match element {
        Value::Array(items) => items
            .iter()
            .any(|item| date_matches(comparator, query, item)),
        Value::String(text) => {
            InstantPeriod::parse(text).is_ok_and(|stored| compare(comparator, query, &stored))
        }
        Value::Object(map) => {
            let start = map.get("start").and_then(Value::as_str);
            let end = map.get("end").and_then(Value::as_str);
            match span(start, end) {
                Some(stored) => compare(comparator, query, &stored),
                None => false,
            }
        }
        Value::Bool(_) | Value::Number(_) | Value::Null => false,
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

fn compare(comparator: Comparator, query: &InstantPeriod, stored: &InstantPeriod) -> bool {
    match comparator {
        Comparator::Eq | Comparator::Ne => {
            stored.low() >= query.low() && stored.high() <= query.high()
        }
        Comparator::Gt => stored.high() > query.high(),
        Comparator::Lt => stored.low() < query.low(),
        Comparator::Ge => stored.high() >= query.low(),
        Comparator::Le => stored.low() <= query.high(),
        Comparator::Sa => stored.low() > query.high(),
        Comparator::Eb => stored.high() < query.low(),
        Comparator::Ap => query
            .widened(DAY)
            .is_some_and(|near| stored.low() <= near.high() && stored.high() >= near.low()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_prefix_is_carried_only_by_the_types_that_publish_one() {
        for (raw, comparator) in [
            ("eq2026", Comparator::Eq),
            ("ne2026", Comparator::Ne),
            ("gt2026", Comparator::Gt),
            ("lt2026", Comparator::Lt),
            ("ge2026", Comparator::Ge),
            ("le2026", Comparator::Le),
            ("sa2026", Comparator::Sa),
            ("eb2026", Comparator::Eb),
            ("ap2026", Comparator::Ap),
            ("2026", Comparator::Eq),
        ] {
            let held = SearchValue::parse(ValueType::Date, raw).expect("the date parses");
            assert!(
                matches!(held, SearchValue::Date { comparator: found, .. } if found == comparator),
                "{raw}"
            );
        }
        let number = SearchValue::parse(ValueType::Number, "ge4.5").unwrap();
        assert!(matches!(
            number,
            SearchValue::Number {
                comparator: Comparator::Ge,
                ..
            }
        ));
        let quantity = SearchValue::parse(ValueType::Quantity, "lt5|urn:u|kg").unwrap();
        let SearchValue::Quantity { number, .. } = quantity else {
            panic!("a quantity carries a number")
        };
        assert!(matches!(
            *number,
            SearchValue::Number {
                comparator: Comparator::Lt,
                ..
            }
        ));

        assert_eq!(
            SearchValue::parse(ValueType::String, "gtAnn").unwrap(),
            SearchValue::Text("gtAnn".to_owned())
        );
        assert_eq!(
            SearchValue::parse(ValueType::Uri, "eqhttp://x/y").unwrap(),
            SearchValue::Uri("eqhttp://x/y".to_owned())
        );
        assert_eq!(
            SearchValue::parse(ValueType::Reference, "sa-1").unwrap(),
            SearchValue::Reference("sa-1".to_owned())
        );
        let token = SearchValue::parse(ValueType::Token, "lead").unwrap();
        assert_eq!(token, SearchValue::Token(parse_token("lead")));
        assert!(token.matches(&json!("lead")));
        assert!(!token.matches(&json!("ad")));
    }

    #[test]
    fn an_of_type_value_without_all_three_parts_is_refused() {
        assert!(SearchValue::of_type("urn:s|code").is_err());
        assert!(SearchValue::of_type("urn:s|code|").is_err());
        assert!(SearchValue::of_type("only").is_err());
        let held = SearchValue::of_type("|code|value").expect("a value with any system");
        assert!(matches!(held, SearchValue::OfType { .. }));
    }

    #[test]
    fn a_value_that_cannot_be_read_as_a_number_is_refused() {
        assert!(SearchValue::parse(ValueType::Number, "many").is_err());
        assert!(SearchValue::parse(ValueType::Number, "geinf").is_err());
        assert!(SearchValue::parse(ValueType::Number, "ge4.5").is_ok());
    }

    #[test]
    fn a_composite_and_a_missing_value_never_match_an_element() {
        let composite = SearchValue::composite(
            SearchValue::Text("a".to_owned()),
            SearchValue::Text("b".to_owned()),
        );
        assert!(!composite.matches(&json!("a")));
        assert!(!SearchValue::Missing(true).matches(&json!("a")));
    }

    #[test]
    fn a_number_is_matched_however_the_element_carries_it() {
        let held = SearchValue::parse(ValueType::Number, "4.5").expect("a number parses");
        assert!(held.matches(&json!(4.5)));
        assert!(held.matches(&json!("4.5")));
        assert!(held.matches(&json!([1, 4.5])));
        assert!(held.matches(&json!({"value": 4.5})));
        assert!(!held.matches(&json!("not a number")));
        assert!(!held.matches(&json!(true)));
        assert!(!held.matches(&Value::Null));
    }

    #[test]
    fn a_number_selects_the_closed_range_its_precision_denotes() {
        let held = SearchValue::parse(ValueType::Number, "0.4").expect("a number parses");
        for inside in [0.35, 0.36, 0.4, 0.42, 0.44, 0.45] {
            assert!(held.matches(&json!(inside)), "{inside}");
        }
        for outside in [0.34, 0.46, 0.5] {
            assert!(!held.matches(&json!(outside)), "{outside}");
        }
        let exact = SearchValue::parse(ValueType::Number, "100").expect("a number parses");
        for inside in [99.5, 100.0, 100.5] {
            assert!(exact.matches(&json!(inside)), "{inside}");
        }
        assert!(!exact.matches(&json!(100.6)));
        let negated = SearchValue::parse(ValueType::Number, "ne0.4").expect("a number parses");
        assert!(negated.is_negated());
        assert!(negated.matches(&json!(0.45)));
        assert!(!negated.matches(&json!(0.46)));
    }

    #[test]
    fn an_ordering_comparator_on_a_number_is_the_bare_relation() {
        for (raw, stored, wanted) in [
            ("gt0.4", 0.45, true),
            ("gt0.4", 0.4, false),
            ("ge0.4", 0.4, true),
            ("lt0.4", 0.35, true),
            ("lt0.4", 0.4, false),
            ("le0.4", 0.4, true),
            ("sa0.4", 0.45, true),
            ("eb0.4", 0.35, true),
        ] {
            let held = SearchValue::parse(ValueType::Number, raw).expect("the number parses");
            assert_eq!(
                held.matches(&json!(stored)),
                wanted,
                "{raw} against {stored}"
            );
        }
    }

    #[test]
    fn a_malformed_date_is_an_invalid_parameter() {
        let error = SearchValue::parse(ValueType::Date, "whenever").unwrap_err();
        assert!(matches!(error, Error::InvalidParameter(_)));
    }

    #[test]
    fn token_systems_are_distinguished() {
        assert_eq!(
            parse_token("a|b"),
            Token {
                system: TokenSystem::Exact("a".to_owned()),
                code: Some("b".to_owned())
            }
        );
        assert_eq!(parse_token("a|").system, TokenSystem::Exact("a".to_owned()));
        assert_eq!(parse_token("a|").code, None);
        assert_eq!(parse_token("|b").system, TokenSystem::Absent);
        assert_eq!(parse_token("b").system, TokenSystem::Any);
    }

    #[test]
    fn a_token_matches_the_way_its_system_was_supplied() {
        let with = json!({"system": "urn:s", "code": "c"});
        let without = json!({"code": "c"});
        for (raw, wanted, sample) in [
            ("c", true, &with),
            ("c", true, &without),
            ("urn:s|c", true, &with),
            ("urn:s|c", false, &without),
            ("|c", false, &with),
            ("|c", true, &without),
            ("urn:s|", true, &with),
            ("urn:s|", false, &without),
            ("urn:other|c", false, &with),
        ] {
            let token = parse_token(raw);
            assert_eq!(
                token_matches(&token, sample),
                wanted,
                "{raw} against {sample}"
            );
        }
    }

    #[test]
    fn a_token_reaches_into_a_codeable_concept() {
        let token = parse_token("urn:s|c");
        let concept = json!({"coding": [{"system": "urn:s", "code": "c"}]});
        assert!(token_matches(&token, &concept));
        assert!(!token_matches(&parse_token("urn:s|d"), &concept));
        assert!(!token_matches(&token, &Value::Null));
        assert!(token_matches(&parse_token("7"), &json!(7)));
        assert!(token_matches(&parse_token("true"), &json!(true)));
        assert!(token_matches(
            &parse_token("urn:mrn|42"),
            &json!({"system": "urn:mrn", "value": "42"})
        ));
    }

    #[test]
    fn a_date_value_compares_against_a_stored_period() {
        let value = SearchValue::parse(ValueType::Date, "ge2026-09-06").unwrap();
        assert!(value.matches(&json!({"start": "2026-09-07T00:00:00Z"})));
        assert!(!value
            .matches(&json!({"start": "2020-01-01T00:00:00Z", "end": "2020-02-01T00:00:00Z"})));
        assert!(!value.matches(&json!({})));
        assert!(!value.matches(&json!(3)));
    }

    #[test]
    fn approximate_and_ordering_comparators_are_distinct() {
        let near = SearchValue::parse(ValueType::Date, "ap2026-09-06").unwrap();
        assert!(near.matches(&json!("2026-09-07")));
        assert!(!near.matches(&json!("2026-09-30")));
        let after = SearchValue::parse(ValueType::Date, "sa2026-09-06").unwrap();
        assert!(after.matches(&json!("2026-09-07")));
        assert!(!after.matches(&json!("2026-09-06")));
        let before = SearchValue::parse(ValueType::Date, "eb2026-09-06").unwrap();
        assert!(before.matches(&json!("2026-09-05")));
        let lower = SearchValue::parse(ValueType::Date, "lt2026-09-06").unwrap();
        assert!(lower.matches(&json!("2026-09-05")));
        let upper = SearchValue::parse(ValueType::Date, "le2026-09-06").unwrap();
        assert!(upper.matches(&json!("2026-09-06")));
        let greater = SearchValue::parse(ValueType::Date, "gt2026-09-06").unwrap();
        assert!(greater.matches(&json!("2026-09-07")));
    }

    #[test]
    fn a_uri_value_is_compared_verbatim() {
        let value = SearchValue::parse(ValueType::Uri, "http://x/y").unwrap();
        assert!(value.matches(&json!(["http://x/y"])));
        assert!(value.matches(&json!({"reference": "http://x/y"})));
        assert!(!value.matches(&json!("http://x/z")));
        assert!(!value.matches(&json!("http://x/y/z")));
        assert!(!value.matches(&json!("HTTP://X/Y")));
        assert!(!value.matches(&json!(4)));
        assert!(!value.is_negated());
    }

    #[test]
    fn a_reference_without_a_type_matches_the_id_alone() {
        let stored = json!({"reference": "Patient/p-1"});
        assert!(SearchValue::parse(ValueType::Reference, "Patient/p-1")
            .unwrap()
            .matches(&stored));
        assert!(SearchValue::parse(ValueType::Reference, "p-1")
            .unwrap()
            .matches(&stored));
        assert!(!SearchValue::parse(ValueType::Reference, "Group/p-1")
            .unwrap()
            .matches(&stored));
        assert!(!SearchValue::parse(ValueType::Reference, "p-2")
            .unwrap()
            .matches(&stored));
    }
}
