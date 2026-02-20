pub const KEY_LIMIT: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexKey {
    key: String,
    overflow: Option<String>,
}

impl IndexKey {
    pub fn of(text: &str) -> IndexKey {
        match text.char_indices().nth(KEY_LIMIT) {
            None => IndexKey {
                key: text.to_owned(),
                overflow: None,
            },
            Some((cut, _)) => IndexKey {
                key: text[..cut].to_owned(),
                overflow: Some(text[cut..].to_owned()),
            },
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn overflows(&self) -> bool {
        self.overflow.is_some()
    }

    pub fn overflow(&self) -> Option<&str> {
        self.overflow.as_deref()
    }

    pub fn matches(&self, text: &str) -> bool {
        let other = IndexKey::of(text);
        self.key == other.key && self.overflow == other.overflow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_at_the_limit_still_fits_the_key() {
        let text = "x".repeat(KEY_LIMIT);
        let key = IndexKey::of(&text);
        assert!(!key.overflows());
        assert_eq!(key.overflow(), None);
        assert_eq!(key.key(), text);
        assert!(key.matches(&text));
    }

    #[test]
    fn a_value_past_the_limit_carries_its_overflow() {
        let text = "x".repeat(KEY_LIMIT + 3);
        let key = IndexKey::of(&text);
        assert_eq!(key.key().chars().count(), KEY_LIMIT);
        assert_eq!(key.overflow(), Some("xxx"));
        assert!(key.matches(&text));
        assert!(!key.matches(&"x".repeat(KEY_LIMIT + 2)));
    }

    #[test]
    fn the_bounded_key_is_what_a_backend_indexes_and_never_what_it_compares_alone() {
        let shared = "y".repeat(KEY_LIMIT);
        let one = IndexKey::of(&format!("{shared}first"));
        let other = IndexKey::of(&format!("{shared}second"));
        assert_eq!(one.key(), other.key());
        assert_ne!(one, other);
        assert!(!one.matches(&format!("{shared}second")));
    }

    #[test]
    fn an_empty_value_indexes_to_an_empty_key() {
        let key = IndexKey::of("");
        assert_eq!(key.key(), "");
        assert!(!key.overflows());
        assert!(key.matches(""));
        assert!(!key.matches("x"));
    }
}
