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
    Date,
    Token,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchValue {
    Date {
        comparator: Comparator,
        period: InstantPeriod,
    },
    Token(Token),
    Uri(String),
}

impl SearchValue {
    pub fn parse(value_type: ValueType, raw: &str) -> Result<SearchValue, Error> {
        match value_type {
            ValueType::Date => {
                let (comparator, rest) = Comparator::split(raw);
                let period = InstantPeriod::parse(rest)
                    .map_err(|_| Error::InvalidParameter(format!("date {raw:?}")))?;
                Ok(SearchValue::Date { comparator, period })
            }
            ValueType::Token => Ok(SearchValue::Token(parse_token(raw))),
            ValueType::Uri => Ok(SearchValue::Uri(raw.to_owned())),
        }
    }

    pub fn is_negated(&self) -> bool {
        matches!(
            self,
            SearchValue::Date {
                comparator: Comparator::Ne,
                ..
            }
        )
    }

    pub fn matches(&self, element: &Value) -> bool {
        match self {
            SearchValue::Date { comparator, period } => date_matches(*comparator, period, element),
            SearchValue::Token(token) => token_matches(token, element),
            SearchValue::Uri(text) => uri_matches(text, element),
        }
    }
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
