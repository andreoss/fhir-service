use fhir_core::Error;


pub const MOST_CONDITIONAL_DELETE: usize = 10_000;




#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConditionalDelete {
    
    
    #[default]
    Single,
    
    
    Multiple(usize),
}

impl ConditionalDelete {
    
    pub fn parse(raw: &str) -> Result<ConditionalDelete, Error> {
        let held = raw.trim();
        if held.eq_ignore_ascii_case("single") {
            return Ok(ConditionalDelete::Single);
        }
        let (name, count) = match held.split_once(':') {
            None => (held, None),
            Some((name, count)) => (name, Some(count.trim())),
        };
        if !name.trim().eq_ignore_ascii_case("multiple") {
            return Err(Error::Config(format!(
                "conditional delete {held:?} names neither single nor multiple"
            )));
        }
        let most = match count {
            None => MOST_CONDITIONAL_DELETE,
            Some(raw) => raw
                .parse::<usize>()
                .ok()
                .filter(|held| (1..=MOST_CONDITIONAL_DELETE).contains(held))
                .ok_or_else(|| {
                    Error::Config(format!(
                        "conditional delete count {raw:?} is not 1 to {MOST_CONDITIONAL_DELETE}"
                    ))
                })?,
        };
        Ok(ConditionalDelete::Multiple(most))
    }

    
    pub fn as_str(&self) -> &'static str {
        match self {
            ConditionalDelete::Single => "single",
            ConditionalDelete::Multiple(_) => "multiple",
        }
    }

    pub fn most(&self) -> usize {
        match self {
            ConditionalDelete::Single => 1,
            ConditionalDelete::Multiple(most) => *most,
        }
    }
}



#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub conditional_delete: ConditionalDelete,
    
    
    
    pub create_on_update: bool,
    
    pub include_depth: usize,
}

impl Default for Capabilities {
    fn default() -> Capabilities {
        Capabilities {
            conditional_delete: ConditionalDelete::Single,
            create_on_update: true,
            include_depth: fhir_store::DEFAULT_INCLUDE_DEPTH,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_configured_refuses_a_condition_that_matches_more_than_one() {
        let held = Capabilities::default();
        assert_eq!(held.conditional_delete, ConditionalDelete::Single);
        assert_eq!(held.conditional_delete.as_str(), "single");
        assert_eq!(held.conditional_delete.most(), 1);
        assert!(held.create_on_update);
    }

    #[test]
    fn multiple_takes_every_match_up_to_a_bound() {
        assert_eq!(
            ConditionalDelete::parse("multiple").unwrap(),
            ConditionalDelete::Multiple(MOST_CONDITIONAL_DELETE)
        );
        let held = ConditionalDelete::parse("multiple:25").unwrap();
        assert_eq!(held, ConditionalDelete::Multiple(25));
        assert_eq!(held.as_str(), "multiple");
        assert_eq!(held.most(), 25);
    }

    #[test]
    fn single_is_read_back() {
        assert_eq!(
            ConditionalDelete::parse(" Single ").unwrap(),
            ConditionalDelete::Single
        );
    }

    #[test]
    fn a_setting_that_names_neither_is_refused() {
        assert!(ConditionalDelete::parse("some").is_err());
        assert!(ConditionalDelete::parse("multiple:0").is_err());
        assert!(ConditionalDelete::parse("multiple:many").is_err());
        assert!(ConditionalDelete::parse("multiple:100000").is_err());
    }
}
