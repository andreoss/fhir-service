use fhir_core::Error;

pub fn encode(offset: usize) -> String {
    let body = format!("{offset:x}");
    format!("{:02x}{body}", checksum(&body))
}

pub fn decode(text: &str) -> Result<usize, Error> {
    let invalid = || Error::InvalidParameter(format!("ct {text:?}"));
    if text.len() < 3 || !text.is_char_boundary(2) {
        return Err(invalid());
    }
    let (check, body) = text.split_at(2);
    if check != format!("{:02x}", checksum(body)) {
        return Err(invalid());
    }
    usize::from_str_radix(body, 16).map_err(|_| invalid())
}

pub fn with_token(self_url: &str, token: &str) -> String {
    let (path, query) = match self_url.split_once('?') {
        Some((path, query)) => (path, query),
        None => (self_url, ""),
    };
    let mut parts: Vec<String> = query
        .split('&')
        .filter(|pair| !pair.is_empty() && !pair.starts_with("ct="))
        .map(str::to_owned)
        .collect();
    parts.push(format!("ct={token}"));
    format!("{path}?{}", parts.join("&"))
}

fn checksum(body: &str) -> u8 {
    body.bytes()
        .fold(7u8, |acc, byte| acc.wrapping_mul(31).wrapping_add(byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_round_trips_and_rejects_tampering() {
        for offset in [0usize, 1, 25, 4096] {
            assert_eq!(decode(&encode(offset)).unwrap(), offset);
        }
        let token = encode(25);
        let mangled = format!("{}f", &token[..token.len() - 1]);
        assert!(decode(&mangled).is_err());
        assert!(decode("").is_err());
        assert!(decode("zz").is_err());
    }

    #[test]
    fn a_next_link_replaces_an_existing_token() {
        let url = with_token("http://localhost/_history?_count=2&ct=abc", "ff10");
        assert_eq!(url, "http://localhost/_history?_count=2&ct=ff10");
        assert_eq!(with_token("http://localhost/_history", "ff10"), "http://localhost/_history?ct=ff10");
    }
}
