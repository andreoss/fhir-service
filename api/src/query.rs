pub fn pairs(raw: Option<&str>) -> Vec<(String, String)> {
    let Some(raw) = raw else { return Vec::new() };
    raw.split('&')
        .filter(|pair| !pair.is_empty())
        .map(split)
        .map(|(name, value)| (decode(name), decode(value)))
        .collect()
}

pub fn param(raw: Option<&str>, wanted: &str) -> Option<String> {
    raw?.split('&')
        .filter(|pair| !pair.is_empty())
        .map(split)
        .find(|(name, _)| decode(name) == wanted)
        .map(|(_, value)| decode(value))
}

fn split(pair: &str) -> (&str, &str) {
    pair.split_once('=').unwrap_or((pair, ""))
}

pub fn decoded(text: &str) -> String {
    decode(text)
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                match hex(bytes[index + 1]).zip(hex(bytes[index + 2])) {
                    Some((high, low)) => {
                        out.push(high * 16 + low);
                        index += 3;
                    }
                    None => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}




pub fn encoded(text: &str) -> String {
    let mut held = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'*' => {
                held.push(byte as char)
            }
            b' ' => held.push_str("%20"),
            other => held.push_str(&format!("%{other:02X}")),
        }
    }
    held
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_has_no_pairs() {
        assert!(pairs(None).is_empty());
        assert!(pairs(Some("")).is_empty());
    }

    #[test]
    fn pairs_are_percent_decoded() {
        let params = pairs(Some("name=de%20la%20Cruz&given=a+b"));
        assert_eq!(params[0], ("name".to_owned(), "de la Cruz".to_owned()));
        assert_eq!(params[1], ("given".to_owned(), "a b".to_owned()));
    }

    #[test]
    fn a_malformed_escape_is_kept_verbatim() {
        let params = pairs(Some("name=100%"));
        assert_eq!(params[0].1, "100%");
    }

    #[test]
    fn a_fragment_without_a_value_keeps_its_name() {
        let params = pairs(Some("nonesuch&_count=5"));
        assert_eq!(params[0], ("nonesuch".to_owned(), String::new()));
        assert_eq!(param(Some("_summary"), "_summary"), Some(String::new()));
    }

    #[test]
    fn a_named_parameter_is_read_back() {
        assert_eq!(
            param(Some("_count=5&_since=x"), "_count"),
            Some("5".to_owned())
        );
        assert_eq!(param(Some("_count=5"), "_sort"), None);
        assert_eq!(param(None, "_count"), None);
    }
}
