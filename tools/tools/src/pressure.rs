use crate::generate;
use crate::http;
use crate::measure::{Record, Run, Summary};
use fhir_core::Error;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WINDOWS: usize = 5;
const DRIFT: f64 = 1.5;
const HELD: u16 = 409;

#[derive(Debug, Clone)]
pub struct Plan {
    pub label: String,
    pub concurrency: usize,
    pub subjects: usize,
    pub seconds: u64,
    pub seed: u64,
}

impl Plan {
    pub fn parse(label: &str, concurrency: &str, amount: &str, seed: u64) -> Result<Plan, Error> {
        let concurrency = concurrency
            .parse::<usize>()
            .map_err(|_| Error::InvalidParameter(format!("concurrency {concurrency:?}")))?;
        let amount = amount
            .parse::<u64>()
            .map_err(|_| Error::InvalidParameter(format!("amount {amount:?}")))?;
        if concurrency == 0 || amount == 0 {
            return Err(Error::InvalidParameter(
                "a run needs at least one caller and one unit of work".to_owned(),
            ));
        }
        Ok(Plan {
            label: label.to_owned(),
            concurrency,
            subjects: amount as usize,
            seconds: amount,
            seed,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Subject {
    pub id: String,
    pub rows: Vec<(String, String, String)>,
}

pub fn subjects(count: usize, seed: u64) -> Result<Vec<Subject>, Error> {
    let supply = generate::cohort(count, seed)?;
    let mut held = Vec::new();
    for chunk in supply
        .lines()
        .collect::<Vec<&str>>()
        .chunks(generate::COHORT_SHARE)
    {
        let mut rows = Vec::new();
        for row in chunk {
            let body: Value =
                serde_json::from_str(row).map_err(|error| Error::InvalidJson(error.to_string()))?;
            let kind = body["resourceType"]
                .as_str()
                .ok_or_else(|| Error::InvalidEnvelope("a row names no type".to_owned()))?;
            let id = body["id"]
                .as_str()
                .ok_or_else(|| Error::InvalidEnvelope("a row carries no id".to_owned()))?;
            rows.push((kind.to_owned(), id.to_owned(), (*row).to_owned()));
        }
        let id = rows
            .first()
            .map(|(_, id, _)| id.clone())
            .ok_or_else(|| Error::InvalidEnvelope("a subject holds nothing".to_owned()))?;
        held.push(Subject { id, rows });
    }
    Ok(held)
}

#[derive(Default)]
struct Tally {
    write: Vec<u64>,
    read: Vec<u64>,
    search: Vec<u64>,
}

fn timed(
    taken: &mut Vec<u64>,
    refused: &AtomicU64,
    call: impl FnOnce() -> Result<u16, Error>,
) -> u16 {
    let started = Instant::now();
    let answered = call();
    taken.push(started.elapsed().as_micros() as u64);
    match answered {
        Ok(status) if (200..300).contains(&status) || status == HELD => status,
        Ok(status) => {
            refused.fetch_add(1, Ordering::Relaxed);
            status
        }
        Err(_) => {
            refused.fetch_add(1, Ordering::Relaxed);
            0
        }
    }
}

fn write_row(address: &str, kind: &str, id: &str, body: &str, held: bool) -> Result<u16, Error> {
    match held {
        true => http::send(address, "localhost", "PUT", &format!("/{kind}/{id}"), body)
            .map(|(status, _)| status),
        false => http::send(address, "localhost", "POST", &format!("/{kind}"), body)
            .map(|(status, _)| status),
    }
}

pub fn varied(body: &str, cycle: u64) -> String {
    let Ok(mut held) = serde_json::from_str::<Value>(body) else {
        return body.to_owned();
    };
    held["meta"]["tag"][0]["code"] = Value::String(format!("cycle-{cycle}"));
    held.to_string()
}

fn read_row(address: &str, kind: &str, id: &str) -> Result<u16, Error> {
    http::send(address, "localhost", "GET", &format!("/{kind}/{id}"), "").map(|(status, _)| status)
}

fn search_rows(address: &str, subject: &str) -> Result<u16, Error> {
    let path = format!("/Observation?patient=Patient/{subject}&_count=10");
    http::send(address, "localhost", "GET", &path, "").map(|(status, _)| status)
}

pub struct Outcome {
    pub record: Record,
    pub refused: u64,
    pub windows: Vec<Summary>,
}

impl Outcome {
    pub fn to_value(&self) -> Value {
        json!({
            "runs": self.record.runs.iter().map(Summary::to_value).collect::<Vec<Value>>(),
            "refused": self.refused,
            "windows": self.windows.iter().map(Summary::to_value).collect::<Vec<Value>>(),
            "drift": drift(&self.windows),
        })
    }

    pub fn to_json(&self) -> String {
        self.to_value().to_string()
    }
}

pub fn drift(windows: &[Summary]) -> Value {
    let mut held = Vec::new();
    let names: Vec<String> = {
        let mut names: Vec<String> = windows.iter().map(|run| run.operation.clone()).collect();
        names.dedup();
        names.sort_unstable();
        names.dedup();
        names
    };
    for name in names {
        let mut of_name: Vec<&Summary> = windows
            .iter()
            .filter(|run| run.operation == name)
            .collect::<Vec<&Summary>>();
        of_name.sort_by_key(|run| run.label.clone());
        let (Some(first), Some(last)) = (of_name.first(), of_name.last()) else {
            continue;
        };
        if first.p95 == 0 || of_name.len() < 2 {
            continue;
        }
        let ratio = last.p95 as f64 / first.p95 as f64;
        held.push(json!({
            "operation": name,
            "firstP95": first.p95,
            "lastP95": last.p95,
            "ratio": (ratio * 1_000.0).round() / 1_000.0,
            "verdict": if ratio > DRIFT { "drifting" } else { "steady" },
        }));
    }
    Value::Array(held)
}

fn gathered(label: &str, tallies: Vec<Tally>, wall: u64) -> Record {
    let mut write = Vec::new();
    let mut read = Vec::new();
    let mut search = Vec::new();
    for tally in tallies {
        write.extend(tally.write);
        read.extend(tally.read);
        search.extend(tally.search);
    }
    Record::new(vec![
        Run::over(label, "write", write, wall).summary(),
        Run::over(label, "read", read, wall).summary(),
        Run::over(label, "search", search, wall).summary(),
    ])
}

pub fn load(address: &str, plan: &Plan) -> Result<Outcome, Error> {
    let held = subjects(plan.subjects, plan.seed)?;
    let refused = Arc::new(AtomicU64::new(0));
    let mut runs = Vec::new();
    for (phase, operation) in ["write", "read", "search"].iter().enumerate() {
        let started = Instant::now();
        let mut workers = Vec::new();
        for ordinal in 0..plan.concurrency {
            let share: Vec<Subject> = held
                .iter()
                .skip(ordinal)
                .step_by(plan.concurrency)
                .cloned()
                .collect();
            let address = address.to_owned();
            let refused = Arc::clone(&refused);
            workers.push(std::thread::spawn(move || {
                let mut taken = Vec::new();
                for subject in &share {
                    match phase {
                        0 => {
                            for (kind, id, body) in &subject.rows {
                                timed(&mut taken, &refused, || {
                                    write_row(&address, kind, id, body, false)
                                });
                            }
                        }
                        1 => {
                            for (kind, id, _) in &subject.rows {
                                timed(&mut taken, &refused, || read_row(&address, kind, id));
                            }
                        }
                        _ => {
                            timed(&mut taken, &refused, || search_rows(&address, &subject.id));
                        }
                    }
                }
                taken
            }));
        }
        let mut samples = Vec::new();
        for worker in workers {
            samples.extend(
                worker
                    .join()
                    .map_err(|_| Error::Internal("a caller stopped early".to_owned()))?,
            );
        }
        let wall = started.elapsed().as_micros().max(1) as u64;
        runs.push(Run::over(&plan.label, *operation, samples, wall).summary());
    }
    Ok(Outcome {
        record: Record::new(runs),
        refused: refused.load(Ordering::Relaxed),
        windows: Vec::new(),
    })
}

pub fn soak(address: &str, plan: &Plan) -> Result<Outcome, Error> {
    let held = subjects(plan.subjects.max(plan.concurrency), plan.seed)?;
    let refused = Arc::new(AtomicU64::new(0));
    let deadline = Duration::from_secs(plan.seconds);
    let window = deadline / WINDOWS as u32;
    let started = Instant::now();
    let mut workers = Vec::new();
    for ordinal in 0..plan.concurrency {
        let share: Vec<Subject> = held
            .iter()
            .skip(ordinal)
            .step_by(plan.concurrency)
            .cloned()
            .collect();
        let address = address.to_owned();
        let refused = Arc::clone(&refused);
        workers.push(std::thread::spawn(move || {
            let mut per_window: Vec<Tally> = (0..WINDOWS).map(|_| Tally::default()).collect();
            let mut written: HashSet<String> = HashSet::new();
            let mut position = 0usize;
            while started.elapsed() < deadline && !share.is_empty() {
                let index = ((started.elapsed().as_micros() / window.as_micros().max(1)) as usize)
                    .min(WINDOWS - 1);
                let subject = &share[position % share.len()];
                position += 1;
                let tally = &mut per_window[index];
                let cycle = (position / share.len().max(1)) as u64;
                for (kind, id, body) in &subject.rows {
                    let held = written.contains(id);
                    let body = match held {
                        true => varied(body, cycle),
                        false => body.clone(),
                    };
                    timed(&mut tally.write, &refused, || {
                        write_row(&address, kind, id, &body, held)
                    });
                    written.insert(id.clone());
                }
                for (kind, id, _) in &subject.rows {
                    timed(&mut tally.read, &refused, || read_row(&address, kind, id));
                }
                timed(&mut tally.search, &refused, || {
                    search_rows(&address, &subject.id)
                });
            }
            per_window
        }));
    }
    let mut gathered_windows: Vec<Tally> = (0..WINDOWS).map(|_| Tally::default()).collect();
    for worker in workers {
        let held = worker
            .join()
            .map_err(|_| Error::Internal("a caller stopped without finishing".to_owned()))?;
        for (index, tally) in held.into_iter().enumerate() {
            gathered_windows[index].write.extend(tally.write);
            gathered_windows[index].read.extend(tally.read);
            gathered_windows[index].search.extend(tally.search);
        }
    }
    let wall = started.elapsed().as_micros() as u64;
    let each = (wall / WINDOWS as u64).max(1);
    let mut windows = Vec::new();
    let mut totals = Tally::default();
    for (index, tally) in gathered_windows.into_iter().enumerate() {
        let label = format!("w{index}");
        windows.push(Run::over(&label, "write", tally.write.clone(), each).summary());
        windows.push(Run::over(&label, "read", tally.read.clone(), each).summary());
        windows.push(Run::over(&label, "search", tally.search.clone(), each).summary());
        totals.write.extend(tally.write);
        totals.read.extend(tally.read);
        totals.search.extend(tally.search);
    }
    let record = gathered(&plan.label, vec![totals], wall.max(1));
    Ok(Outcome {
        record,
        refused: refused.load(Ordering::Relaxed),
        windows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cohort_reads_back_as_subjects_carrying_their_records() {
        let held = subjects(3, 4).expect("a cohort reads back");
        assert_eq!(held.len(), 3);
        assert_eq!(held[0].rows.len(), generate::COHORT_SHARE);
        assert!(held[0].rows.iter().any(|(kind, _, _)| kind == "Patient"));
        assert!(held[0]
            .rows
            .iter()
            .any(|(kind, _, _)| kind == "Observation"));
        assert_eq!(held[0].rows[0].1, held[0].id);
    }

    #[test]
    fn a_plan_without_work_is_refused() {
        assert!(Plan::parse("one", "0", "10", 1).is_err());
        assert!(Plan::parse("one", "2", "0", 1).is_err());
        assert!(Plan::parse("one", "two", "10", 1).is_err());
        assert!(Plan::parse("one", "2", "ten", 1).is_err());
        let plan = Plan::parse("one", "2", "10", 1).expect("a plan parses");
        assert_eq!(plan.concurrency, 2);
        assert_eq!(plan.subjects, 10);
        assert_eq!(plan.seconds, 10);
    }

    #[test]
    fn a_run_that_slows_as_it_goes_is_named_drifting() {
        let windows = vec![
            Run::over("w0", "read", vec![10; 4], 1_000).summary(),
            Run::over("w1", "read", vec![100; 4], 1_000).summary(),
        ];
        let judged = drift(&windows);
        assert_eq!(judged[0]["verdict"], "drifting");
        let steady = vec![
            Run::over("w0", "read", vec![10; 4], 1_000).summary(),
            Run::over("w1", "read", vec![11; 4], 1_000).summary(),
        ];
        assert_eq!(drift(&steady)[0]["verdict"], "steady");
        assert_eq!(drift(&[]).as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn a_marked_body_differs_from_the_one_before_it() {
        let held = subjects(1, 2).expect("a cohort reads back");
        let body = &held[0].rows[0].2;
        let one = varied(body, 1);
        assert_ne!(one, varied(body, 2));
        let read: Value = serde_json::from_str(&one).expect("a marked body is a resource");
        assert_eq!(read["meta"]["tag"][0]["code"], "cycle-1");
        assert_eq!(
            read["id"],
            serde_json::from_str::<Value>(body).unwrap()["id"]
        );
        assert_eq!(varied("nonsense", 1), "nonsense");
    }

    #[test]
    fn an_unreachable_instance_is_counted_rather_than_waited_on() {
        let plan = Plan {
            label: "closed".to_owned(),
            concurrency: 1,
            subjects: 1,
            seconds: 1,
            seed: 1,
        };
        let outcome = load("127.0.0.1:1", &plan).expect("a run reports what it could not do");
        assert!(outcome.refused > 0);
    }
}
