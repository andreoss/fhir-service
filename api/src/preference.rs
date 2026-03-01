use axum::http::HeaderMap;

const PREFER: &str = "prefer";
const RETURN: &str = "return";
const MINIMAL: &str = "minimal";
const REPRESENTATION: &str = "representation";
const OUTCOME: &str = "operationoutcome";

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
}
