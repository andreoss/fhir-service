use axum::http::HeaderMap;

const PREFER: &str = "prefer";
const RETURN: &str = "return";
const MINIMAL: &str = "minimal";
const REPRESENTATION: &str = "representation";
const OUTCOME: &str = "operationoutcome";
const RESPOND_ASYNC: &str = "respond-async";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Return {
    Minimal,
    Representation,
    Outcome,
}

impl Return {
    pub fn asked_for(headers: &HeaderMap) -> Option<Return> {
        headers.get_all(PREFER).iter().filter_map(asked).next()
    }

    fn parse(text: &str) -> Option<Return> {
        let (name, value) = text.split_once('=')?;
        match name.trim().eq_ignore_ascii_case(RETURN) {
            false => None,
            true => match value.trim().to_ascii_lowercase().as_str() {
                MINIMAL => Some(Return::Minimal),
                REPRESENTATION => Some(Return::Representation),
                OUTCOME => Some(Return::Outcome),
                _ => None,
            },
        }
    }
}

fn asked(value: &axum::http::HeaderValue) -> Option<Return> {
    let text = value.to_str().ok()?;
    text.split(',').filter_map(Return::parse).next()
}

const HANDLING: &str = "handling";
const STRICT: &str = "strict";
const LENIENT: &str = "lenient";




#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handling {
    Strict,
    Lenient,
}

impl Handling {
    pub fn asked_for(headers: &HeaderMap) -> Result<Handling, fhir_core::Error> {
        let mut held = Handling::Strict;
        for value in headers.get_all(PREFER).iter() {
            let Ok(text) = value.to_str() else {
                continue;
            };
            for token in text.split(',').flat_map(|part| part.split(';')) {
                let Some((name, wanted)) = token.split_once('=') else {
                    continue;
                };
                if !name.trim().eq_ignore_ascii_case(HANDLING) {
                    continue;
                }
                held = match wanted.trim().to_ascii_lowercase().as_str() {
                    STRICT => Handling::Strict,
                    LENIENT => Handling::Lenient,
                    other => {
                        return Err(fhir_core::Error::InvalidParameter(format!(
                            "Prefer: handling={other:?} names neither strict nor lenient"
                        )))
                    }
                };
            }
        }
        Ok(held)
    }

    pub fn is_lenient(&self) -> bool {
        matches!(self, Handling::Lenient)
    }
}

pub fn respond_async(headers: &HeaderMap) -> bool {
    headers
        .get_all(PREFER)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|text| {
            text.split(',')
                .chain(text.split(';'))
                .any(|token| token.trim().eq_ignore_ascii_case(RESPOND_ASYNC))
        })
}

#[cfg(test)]
mod tests {
    use super::Return;
    use axum::http::HeaderMap;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append("prefer", value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn a_return_is_read_from_the_preference_a_client_sends() {
        for (text, expected) in [
            ("return=minimal", Return::Minimal),
            ("return = representation", Return::Representation),
            ("return=OperationOutcome", Return::Outcome),
            ("return=OPERATIONOUTCOME", Return::Outcome),
        ] {
            assert_eq!(
                Return::asked_for(&headers(&[text])),
                Some(expected),
                "{text}"
            );
        }
    }

    #[test]
    fn a_preference_that_is_not_a_return_is_not_one() {
        for text in [
            "return=nonsense",
            "handling=lenient",
            "nonsense",
            "respond-async",
            "",
        ] {
            assert_eq!(Return::asked_for(&headers(&[text])), None, "{text}");
        }
    }

    #[test]
    fn a_return_beside_another_preference_is_read() {
        let headers = headers(&["handling=lenient, return=minimal"]);
        assert_eq!(Return::asked_for(&headers), Some(Return::Minimal));
    }

    #[test]
    fn the_first_return_of_several_preference_headers_wins() {
        let headers = headers(&["handling=strict", "return=minimal", "return=representation"]);
        assert_eq!(Return::asked_for(&headers), Some(Return::Minimal));
    }

    #[test]
    fn a_client_that_asks_to_answer_later_is_heard() {
        for text in [
            "respond-async",
            "Respond-Async",
            " respond-async ",
            "handling=lenient, respond-async",
            "respond-async; return=minimal",
        ] {
            assert!(super::respond_async(&headers(&[text])), "{text}");
        }
    }

    #[test]
    fn a_preference_that_is_not_to_answer_later_is_not_one() {
        for text in ["return=minimal", "handling=lenient", "", "respond"] {
            assert!(!super::respond_async(&headers(&[text])), "{text}");
        }
        assert!(!super::respond_async(&HeaderMap::new()));
    }
}
