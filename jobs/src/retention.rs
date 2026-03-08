







use fhir_core::{Error, FhirVersion, ResourceType};
use fhir_store::{system_ticker, JobId, JobKind, JobRequest, JobStore, Ticker};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

const MILLIS_PER_DAY: i64 = 24 * 60 * 60 * 1_000;


#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Removal {
    
    #[default]
    Soft,
    
    Purge,
}

impl Removal {
    fn as_str(&self) -> &'static str {
        match self {
            Removal::Soft => "soft",
            Removal::Purge => "purge",
        }
    }
}



#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub resource_type: ResourceType,
    pub days: i64,
    pub removal: Removal,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Retention {
    rules: Vec<Rule>,
    every: i64,
}



#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Swept {
    pub submitted: Vec<(ResourceType, String)>,
}

impl Swept {
    pub fn is_empty(&self) -> bool {
        self.submitted.is_empty()
    }
}

impl Retention {
    pub const DEFAULT_EVERY: i64 = 4 * 60 * 60 * 1_000;

    
    
    
    
    pub fn parse(raw: &str, version: FhirVersion) -> Result<Retention, Error> {
        let mut held = Retention {
            rules: Vec::new(),
            every: Retention::DEFAULT_EVERY,
        };
        for part in raw
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let (name, rest) = part.split_once('=').ok_or_else(|| {
                Error::Config(format!(
                    "retention {part:?} names no window; write Type=days"
                ))
            })?;
            let (days, removal) = match rest.split_once(':') {
                None => (rest, "soft"),
                Some((days, removal)) => (days, removal),
            };
            let resource_type = name.trim().parse::<ResourceType>().map_err(|_| {
                Error::Config(format!(
                    "retention names {:?}, which is no resource type",
                    name.trim()
                ))
            })?;
            if !ResourceType::served(version).contains(&resource_type) {
                return Err(Error::Config(format!(
                    "retention names {resource_type}, which {version} does not serve"
                )));
            }
            let days = days
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|held| *held > 0)
                .ok_or_else(|| {
                    Error::Config(format!(
                        "retention window {:?} is not a positive number of days",
                        days.trim()
                    ))
                })?;
            let removal = match removal.trim() {
                "soft" => Removal::Soft,
                "purge" => Removal::Purge,
                other => {
                    return Err(Error::Config(format!(
                        "retention removal {other:?} names neither soft nor purge"
                    )))
                }
            };
            held.rules.push(Rule {
                resource_type,
                days,
                removal,
            });
        }
        Ok(held)
    }

    pub fn every(mut self, millis: i64) -> Retention {
        self.every = millis.max(1);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn period(&self) -> i64 {
        self.every
    }

    
    
    pub fn payload(&self, rule: &Rule, cutoff: &str) -> Value {
        json!({
            "_type": [rule.resource_type.as_str()],
            "_before": cutoff,
            "hardDelete": false,
            "purgeHistory": matches!(rule.removal, Removal::Purge),
            "softDeleted": false,
            "_rule": format!("{} kept {} days, removed {}", rule.resource_type, rule.days, rule.removal.as_str()),
        })
    }
}




pub struct RetentionWorker {
    jobs: Arc<dyn JobStore>,
    retention: Retention,
    ticker: Ticker,
    swept_at: AtomicI64,
}

impl RetentionWorker {
    pub fn new(jobs: Arc<dyn JobStore>, retention: Retention) -> RetentionWorker {
        RetentionWorker {
            jobs,
            retention,
            ticker: system_ticker(),
            swept_at: AtomicI64::new(i64::MIN),
        }
    }

    pub fn with_ticker(self, ticker: Ticker) -> RetentionWorker {
        RetentionWorker { ticker, ..self }
    }

    fn due(&self, now: i64) -> bool {
        let last = self.swept_at.load(Ordering::SeqCst);
        match last == i64::MIN || now - last >= self.retention.period() {
            true => {
                self.swept_at.store(now, Ordering::SeqCst);
                true
            }
            false => false,
        }
    }

