use fhir_core::Error;

const CREDENTIAL_LIMIT: usize = 16;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Scrape {
    #[default]
    Closed,
    Guarded(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Unserved,
    Refused,
    Granted,
}

impl Scrape {
    pub fn closed() -> Scrape {
        Scrape::Closed
    }

    pub fn guarded(credential: &str) -> Result<Scrape, Error> {
        match credential.len() >= CREDENTIAL_LIMIT {
            true => Ok(Scrape::Guarded(credential.to_owned())),
            false => Err(Error::Config(
                "a scrape credential is at least sixteen characters".to_owned(),
            )),
        }
    }

    pub fn admits(&self, offered: Option<&str>) -> Admission {
        match self {
            Scrape::Closed => Admission::Unserved,
            Scrape::Guarded(held) => match offered.is_some_and(|offered| matches(held, offered)) {
                true => Admission::Granted,
                false => Admission::Refused,
            },
        }
    }

    pub fn serves(&self) -> bool {
        match self {
            Scrape::Closed => false,
            Scrape::Guarded(_) => true,
        }
    }
}

fn matches(held: &str, offered: &str) -> bool {
    let held = held.as_bytes();
    let offered = offered.as_bytes();
    let mut differing = held.len() ^ offered.len();
    for position in 0..held.len().min(offered.len()) {
        differing |= usize::from(held[position] ^ offered[position]);
    }
    differing == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREDENTIAL: &str = "a-reader-credential";

    #[test]
    fn an_unconfigured_instance_serves_nothing() {
        let closed = Scrape::default();
        assert_eq!(closed, Scrape::Closed);
        assert!(!closed.serves());
        assert_eq!(closed.admits(None), Admission::Unserved);
        assert_eq!(closed.admits(Some(CREDENTIAL)), Admission::Unserved);
    }

    #[test]
    fn a_guarded_instance_serves_only_the_credential_it_holds() {
        let guarded = Scrape::guarded(CREDENTIAL).unwrap();
        assert!(guarded.serves());
        assert_eq!(guarded.admits(Some(CREDENTIAL)), Admission::Granted);
        assert_eq!(guarded.admits(None), Admission::Refused);
        assert_eq!(guarded.admits(Some("")), Admission::Refused);
        assert_eq!(
            guarded.admits(Some("a-reader-credentia")),
            Admission::Refused
        );
        assert_eq!(
            guarded.admits(Some("a-reader-credential-and-more")),
            Admission::Refused
        );
    }

    #[test]
    fn a_credential_too_short_to_hold_is_refused_at_configuration() {
        assert!(Scrape::guarded("short").is_err());
        assert!(Scrape::guarded("").is_err());
        assert!(Scrape::guarded("0123456789abcdef").is_ok());
    }
}
