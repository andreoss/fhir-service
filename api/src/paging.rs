use fhir_core::Error;

pub const DEFAULT_SIZE: usize = 20;
pub const DEFAULT_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Counting {
    None,

    #[default]
    Accurate,
}

impl Counting {
    pub fn parse(raw: &str) -> Result<Counting, Error> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Counting::None),
            "accurate" => Ok(Counting::Accurate),
            other => Err(Error::Config(format!(
                "{other:?} is neither \"none\" nor \"accurate\""
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Counting::None => "none",
            Counting::Accurate => "accurate",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paging {
    size: usize,
    limit: usize,
    sort: Option<String>,
    total: Counting,
}

impl Default for Paging {
    fn default() -> Paging {
        Paging {
            size: DEFAULT_SIZE,
            limit: DEFAULT_LIMIT,
            sort: None,
            total: Counting::Accurate,
        }
    }
}

impl Paging {
    pub fn new(size: usize, limit: usize) -> Result<Paging, Error> {
        if size == 0 || limit == 0 {
            return Err(Error::Config(
                "a page of nothing would answer every search with an empty bundle".to_owned(),
            ));
        }
        if size > limit {
            return Err(Error::Config(format!(
                "the default page of {size} is larger than the {limit} a client may ask for"
            )));
        }
        Ok(Paging {
            size,
            limit,
            ..Paging::default()
        })
    }

    pub fn sorting_by(self, sort: Option<String>) -> Paging {
        Paging { sort, ..self }
    }

    pub fn counting(self, total: Counting) -> Paging {
        Paging { total, ..self }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn sort(&self) -> Option<&str> {
        self.sort.as_deref()
    }

    pub fn total(&self) -> Counting {
        self.total
    }

    pub fn count_of(&self, asked: Option<usize>) -> usize {
        match asked {
            Some(held) => held.min(self.limit),
            None => self.size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_what_this_build_always_served() {
        let held = Paging::default();
        assert_eq!(held.count_of(None), DEFAULT_SIZE);
        assert_eq!(held.count_of(Some(5_000)), DEFAULT_LIMIT);
        assert_eq!(held.count_of(Some(5)), 5);
        assert_eq!(held.total(), Counting::Accurate);
        assert_eq!(held.sort(), None);
    }

    #[test]
    fn a_page_of_nothing_is_refused() {
        assert!(Paging::new(0, 10).is_err());
        assert!(Paging::new(10, 0).is_err());
    }

    #[test]
    fn a_default_larger_than_the_bound_is_refused_rather_than_lowered() {
        let error = Paging::new(200, 100).expect_err("the two settings disagree");
        assert!(error.to_string().contains("200"), "{error}");
    }

    #[test]
    fn a_total_is_read_or_refused() {
        assert_eq!(Counting::parse("accurate").unwrap(), Counting::Accurate);
        assert_eq!(Counting::parse(" None ").unwrap(), Counting::None);
        assert!(Counting::parse("estimate").is_err());
    }
}
