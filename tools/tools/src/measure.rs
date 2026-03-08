use fhir_core::Error;
use serde_json::{json, Value};

const BAND: f64 = 0.05;

fn rounded(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}

fn ranked(sorted: &[u64], part: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (part * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

fn number(value: &Value, name: &str) -> Result<u64, Error> {
    value
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::InvalidParameter(format!("{name} is missing from the record")))
}

fn text(value: &Value, name: &str) -> Result<String, Error> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidParameter(format!("{name} is missing from the record")))
}

pub struct Run {
    label: String,
    operation: String,
    samples: Vec<u64>,
    wall: Option<u64>,
}

impl Run {
    pub fn new(label: impl Into<String>, operation: impl Into<String>, samples: Vec<u64>) -> Run {
        Run {
            label: label.into(),
            operation: operation.into(),
            samples,
            wall: None,
        }
    }

    pub fn over(
        label: impl Into<String>,
        operation: impl Into<String>,
        samples: Vec<u64>,
        wall: u64,
    ) -> Run {
        Run {
            label: label.into(),
            operation: operation.into(),
            samples,
            wall: Some(wall),
        }
    }

    pub fn summary(&self) -> Summary {
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let micros: u64 = sorted.iter().sum();
        let elapsed = self.wall.unwrap_or(micros);
        let seconds = elapsed as f64 / 1_000_000.0;
        Summary {
            label: self.label.clone(),
            operation: self.operation.clone(),
            count: sorted.len() as u64,
            micros,
            wall_micros: elapsed,
            per_second: match seconds > 0.0 {
                true => rounded(sorted.len() as f64 / seconds),
                false => 0.0,
            },
            p50: ranked(&sorted, 0.50),
            p95: ranked(&sorted, 0.95),
            p99: ranked(&sorted, 0.99),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub label: String,
    pub operation: String,
    pub count: u64,
    pub micros: u64,
    pub wall_micros: u64,
    pub per_second: f64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
}

impl Summary {
    pub fn to_value(&self) -> Value {
        json!({
            "label": self.label,
            "operation": self.operation,
            "count": self.count,
            "micros": self.micros,
            "wallMicros": self.wall_micros,
            "perSecond": self.per_second,
            "p50": self.p50,
            "p95": self.p95,
            "p99": self.p99,
        })
    }

    pub fn to_json(&self) -> String {
        self.to_value().to_string()
    }

    pub fn of(value: &Value) -> Result<Summary, Error> {
        Ok(Summary {
            label: text(value, "label")?,
            operation: text(value, "operation")?,
            count: number(value, "count")?,
            micros: number(value, "micros")?,
            wall_micros: value
                .get("wallMicros")
                .and_then(Value::as_u64)
                .unwrap_or(number(value, "micros")?),
            per_second: value
                .get("perSecond")
                .and_then(Value::as_f64)
                .unwrap_or_default(),
            p50: number(value, "p50")?,
            p95: number(value, "p95")?,
            p99: number(value, "p99")?,
        })
    }

    pub fn parse(raw: &str) -> Result<Summary, Error> {
        let value: Value =
            serde_json::from_str(raw).map_err(|error| Error::InvalidJson(error.to_string()))?;
        Summary::of(&value)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub runs: Vec<Summary>,
}

impl Record {
    pub fn new(runs: Vec<Summary>) -> Record {
        Record { runs }
    }

    pub fn to_json(&self) -> String {
        let runs: Vec<Value> = self.runs.iter().map(Summary::to_value).collect();
        json!({ "runs": runs }).to_string()
    }

    pub fn parse(raw: &str) -> Result<Record, Error> {
        let value: Value =
            serde_json::from_str(raw).map_err(|error| Error::InvalidJson(error.to_string()))?;
        let held = value
            .get("runs")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::InvalidParameter("runs is missing from the record".to_owned()))?;
        Ok(Record {
            runs: held
                .iter()
                .map(Summary::of)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub left: String,
    pub right: String,
    pub operation: String,
    pub ratio: f64,
    pub verdict: String,
}

impl Comparison {
    pub fn to_value(&self) -> Value {
        json!({
            "left": self.left,
            "right": self.right,
            "operation": self.operation,
            "ratio": self.ratio,
            "verdict": self.verdict,
        })
    }
}

pub fn compare(left: &Summary, right: &Summary) -> Result<Comparison, Error> {
    if left.operation != right.operation {
        return Err(Error::InvalidParameter(format!(
            "{} and {} measured different operations",
            left.operation, right.operation
        )));
    }
    if left.count != right.count {
        return Err(Error::InvalidParameter(format!(
            "{} and {} measured different amounts of work",
            left.count, right.count
        )));
    }
    let ratio = match left.per_second > 0.0 {
        true => right.per_second / left.per_second,
        false => 0.0,
    };
    let verdict = if ratio > 1.0 + BAND {
        "faster"
    } else if ratio < 1.0 - BAND {
        "slower"
    } else {
        "level"
    };
    Ok(Comparison {
        left: left.label.clone(),
        right: right.label.clone(),
        operation: left.operation.clone(),
        ratio,
        verdict: verdict.to_owned(),
    })
}

pub fn compare_records(left: &Record, right: &Record) -> Result<Vec<Comparison>, Error> {
    let mut judged = Vec::new();
    for summary in &left.runs {
        let Some(other) = right
            .runs
            .iter()
            .find(|held| held.operation == summary.operation)
        else {
            continue;
        };
        judged.push(compare(summary, other)?);
    }
    match judged.is_empty() {
        true => Err(Error::InvalidParameter(
            "the records share no operation".to_owned(),
        )),
        false => Ok(judged),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_run_reports_nothing_rather_than_dividing() {
        let summary = Run::new("none", "create", Vec::new()).summary();
        assert_eq!(summary.count, 0);
        assert_eq!(summary.per_second, 0.0);
        assert_eq!(summary.p50, 0);
    }

    #[test]
    fn a_record_round_trips() {
        let record = Record::new(vec![
            Run::new("one", "write", vec![10, 20]).summary(),
            Run::new("one", "read", vec![5, 6]).summary(),
        ]);
        let read = Record::parse(&record.to_json()).expect("a record reads back");
        assert_eq!(read, record);
        assert!(Record::parse("{}").is_err());
        assert!(Record::parse("nonsense").is_err());
        assert!(Summary::parse("{\"label\":\"one\"}").is_err());
    }

    #[test]
    fn records_sharing_no_operation_are_refused() {
        let left = Record::new(vec![Run::new("one", "write", vec![10]).summary()]);
        let right = Record::new(vec![Run::new("two", "read", vec![10]).summary()]);
        assert!(compare_records(&left, &right).is_err());
        let matched = Record::new(vec![Run::new("two", "write", vec![5]).summary()]);
        let judged = compare_records(&left, &matched).expect("one shared operation compares");
        assert_eq!(judged.len(), 1);
        assert_eq!(judged[0].verdict, "faster");
        assert!(judged[0].to_value().get("ratio").is_some());
    }

    #[test]
    fn a_slower_run_is_named_slower() {
        let left = Run::new("one", "write", vec![10; 4]).summary();
        let right = Run::new("two", "write", vec![40; 4]).summary();
        assert_eq!(compare(&left, &right).expect("one shape").verdict, "slower");
    }
}
