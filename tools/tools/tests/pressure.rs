mod support;

use fhir_tools::pressure::{load, soak, Plan};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

fn plan(label: &str, concurrency: usize, amount: u64) -> Plan {
    Plan {
        label: label.to_owned(),
        concurrency,
        subjects: amount as usize,
        seconds: amount,
        seed: 23,
    }
}

fn instance() -> Option<(Child, String)> {
    let binary = support::service_binary()?;
    let mut child = Command::new(binary)
        .env("FHIR_BACKEND", "memory")
        .env("FHIR_BIND", "127.0.0.1:0")
        .env("FHIR_VERSION", "R4")
        .env_remove("FHIR_DATABASE_URL")
        .env_remove("FHIR_METRICS_CREDENTIAL")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the service starts");
    let stdout = child.stdout.take().expect("the service speaks");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("the service announces an address");
    match line.trim().rsplit_once(' ') {
        Some((_, address)) if !address.is_empty() => Some((child, address.to_owned())),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the service announced no address, said {line:?}");
        }
    }
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn a_load_run_measures_every_phase_against_a_live_instance() {
    let Some((child, address)) = instance() else {
        return;
    };
    let outcome = load(&address, &plan("live", 2, 4));
    stop(child);
    let outcome = outcome.expect("a load run reports");
    assert_eq!(outcome.refused, 0, "the instance refused work");
    let named: Vec<&str> = outcome
        .record
        .runs
        .iter()
        .map(|run| run.operation.as_str())
        .collect();
    assert_eq!(named, vec!["write", "read", "search"]);
    assert_eq!(outcome.record.runs[0].count, 4 * 5);
    assert_eq!(outcome.record.runs[2].count, 4);
    assert!(outcome.record.runs[0].per_second > 0.0);
    assert!(outcome.record.runs[0].wall_micros > 0);
}

#[test]
fn a_soak_run_reports_a_window_at_a_time() {
    let Some((child, address)) = instance() else {
        return;
    };
    let outcome = soak(&address, &plan("soak", 2, 1));
    stop(child);
    let outcome = outcome.expect("a soak run reports");
    assert_eq!(outcome.refused, 0, "the instance refused work");
    assert_eq!(outcome.windows.len(), 15);
    assert!(outcome.record.runs[0].count > 0);
    let held = outcome.to_value();
    assert!(held["drift"].is_array());
    assert_eq!(held["refused"], 0);
}
