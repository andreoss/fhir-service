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
        }
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
    let decimals = rest.split_once('.').map(|(_, tail)| tail.len()).unwrap_or_default();
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
        Comparator::Eq | Comparator::Ne => (stored - value).abs() < tolerance,
        Comparator::Gt | Comparator::Sa => stored > value,
        Comparator::Lt | Comparator::Eb => stored < value,
        Comparator::Ge => stored >= value,
        Comparator::Le => stored <= value,
        Comparator::Ap => (stored - value).abs() <= value.abs() * 0.1 + tolerance,
    }
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
            let code_ok = code.is_none_or(|wanted| stored_code == Some(wanted));
            system_ok && code_ok && map.get("value").is_some_and(|value| number.matches(value))
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
            if map.get("coding").is_some_and(|codings| token_matches(token, codings)) {
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
        TokenSystem::Exact(wanted) => system == Some(wanted.as_str()),
    };
    system_ok && token.code.as_ref().is_none_or(|wanted| wanted == code)
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
        Value::Array(items) => items.iter().any(|item| date_matches(comparator, query, item)),
        Value::String(text) => InstantPeriod::parse(text)
            .is_ok_and(|stored| compare(comparator, query, &stored)),
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
    fn a_prefix_is_split_off_and_a_bare_value_is_equality() {
        assert_eq!(Comparator::split("ge2026"), (Comparator::Ge, "2026"));
        assert_eq!(Comparator::split("2026"), (Comparator::Eq, "2026"));
        assert_eq!(Comparator::split("eq"), (Comparator::Eq, "eq"));
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
    fn a_token_reaches_into_a_codeable_concept() {
        let token = parse_token("urn:s|c");
        let concept = json!({"coding": [{"system": "urn:s", "code": "c"}]});
        assert!(token_matches(&token, &concept));
        assert!(!token_matches(&parse_token("urn:s|d"), &concept));
        assert!(!token_matches(&token, &Value::Null));
        assert!(token_matches(&parse_token("7"), &json!(7)));
    }

    #[test]
    fn a_date_value_compares_against_a_stored_period() {
        let value = SearchValue::parse(ValueType::Date, "ge2026-09-06").unwrap();
        assert!(value.matches(&json!({"start": "2026-09-07T00:00:00Z"})));
        assert!(!value.matches(&json!({"start": "2020-01-01T00:00:00Z", "end": "2020-02-01T00:00:00Z"})));
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
        assert!(!value.matches(&json!(4)));
        assert!(!value.is_negated());
    }
}
