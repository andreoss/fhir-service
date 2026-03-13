use axum::http::header::{self, HeaderMap};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Forwarding {
    trusted: bool,
}

impl Forwarding {
    pub fn untrusted() -> Forwarding {
        Forwarding::default()
    }

    pub fn trusted() -> Forwarding {
        Forwarding { trusted: true }
    }

    pub fn is_trusted(&self) -> bool {
        self.trusted
    }

    pub fn base(&self, headers: &HeaderMap) -> String {
        let host = self.host(headers);
        format!("{}://{host}", self.scheme(headers))
    }

    pub fn host(&self, headers: &HeaderMap) -> String {
        let forwarded = match self.trusted {
            false => None,
            true => first(headers, "x-forwarded-host"),
        };
        let held = forwarded
            .or_else(|| {
                headers
                    .get(header::HOST)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        match held.trim().is_empty() {
            true => "localhost".to_owned(),
            false => held.trim().to_owned(),
        }
    }

    pub fn scheme(&self, headers: &HeaderMap) -> &'static str {
        let named = match self.trusted {
            false => None,
            true => first(headers, "x-forwarded-proto"),
        };
        match named.as_deref() {
            Some(held) if held.eq_ignore_ascii_case("https") => "https",
            _ => "http",
        }
    }
}

fn first(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|held| !held.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut held = HeaderMap::new();
        for (name, value) in pairs {
            held.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("a header name"),
                value.parse().expect("a header value"),
            );
        }
        held
    }

    #[test]
    fn an_instance_holding_its_own_socket_reads_host() {
        let held = Forwarding::untrusted();
        let carried = headers(&[
            ("host", "127.0.0.1:8080"),
            ("x-forwarded-host", "fhir.example.org"),
            ("x-forwarded-proto", "https"),
        ]);
        assert_eq!(
            held.base(&carried),
            "http://127.0.0.1:8080",
            "a client may write those two headers, so an untrusting instance \
             does not read them"
        );
    }

    #[test]
    fn an_instance_behind_a_proxy_reads_what_the_client_asked_for() {
        let held = Forwarding::trusted();
        let carried = headers(&[
            ("host", "127.0.0.1:8080"),
            ("x-forwarded-host", "fhir.example.org"),
            ("x-forwarded-proto", "https"),
        ]);
        assert_eq!(held.base(&carried), "https://fhir.example.org");
    }

    #[test]
    fn a_chain_of_proxies_is_read_from_the_first_hop() {
        let held = Forwarding::trusted();
        let carried = headers(&[
            ("host", "127.0.0.1:8080"),
            ("x-forwarded-host", "fhir.example.org, inner.example.org"),
            ("x-forwarded-proto", "https, http"),
        ]);
        assert_eq!(held.base(&carried), "https://fhir.example.org");
    }

    #[test]
    fn a_trusting_instance_falls_back_to_host() {
        let held = Forwarding::trusted();
        assert_eq!(
            held.base(&headers(&[("host", "fhir.example.org")])),
            "http://fhir.example.org"
        );
    }

    #[test]
    fn a_request_with_no_host_at_all_is_localhost() {
        assert_eq!(
            Forwarding::untrusted().base(&headers(&[])),
            "http://localhost"
        );
    }
}
