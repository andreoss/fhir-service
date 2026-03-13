use std::sync::{Arc, Mutex};

use crate::dimension::Dimensions;
use fhir_core::CorrelationId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub dimensions: Dimensions,
    pub millis: u64,
    pub correlation: Option<CorrelationId>,
}

impl Event {
    pub fn of(dimensions: Dimensions, millis: u64) -> Event {
        Event {
            dimensions,
            millis,
            correlation: None,
        }
    }

    pub fn tied(self, correlation: Option<CorrelationId>) -> Event {
        Event {
            correlation,
            ..self
        }
    }

    pub fn line(&self) -> String {
        let line = format!(
            "operation={} outcome={} duration_ms={}",
            self.dimensions.operation.as_str(),
            self.dimensions.outcome.as_str(),
            self.millis
        );
        match &self.correlation {
            Some(correlation) => format!("{line} correlation={correlation}"),
            None => line,
        }
    }
}

pub trait Sink: Send + Sync {
    fn write(&self, line: &str);
}

#[derive(Debug, Default)]
pub struct Stream;

impl Sink for Stream {
    fn write(&self, line: &str) {
        eprintln!("{line}");
    }
}

#[derive(Debug, Default)]
pub struct Silent;

impl Sink for Silent {
    fn write(&self, _line: &str) {}
}

#[derive(Debug, Default, Clone)]
pub struct Held(Arc<Mutex<Vec<String>>>);

impl Held {
    pub fn sink(&self) -> Arc<dyn Sink> {
        Arc::new(Held(Arc::clone(&self.0)))
    }

    pub fn lines(&self) -> Vec<String> {
        match self.0.lock() {
            Ok(held) => held.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Sink for Held {
    fn write(&self, line: &str) {
        match self.0.lock() {
            Ok(mut held) => held.push(line.to_owned()),
            Err(poisoned) => poisoned.into_inner().push(line.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dimension::{Operation, Outcome};

    #[test]
    fn a_held_sink_reads_back_what_was_written() {
        let held = Held::default();
        held.sink().write("operation=read");
        assert_eq!(held.lines(), vec!["operation=read".to_owned()]);
    }

    #[test]
    fn a_silent_sink_keeps_nothing() {
        Silent.write("operation=read");
    }

    #[test]
    fn a_line_names_the_dimensions_and_the_duration() {
        let event = Event::of(Dimensions::of(Operation::Update, Outcome::ServerFault), 9);
        assert_eq!(
            event.line(),
            "operation=update outcome=server_fault duration_ms=9"
        );
        let tied = event.tied(Some(
            CorrelationId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        ));
        assert_eq!(
            tied.line(),
            "operation=update outcome=server_fault duration_ms=9 correlation=0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn an_event_holds_no_correlation_until_it_is_tied() {
        let event = Event::of(Dimensions::of(Operation::Read, Outcome::Success), 1);
        assert!(event.correlation.is_none());
        assert!(!event.line().contains("correlation"));
    }
}

pub trait Traces: Send + Sync {
    fn span(&self, event: &Event);
}

#[derive(Debug, Default, Clone)]
pub struct Traced(Arc<Mutex<Vec<Event>>>);

impl Traced {
    pub fn traces(&self) -> Arc<dyn Traces> {
        Arc::new(Traced(Arc::clone(&self.0)))
    }

    pub fn spans(&self) -> Vec<Event> {
        match self.0.lock() {
            Ok(held) => held.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Traces for Traced {
    fn span(&self, event: &Event) {
        match self.0.lock() {
            Ok(mut held) => held.push(event.clone()),
            Err(poisoned) => poisoned.into_inner().push(event.clone()),
        }
    }
}

pub trait Alarm: Send + Sync {
    fn raise(&self, line: &str) -> Result<(), String>;
}

#[derive(Debug, Default, Clone)]
pub struct Recorded(Arc<Mutex<Vec<String>>>);

impl Recorded {
    pub fn alarm(&self) -> Arc<dyn Alarm> {
        Arc::new(Recorded(Arc::clone(&self.0)))
    }

    pub fn raised(&self) -> Vec<String> {
        match self.0.lock() {
            Ok(held) => held.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Alarm for Recorded {
    fn raise(&self, line: &str) -> Result<(), String> {
        match self.0.lock() {
            Ok(mut held) => held.push(line.to_owned()),
            Err(poisoned) => poisoned.into_inner().push(line.to_owned()),
        }
        Ok(())
    }
}