    pub async fn sweep(&self) -> Result<Swept, Error> {
        if self.retention.is_empty() {
            return Ok(Swept::default());
        }
        let now = (self.ticker)();
        if !self.due(now) {
            return Ok(Swept::default());
        }
        let mut swept = Swept::default();
        for rule in self.retention.rules() {
            let cutoff = instant_at(now - rule.days * MILLIS_PER_DAY)?;
            let payload = self.retention.payload(rule, &cutoff);
            let id = JobId::parse(&format!(
                "retention-{}-{now}",
                rule.resource_type.as_str().to_ascii_lowercase()
            ))?;
            let request = JobRequest::new(id, JobKind::BulkDelete, payload.to_string());
            self.jobs.submit(request).await?;
            swept.submitted.push((rule.resource_type, cutoff));
        }
        Ok(swept)
    }
}

fn instant_at(millis: i64) -> Result<String, Error> {
    let seconds = millis.div_euclid(1_000);
    let remainder = millis.rem_euclid(1_000);
    let held = time::OffsetDateTime::from_unix_timestamp(seconds)
        .map_err(|_| Error::InvalidInstant(format!("{millis} is no instant")))?
        + time::Duration::milliseconds(remainder);
    held.format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| Error::InvalidInstant(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("a served type")
    }

    #[test]
    fn nothing_configured_removes_nothing() {
        let held = Retention::default();
        assert!(held.is_empty());
    }

    #[test]
    fn a_rule_is_read_with_its_window_and_its_removal() {
        let held =
            Retention::parse("AuditEvent=90;Observation=3650:purge", FhirVersion::R4).unwrap();
        assert_eq!(held.rules().len(), 2);
        assert_eq!(held.rules()[0].resource_type, kind("AuditEvent"));
        assert_eq!(held.rules()[0].days, 90);
        assert_eq!(held.rules()[0].removal, Removal::Soft);
        assert_eq!(held.rules()[1].removal, Removal::Purge);
    }

    #[test]
    fn an_unknown_type_is_refused_at_startup() {
        assert!(Retention::parse("Nonesuch=30", FhirVersion::R4).is_err());
    }

    #[test]
    fn a_type_the_release_does_not_serve_is_refused_at_startup() {
        let error = Retention::parse("Citation=30", FhirVersion::Stu3).unwrap_err();
        assert!(error.to_string().contains("does not serve"), "{error}");
    }

    #[test]
    fn a_window_that_is_not_days_is_refused_at_startup() {
        assert!(Retention::parse("AuditEvent=never", FhirVersion::R4).is_err());
        assert!(Retention::parse("AuditEvent=0", FhirVersion::R4).is_err());
        assert!(Retention::parse("AuditEvent=-5", FhirVersion::R4).is_err());
    }

    #[test]
    fn an_unknown_removal_is_refused_at_startup() {
        let error = Retention::parse("AuditEvent=30:shred", FhirVersion::R4).unwrap_err();
        assert!(error.to_string().contains("shred"), "{error}");
    }

    #[test]
    fn a_rule_naming_no_window_is_refused_at_startup() {
        assert!(Retention::parse("AuditEvent", FhirVersion::R4).is_err());
    }

    #[test]
    fn a_payload_names_the_type_the_cutoff_and_the_rule() {
        let held = Retention::parse("AuditEvent=90:purge", FhirVersion::R4).unwrap();
        let payload = held.payload(&held.rules()[0], "2026-06-16T00:00:00Z");
        assert_eq!(payload["_type"][0], "AuditEvent");
        assert_eq!(payload["_before"], "2026-06-16T00:00:00Z");
        assert_eq!(payload["purgeHistory"], true);
        assert_eq!(payload["hardDelete"], false);
        assert!(
            payload["_rule"].as_str().unwrap().contains("90 days"),
            "{payload}"
        );
    }

    #[test]
    fn an_instant_is_written_back_as_one() {
        let held = instant_at(1_780_000_000_000).unwrap();
        assert!(held.starts_with("20"), "{held}");
        assert!(held.ends_with('Z') || held.contains('+'), "{held}");
    }
}
