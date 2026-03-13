use axum::http::{HeaderName, HeaderValue, Request};
use axum::middleware::Next;
use axum::response::Response;
use fhir_core::Error;

const HELD: &[(&str, &str, &str)] = &[
    ("content-type-options", "x-content-type-options", "nosniff"),
    ("frame-options", "x-frame-options", "DENY"),
    ("referrer-policy", "referrer-policy", "no-referrer"),
    (
        "content-security-policy",
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
    (
        "hsts",
        "strict-transport-security",
        "max-age=31536000; includeSubDomains",
    ),
];

const HSTS: &str = "hsts";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityHeaders {
    held: Vec<(HeaderName, HeaderValue, bool)>,
}

impl Default for SecurityHeaders {
    fn default() -> SecurityHeaders {
        SecurityHeaders::parse("").expect("the defaults are valid headers")
    }
}

impl SecurityHeaders {
    pub fn parse(raw: &str) -> Result<SecurityHeaders, Error> {
        let mut chosen: Vec<(&str, Option<String>)> = HELD
            .iter()
            .map(|(name, _, default)| (*name, Some((*default).to_owned())))
            .collect();
        for part in raw
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let (name, value) = part.split_once('=').ok_or_else(|| {
                Error::Config(format!(
                    "security header {part:?} names no value; write name=value or name=off"
                ))
            })?;
            let (name, value) = (name.trim(), value.trim());
            let held = match value.eq_ignore_ascii_case("off") {
                true => None,
                false => Some(value.to_owned()),
            };
            if name == "all" {
                if held.is_some() {
                    return Err(Error::Config(
                        "all=off is the only thing \"all\" may be set to".to_owned(),
                    ));
                }
                chosen.iter_mut().for_each(|(_, value)| *value = None);
                continue;
            }
            let found = chosen
                .iter_mut()
                .find(|(known, _)| *known == name)
                .ok_or_else(|| {
                    Error::Config(format!(
                        "security header {name:?} names none this service sends; it sends {}",
                        HELD.iter()
                            .map(|(name, _, _)| *name)
                            .collect::<Vec<&str>>()
                            .join(", ")
                    ))
                })?;
            found.1 = held;
        }
        let mut built = Vec::new();
        for (name, header, _) in HELD {
            let Some((_, Some(value))) = chosen.iter().find(|(known, _)| known == name) else {
                continue;
            };
            let held = HeaderValue::from_str(value).map_err(|_| {
                Error::Config(format!(
                    "security header {name:?} carries {value:?}, which is no header value"
                ))
            })?;
            built.push((HeaderName::from_static(header), held, *name == HSTS));
        }
        Ok(SecurityHeaders { held: built })
    }

    pub fn none() -> SecurityHeaders {
        SecurityHeaders { held: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    pub fn names(&self) -> Vec<&str> {
        self.held.iter().map(|(name, _, _)| name.as_str()).collect()
    }
}

fn secured<B>(request: &Request<B>) -> bool {
    if request.uri().scheme_str() == Some("https") {
        return true;
    }
    request
        .headers()
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .next()
                .map(str::trim)
                .is_some_and(|held| held.eq_ignore_ascii_case("https"))
        })
}

pub async fn written(
    axum::extract::State(state): axum::extract::State<crate::app::AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if state.security_headers.is_empty() {
        return next.run(request).await;
    }
    let over_tls = secured(&request);
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value, only_over_tls) in &state.security_headers.held {
        if *only_over_tls && !over_tls {
            continue;
        }

        if !headers.contains_key(name) {
            headers.insert(name, value.clone());
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(headers: &SecurityHeaders) -> Vec<String> {
        headers.names().into_iter().map(str::to_owned).collect()
    }

    #[test]
    fn the_defaults_are_the_five_a_browser_wants() {
        let held = SecurityHeaders::default();
        assert_eq!(
            names(&held),
            vec![
                "x-content-type-options",
                "x-frame-options",
                "referrer-policy",
                "content-security-policy",
                "strict-transport-security",
            ]
        );
    }

    #[test]
    fn one_may_be_turned_off_without_the_others() {
        let held = SecurityHeaders::parse("frame-options=off").expect("a valid setting");
        assert!(!names(&held).contains(&"x-frame-options".to_owned()));
        assert!(names(&held).contains(&"referrer-policy".to_owned()));
    }

    #[test]
    fn all_of_them_may_be_turned_off() {
        let held = SecurityHeaders::parse("all=off").expect("a valid setting");
        assert!(held.is_empty());
    }

    #[test]
    fn a_policy_the_operator_writes_replaces_the_one_this_build_guessed() {
        let held = SecurityHeaders::parse("content-security-policy=default-src 'self'")
            .expect("a valid setting");
        let written = held
            .held
            .iter()
            .find(|(name, _, _)| name.as_str() == "content-security-policy")
            .expect("it is sent");
        assert_eq!(written.1, "default-src 'self'");
    }

    #[test]
    fn a_name_this_service_does_not_send_fails_fast() {
        let error = SecurityHeaders::parse("x-made-up=1").expect_err("an unknown name must fail");
        assert!(error.to_string().contains("x-made-up"), "{error}");
        assert!(
            error.to_string().contains("frame-options"),
            "and says what it does send: {error}"
        );
    }

    #[test]
    fn a_setting_that_is_not_a_pair_fails_fast() {
        assert!(SecurityHeaders::parse("frame-options").is_err());
        assert!(SecurityHeaders::parse("all=DENY").is_err());
    }
}
