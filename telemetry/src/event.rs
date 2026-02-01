use std::sync::{Arc, Mutex};

use crate::dimension::Dimensions;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub dimensions: Dimensions,
    pub millis: u64,
}

impl Event {
    pub fn line(&self) -> String {
        format!(
            "operation={} outcome={} duration_ms={}",
            self.dimensions.operation.as_str(),
            self.dimensions.outcome.as_str(),
            self.millis
        )
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
        let event = Event {
            dimensions: Dimensions::of(Operation::Update, Outcome::ServerFault),
            millis: 9,
        };
        assert_eq!(
            event.line(),
            "operation=update outcome=server_fault duration_ms=9"
        );
    }
}
