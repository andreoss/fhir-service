use fhir_core::Error;

pub const KINDS: [&str; 7] = [
    "token",
    "text",
    "number",
    "date",
    "quantity",
    "reference",
    "uri",
];

pub const BACKENDS: [&str; 2] = ["relational", "document"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extra {
    pub backend: String,
    pub kind: String,
    pub param: String,
}

impl Extra {
    pub fn name(&self) -> String {
        let folded: String = self
            .param
            .chars()
            .map(|held| match held.is_ascii_alphanumeric() {
                true => held.to_ascii_lowercase(),
                false => '_',
            })
            .collect();
        let trimmed: String = folded.chars().take(40).collect();
        format!("tune_{}_{trimmed}", self.kind)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tuning {
    extras: Vec<Extra>,
}

impl Tuning {
    pub fn parse(text: &str, known: impl Fn(&str) -> bool) -> Result<Tuning, Error> {
        let mut extras: Vec<Extra> = Vec::new();
        for (at, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let refused =
                |reason: &str| Error::Config(format!("index line {}: {line:?} {reason}", at + 1));
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [backend, kind, param] = fields.as_slice() else {
                return Err(refused("is not a backend, a kind and a parameter"));
            };
            if !BACKENDS.contains(backend) {
                return Err(refused("names no backend this build carries"));
            }
            if !KINDS.contains(kind) {
                return Err(refused("names no index this store carries"));
            }
            if !known(param) {
                return Err(refused("names no search parameter this release publishes"));
            }
            let held = Extra {
                backend: (*backend).to_owned(),
                kind: (*kind).to_owned(),
                param: (*param).to_owned(),
            };
            if !extras.contains(&held) {
                extras.push(held);
            }
        }
        Ok(Tuning { extras })
    }

    pub fn behind(&self, backend: &str) -> Vec<Extra> {
        self.extras
            .iter()
            .filter(|extra| extra.backend == backend)
            .cloned()
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.extras.is_empty()
    }

    pub fn len(&self) -> usize {
        self.extras.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published(param: &str) -> bool {
        ["subject", "code", "date", "identifier"].contains(&param)
    }

    #[test]
    fn a_file_names_the_indexes_an_operator_wants() {
        let held = Tuning::parse(
            "# the ones this deployment asks for\nrelational token code\n\ndocument reference subject\n",
            published,
        )
        .expect("the file reads");
        assert_eq!(held.len(), 2);
        assert_eq!(
            held.behind("relational"),
            vec![Extra {
                backend: "relational".to_owned(),
                kind: "token".to_owned(),
                param: "code".to_owned(),
            }]
        );
        assert_eq!(held.behind("document").len(), 1);
        assert_eq!(held.behind("memory").len(), 0);
    }

    #[test]
    fn an_absent_file_asks_for_nothing() {
        let held = Tuning::parse("", published).expect("an empty file reads");
        assert!(held.is_empty());
        assert_eq!(Tuning::default().len(), 0);
    }

    #[test]
    fn the_same_line_twice_asks_once() {
        let held = Tuning::parse("relational token code\nrelational token code\n", published)
            .expect("the file reads");
        assert_eq!(held.len(), 1);
    }

    #[test]
    fn a_line_the_store_cannot_honour_is_refused_by_its_number() {
        for (text, says) in [
            ("relational token code\nrelational sideways code\n", "index"),
            ("relational token nonesuch\n", "search parameter"),
            ("sideways token code\n", "backend"),
            ("relational token\n", "parameter"),
        ] {
            let error = Tuning::parse(text, published).expect_err(text);
            let Error::Config(told) = &error else {
                panic!("{text} gave {error:?}");
            };
            assert!(told.contains(says), "{told}");
            assert!(told.starts_with("index line"), "{told}");
        }
        let error = Tuning::parse(
            "relational token code\nrelational sideways code\n",
            published,
        )
        .expect_err("the second line is refused");
        let Error::Config(told) = &error else {
            panic!("{error:?}");
        };
        assert!(told.contains("line 2"), "{told}");
    }
}
