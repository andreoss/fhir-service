use sha2::{Digest, Sha256};
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    Status,

    OutcomeIssue,

    Content,

    Concurrency,

    SearchSet,

    SearchOrder,

    BundleEntry,

    PagingLink,

    ServerIdentity,

    Representation,

    Unreached,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Blocking,

    Serious,

    Noted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allowance {
    Empty,

    Recorded,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub class: Class,

    pub severity: Severity,

    pub allowance: Allowance,

    pub because: &'static str,
}

#[derive(Clone, Debug)]
pub struct Exemption {
    pub class: Class,

    pub about: &'static str,

    pub detail: &'static str,

    pub because: &'static str,

    pub rests_on: &'static str,
}

impl Exemption {
    pub fn covers(&self, class: Class, case: &str, shape: &str, detail: &str) -> bool {
        if self.class != class {
            return false;
        }
        if !self.about.is_empty() && self.about != case && self.about != shape {
            return false;
        }
        self.detail.is_empty() || detail.contains(self.detail)
    }

    pub fn written(&self) -> String {
        format!(
            "excused: {} | {} | {} | {} | rests on {}",
            self.class.name(),
            match self.about.is_empty() {
                true => "any case",
                false => self.about,
            },
            match self.detail.is_empty() {
                true => "any difference",
                false => self.detail,
            },
            self.because,
            self.rests_on
        )
    }
}

#[derive(Clone, Debug)]
pub struct Tally {
    pub class: Class,

    pub count: usize,
}

#[derive(Clone, Debug)]
pub struct RunFacts {
    pub cases_planned: usize,

    pub cases_compared: usize,

    pub covered: Vec<String>,

    pub tallies: Vec<Tally>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breach {
    pub class: Option<Class>,

    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Passed,

    Failed(Vec<Breach>),
}

pub struct Gate {
    rules: Vec<Rule>,
    least_cases: usize,
    must_cover: Vec<&'static str>,
    exemptions: Vec<Exemption>,
}

impl Gate {
    pub fn of(rules: Vec<Rule>, least_cases: usize, must_cover: Vec<&'static str>) -> Gate {
        Gate {
            rules,
            least_cases,
            must_cover,
            exemptions: Vec::new(),
        }
    }

    pub fn excusing(mut self, exemptions: Vec<Exemption>) -> Gate {
        self.exemptions = exemptions;
        self
    }

    pub fn exemptions(&self) -> &[Exemption] {
        &self.exemptions
    }

    pub fn excusing_of(
        &self,
        class: Class,
        case: &str,
        shape: &str,
        detail: &str,
    ) -> Option<&Exemption> {
        self.exemptions
            .iter()
            .find(|excused| excused.covers(class, case, shape, detail))
    }

