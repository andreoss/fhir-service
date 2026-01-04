use fhir_store::SearchParams;

const RESULT_CONTROL: [&str; 8] = [
    "_format",
    "_pretty",
    "_summary",
    "_count",
    "_sort",
    "_elements",
    "_total",
    "ct",
];

pub fn conditional_params(raw: Option<&str>) -> SearchParams {
    let Some(raw) = raw else { return Vec::new() };
    raw.split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            let name = decode(name);
            if RESULT_CONTROL.contains(&name.as_str()) {
                return None;
            }
            Some((name, decode(value)))
        })
        .collect()
}

pub fn param(raw: Option<&str>, wanted: &str) -> Option<String> {
    raw?.split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| decode(name) == wanted)
        .map(|(_, value)| decode(value))
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
            b'%' if index + 2 < bytes.len() => match hex(bytes[index + 1]).zip(hex(bytes[index + 2])) {
                Some((high, low)) => {
                    out.push(high * 16 + low);
                    index += 3;
                }
                None => {
                    out.push(bytes[index]);
                    index += 1;
                }
            },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_has_no_parameters() {
        assert!(conditional_params(None).is_empty());
        assert!(conditional_params(Some("")).is_empty());
    }

    #[test]
    fn result_control_parameters_are_dropped() {
        assert!(conditional_params(Some("_format=json&_count=10&ct=abc")).is_empty());
    }

    #[test]
    fn pairs_are_percent_decoded() {
        let params = conditional_params(Some("name=de%20la%20Cruz&given=a+b"));
        assert_eq!(params[0], ("name".to_owned(), "de la Cruz".to_owned()));
        assert_eq!(params[1], ("given".to_owned(), "a b".to_owned()));
    }

    #[test]
    fn a_malformed_escape_is_kept_verbatim() {
        let params = conditional_params(Some("name=100%"));
        assert_eq!(params[0].1, "100%");
    }

    #[test]
    fn a_named_parameter_is_read_back() {
        assert_eq!(param(Some("_count=5&_since=x"), "_count"), Some("5".to_owned()));
        assert_eq!(param(Some("_count=5"), "_sort"), None);
        assert_eq!(param(None, "_count"), None);
    }
}
