use serde_json::Value;

const REFERENCE: &str = "reference";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct References {
    also: Vec<String>,

    relative: bool,

    absolute: bool,
}

impl References {
    pub fn as_written() -> References {
        References::default()
    }

    pub fn normalised<I, S>(also: I) -> References
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        References {
            also: also
                .into_iter()
                .map(|held| held.as_ref().trim().to_owned())
                .filter(|held| !held.is_empty())
                .collect(),
            relative: true,
            absolute: true,
        }
    }

    pub fn relative_both_ways<I, S>(also: I) -> References
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        References {
            absolute: false,
            ..References::normalised(also)
        }
    }

    pub fn is_on(&self) -> bool {
        self.relative || self.absolute
    }

    pub fn answers_absolute(&self) -> bool {
        self.absolute
    }

    fn names(&self, element: &str) -> bool {
        element == REFERENCE || self.also.iter().any(|held| held == element)
    }

    pub fn stored(&self, body: &mut Value, base: &str) {
        if !self.relative {
            return;
        }
        self.walk(body, &|held| shortened(held, base));
    }

    pub fn answered(&self, body: &mut Value, base: &str) {
        if !self.absolute {
            return;
        }
        self.walk(body, &|held| lengthened(held, base));
    }

    fn walk(&self, body: &mut Value, change: &dyn Fn(&str) -> Option<String>) {
        match body {
            Value::Array(held) => {
                for entry in held {
                    self.walk(entry, change);
                }
            }
            Value::Object(held) => {
                for (name, value) in held.iter_mut() {
                    if let Value::String(text) = value {
                        if self.names(name) {
                            if let Some(rewritten) = change(text) {
                                *text = rewritten;
                            }
                        }
                        continue;
                    }
                    self.walk(value, change);
                }
            }
            _ => {}
        }
    }
}

fn shortened(held: &str, base: &str) -> Option<String> {
    let rest = held.strip_prefix(base.trim_end_matches('/'))?;
    let rest = rest.strip_prefix('/')?;
    match rest.is_empty() || rest.starts_with('/') {
        true => None,
        false => Some(rest.to_owned()),
    }
}

fn lengthened(held: &str, base: &str) -> Option<String> {
    if held.is_empty() || held.starts_with('#') || held.contains("://") || held.contains(':') {
        return None;
    }
    Some(format!("{}/{held}", base.trim_end_matches('/')))
}

pub async fn lengthening(
    axum::extract::State(state): axum::extract::State<crate::app::AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if !state.references.answers_absolute() {
        return next.run(request).await;
    }
    let base = state.forwarding.base(request.headers());
    let response = next.run(request).await;
    let (parts, body) = response.into_parts();
    let json = parts
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("json"));
    if !json {
        return axum::response::Response::from_parts(parts, body);
    }
    let Ok(bytes) = axum::body::to_bytes(body, usize::MAX).await else {
        return axum::response::Response::from_parts(parts, axum::body::Body::empty());
    };
    let Ok(mut held) = serde_json::from_slice::<Value>(&bytes) else {
        return axum::response::Response::from_parts(parts, axum::body::Body::from(bytes));
    };
    state.references.answered(&mut held, &base);
    let written = serde_json::to_vec(&held).unwrap_or_else(|_| bytes.to_vec());
    let mut parts = parts;
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    axum::response::Response::from_parts(parts, axum::body::Body::from(written))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BASE: &str = "https://fhir.example.org";

    fn observation() -> Value {
        json!({
            "resourceType": "Observation",
            "id": "ob1",
            "subject": {"reference": "https://fhir.example.org/Patient/p1"},
            "performer": [
                {"reference": "Practitioner/pr1"},
                {"reference": "https://other.example.org/Practitioner/pr2"},
                {"reference": "#contained"},
            ],
        })
    }

    #[test]
    fn a_reference_to_this_server_is_stored_relative() {
        let mut held = observation();
        References::normalised(["url"]).stored(&mut held, BASE);
        assert_eq!(held["subject"]["reference"], "Patient/p1");
        assert_eq!(
            held["performer"][1]["reference"], "https://other.example.org/Practitioner/pr2",
            "another server's reference is another server's"
        );
        assert_eq!(held["performer"][2]["reference"], "#contained");
    }

    #[test]
    fn a_relative_reference_is_answered_absolute() {
        let mut held = json!({
            "resourceType": "Observation",
            "subject": {"reference": "Patient/p1"},
            "performer": [{"reference": "#contained"}, {"reference": "urn:uuid:abc"}],
        });
        References::normalised(["url"]).answered(&mut held, BASE);
        assert_eq!(
            held["subject"]["reference"],
            "https://fhir.example.org/Patient/p1"
        );
        assert_eq!(held["performer"][0]["reference"], "#contained");
        assert_eq!(
            held["performer"][1]["reference"], "urn:uuid:abc",
            "a urn is not a path on this server"
        );
    }

    #[test]
    fn a_named_element_is_rewritten_too() {
        let mut held = json!({
            "resourceType": "DocumentReference",
            "content": [{"attachment": {"url": "https://fhir.example.org/Binary/b1"}}],
        });
        References::normalised(["url"]).stored(&mut held, BASE);
        assert_eq!(held["content"][0]["attachment"]["url"], "Binary/b1");
    }

    #[test]
    fn an_element_nobody_named_is_left_alone() {
        let mut held = json!({
            "resourceType": "DocumentReference",
            "content": [{"attachment": {"url": "https://fhir.example.org/Binary/b1"}}],
        });
        References::normalised(Vec::<String>::new()).stored(&mut held, BASE);
        assert_eq!(
            held["content"][0]["attachment"]["url"],
            "https://fhir.example.org/Binary/b1"
        );
    }

    #[test]
    fn an_instance_that_was_not_asked_changes_nothing() {
        let held = observation();
        let mut stored = held.clone();
        References::as_written().stored(&mut stored, BASE);
        let mut answered = held.clone();
        References::as_written().answered(&mut answered, BASE);
        assert_eq!(stored, held);
        assert_eq!(answered, held);
    }

    #[test]
    fn storing_relative_without_answering_absolute_is_a_choice() {
        let mut held = observation();
        let holding = References::relative_both_ways(["url"]);
        holding.stored(&mut held, BASE);
        assert_eq!(held["subject"]["reference"], "Patient/p1");
        holding.answered(&mut held, BASE);
        assert_eq!(
            held["subject"]["reference"], "Patient/p1",
            "what is stored is what is answered"
        );
    }

    #[test]
    fn a_round_trip_is_what_it_was() {
        let held = observation();
        let mut through = held.clone();
        let holding = References::normalised(["url"]);
        holding.stored(&mut through, BASE);
        holding.answered(&mut through, BASE);
        assert_eq!(
            through["subject"]["reference"], held["subject"]["reference"],
            "a client reads back the reference it wrote"
        );
    }
}