    pub fn agreed() -> Gate {
        Gate::of(
            vec![
                Rule {
                    class: Class::Status,
                    severity: Severity::Blocking,
                    allowance: Allowance::Empty,
                    because: "a caller branches on the response code",
                },
                Rule {
                    class: Class::OutcomeIssue,
                    severity: Severity::Blocking,
                    allowance: Allowance::Empty,
                    because: "a refusal is acted on by its issue code",
                },
                Rule {
                    class: Class::Content,
                    severity: Severity::Blocking,
                    allowance: Allowance::Empty,
                    because: "a record read back differently is a changed record",
                },
                Rule {
                    class: Class::Concurrency,
                    severity: Severity::Blocking,
                    allowance: Allowance::Empty,
                    because: "a version tag decides whether a write is refused",
                },
                Rule {
                    class: Class::SearchSet,
                    severity: Severity::Blocking,
                    allowance: Allowance::Empty,
                    because: "a missing or extra match is a missing or extra record",
                },
                Rule {
                    class: Class::BundleEntry,
                    severity: Severity::Blocking,
                    allowance: Allowance::Empty,
                    because: "an entry outcome decides what was written",
                },
                Rule {
                    class: Class::Unreached,
                    severity: Severity::Blocking,
                    allowance: Allowance::Empty,
                    because: "an unanswered request is evidence of nothing",
                },
                Rule {
                    class: Class::SearchOrder,
                    severity: Severity::Serious,
                    allowance: Allowance::Empty,
                    because: "a caller reading the first page relies on the order",
                },
                Rule {
                    class: Class::PagingLink,
                    severity: Severity::Serious,
                    allowance: Allowance::Empty,
                    because: "a walk that cannot continue loses the rest of the set",
                },
                Rule {
                    class: Class::ServerIdentity,
                    severity: Severity::Noted,
                    allowance: Allowance::Recorded,
                    because: "the software naming itself differs by definition",
                },
                Rule {
                    class: Class::Representation,
                    severity: Severity::Noted,
                    allowance: Allowance::Recorded,
                    because: "bytes carrying the same content are the same answer",
                },
            ],
            24,
            vec![
                "read",
                "vread",
                "read-missing",
                "read-deleted",
                "create",
                "create-rejected",
                "update",
                "update-stale",
                "conditional-update",
                "conditional-create",
                "delete",
                "delete-missing",
                "history",
                "search",
                "search-sorted",
                "search-paging",
                "search-unknown-parameter",
                "bundle-transaction",
                "bundle-transaction-rolled-back",
                "bundle-batch",
                "capability",
            ],
        )
        .excusing(vec![
            Exemption {
                class: Class::Status,
                about: "search-unknown-parameter",
                detail: "",
                because: "a parameter this service does not know is refused, never scanned as though it were understood",
                rests_on: "SRCH-09",
            },
            Exemption {
                class: Class::OutcomeIssue,
                about: "search-unknown-parameter",
                detail: "",
                because: "the refusal of an unknown parameter names it unsupported rather than a generic failure",
                rests_on: "SRCH-09",
            },
            Exemption {
                class: Class::OutcomeIssue,
                about: "",
                detail: "against [\"error/processing\"]",
                because: "a refusal names the published code for what failed, where the other side names one generic code for every refusal",
                rests_on: "REST-01, REST-03, REST-09",
            },
            Exemption {
                class: Class::OutcomeIssue,
                about: "delete-missing",
                detail: "",
                because: "a delete of what is not there answers no content, so it carries no outcome to compare",
                rests_on: "REST-06",
            },
            Exemption {
                class: Class::Status,
                about: "delete",
                detail: "answered 204 against 200",
                because: "a delete answers no content, which the released specifications allow beside 200",
                rests_on: "REST-06",
            },
            Exemption {
                class: Class::Status,
                about: "delete-missing",
                detail: "answered 204 against 200",
                because: "a delete of what is not there answers as a delete of what is",
                rests_on: "REST-06",
            },
            Exemption {
                class: Class::Content,
                about: "delete",
                detail: "content-location named None against",
                because: "an answer carrying no body names no representation of one",
                rests_on: "REST-06",
            },
            Exemption {
                class: Class::Content,
                about: "delete-missing",
                detail: "content-location named None against",
                because: "an answer carrying no body names no representation of one",
                rests_on: "REST-06",
            },
            Exemption {
                class: Class::Content,
                about: "",
                detail: "location named Some(",
                because: "a create says where it put the record, which the other side leaves to a content location alone",
                rests_on: "REST-02, REST-10",
            },
            Exemption {
                class: Class::Content,
                about: "",
                detail: "\"meta\":{}",
                because: "an element with neither value nor children is no part of a resource",
                rests_on: "MDL-01, VAL-02",
            },
            Exemption {
                class: Class::Status,
                about: "read-rolled-back",
                detail: "answered 404 against 200",
                because: "a transaction with one failing entry writes nothing, so nothing is there to read",
                rests_on: "BNDL-01",
            },
            Exemption {
                class: Class::Concurrency,
                about: "read-rolled-back",
                detail: "",
                because: "a transaction with one failing entry writes nothing, so there is no version to tag",
                rests_on: "BNDL-01",
            },
            Exemption {
                class: Class::Content,
                about: "read-rolled-back",
                detail: "",
                because: "a transaction with one failing entry writes nothing, so there is nothing to name",
                rests_on: "BNDL-01",
            },
            Exemption {
                class: Class::Status,
                about: "transaction-rolled-back",
                detail: "answered 400 against 200",
                because: "an entry whose body contradicts the target it names is refused, and one failing entry rolls the whole transaction back",
                rests_on: "BNDL-01, MDL-02",
            },
            Exemption {
                class: Class::Concurrency,
                about: "delete",
                detail: "etag was offered by one side only",
                because: "a soft delete writes a version, and the answer tags the version it wrote",
                rests_on: "REST-06",
            },
            Exemption {
                class: Class::BundleEntry,
                about: "",
                detail: "information/informational",
                because: "an entry outcome reports a failure by its published code, and a written entry reports none",
                rests_on: "BNDL-04, REST-09",
            },
            Exemption {
                class: Class::PagingLink,
                about: "bundle-transaction",
                detail: "offered [] against",
                because: "a transaction answer is per-entry outcomes, not a set with pages to walk",
                rests_on: "BNDL-02, BNDL-04",
            },
            Exemption {
                class: Class::PagingLink,
                about: "bundle-batch",
                detail: "offered [] against",
                because: "a batch answer is per-entry outcomes, not a set with pages to walk",
                rests_on: "BNDL-02, BNDL-04",
            },
        ])
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn least_cases(&self) -> usize {
        self.least_cases
    }

    pub fn must_cover(&self) -> &[&'static str] {
        &self.must_cover
    }

    pub fn rule(&self, class: Class) -> &Rule {
        self.rules
            .iter()
            .find(|rule| rule.class == class)
            .expect("every class is governed")
    }

    pub fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.terms().as_bytes());
        let mut hex = String::with_capacity(64);
        for byte in digest.finalize() {
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    }

    pub fn render(&self) -> String {
        format!("acceptance gate {}\n{}", self.fingerprint(), self.terms())
    }

    fn terms(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(text, "least compared shapes: {}", self.least_cases);
        for shape in &self.must_cover {
            let _ = writeln!(text, "must exercise: {shape}");
        }
        for rule in &self.rules {
            let _ = writeln!(
                text,
                "{} | {} | {} | {}",
                rule.class.name(),
                rule.severity.name(),
                match rule.allowance {
                    Allowance::Empty => "must be empty",
                    Allowance::Recorded => "recorded only",
                },
                rule.because
            );
        }
        for excused in &self.exemptions {
            let _ = writeln!(text, "{}", excused.written());
        }
        text
    }

    pub fn judge(&self, run: &RunFacts) -> Verdict {
        let mut breaches = Vec::new();
        if run.cases_compared < self.least_cases {
            breaches.push(Breach {
                class: None,
                reason: format!(
                    "{} shapes were compared, {} were agreed as the least",
                    run.cases_compared, self.least_cases
                ),
            });
        }
        if run.cases_compared < run.cases_planned {
            breaches.push(Breach {
                class: None,
                reason: format!(
                    "{} of {} planned shapes did not reach both sides",
                    run.cases_planned - run.cases_compared,
                    run.cases_planned
                ),
            });
        }
        for shape in &self.must_cover {
            if !run.covered.iter().any(|done| done == shape) {
                breaches.push(Breach {
                    class: None,
                    reason: format!("the agreed shape {shape} was not exercised"),
                });
            }
        }
        for rule in &self.rules {
            if rule.allowance != Allowance::Empty {
                continue;
            }
            let found: usize = run
                .tallies
                .iter()
                .filter(|tally| tally.class == rule.class)
                .map(|tally| tally.count)
                .sum();
            if found > 0 {
                breaches.push(Breach {
                    class: Some(rule.class),
                    reason: format!(
                        "{} holds {found} divergences and was agreed to be empty: {}",
                        rule.class.name(),
                        rule.because
                    ),
                });
            }
        }
        if breaches.is_empty() {
            Verdict::Passed
        } else {
            Verdict::Failed(breaches)
        }
    }
}

impl Class {
    pub const ALL: [Class; 11] = [
        Class::Status,
        Class::OutcomeIssue,
        Class::Content,
        Class::Concurrency,
        Class::SearchSet,
        Class::SearchOrder,
        Class::BundleEntry,
        Class::PagingLink,
        Class::ServerIdentity,
        Class::Representation,
        Class::Unreached,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Class::Status => "status",
            Class::OutcomeIssue => "outcome-issue",
            Class::Content => "content",
            Class::Concurrency => "concurrency",
            Class::SearchSet => "search-set",
            Class::SearchOrder => "search-order",
            Class::BundleEntry => "bundle-entry",
            Class::PagingLink => "paging-link",
            Class::ServerIdentity => "server-identity",
            Class::Representation => "representation",
            Class::Unreached => "unreached",
        }
    }
}

impl Severity {
    pub fn name(&self) -> &'static str {
        match self {
            Severity::Blocking => "blocking",
            Severity::Serious => "serious",
            Severity::Noted => "noted",
        }
    }
}
