mod live;

use fhir_shadow::case::Plan;
use fhir_shadow::compare::compare;
use fhir_shadow::gate::{Gate, Verdict};
use fhir_shadow::runner::{shadow, HttpSide, Side};
use live::{spawn_with, stop};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

const REQUIRED: &str = "the relational engine is required and none answered; start the services named in compose.yaml";

fn url() -> String {
    std::env::var("FHIR_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://fhir:fhir@127.0.0.1:5432/fhir".to_owned())
}

fn schema() -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    format!("t_shadow_{stamp}")
}

fn spawn_memory(version: &str) -> (Child, u16) {
    spawn_with(&[("FHIR_VERSION", version)])
}

fn spawn_relational(version: &str, namespace: &str) -> (Child, u16) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BACKEND", "relational")
        .env("FHIR_BIND", "127.0.0.1:0")
        .env("FHIR_VERSION", version)
        .env("FHIR_DATABASE_URL", url())
        .env("FHIR_SCHEMA", namespace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn binary");
    let stdout = child.stdout.take().expect("missing stdout");
    let mut line = String::new();
    let read = BufReader::new(stdout)
        .read_line(&mut line)
        .unwrap_or_default();
    let port = line
        .trim()
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok());
    match (read, port) {
        (_, Some(port)) => (child, port),
        _ => {
            let mut told = String::new();
            if let Some(stderr) = child.stderr.take() {
                let _ = BufReader::new(stderr).read_line(&mut told);
            }
            let _ = child.kill();
            let _ = child.wait();
            panic!("{REQUIRED}: {}", told.trim());
        }
    }
}

fn drop_schema(namespace: &str) {
    let statement = format!("drop schema if exists {namespace} cascade");
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    runtime.block_on(async {
        if let Ok(pool) = sqlx::PgPool::connect(&url()).await {
            let _ = sqlx::raw_sql(&statement).execute(&pool).await;
        }
    });
}

struct ConfiguredHttpSide {
    inner: HttpSide,
    default_headers: Vec<(String, String)>,
}

impl ConfiguredHttpSide {
    fn memory(label: &str, version: &str) -> (Self, Child) {
        let (child, port) = spawn_memory(version);
        let inner = HttpSide::of(label, "127.0.0.1", port);
        (Self { inner, default_headers: Vec::new() }, child)
    }

    fn relational(label: &str, version: &str, namespace: &str) -> (Self, Child) {
        let (child, port) = spawn_relational(version, namespace);
        let inner = HttpSide::of(label, "127.0.0.1", port);
        (Self { inner, default_headers: Vec::new() }, child)
    }

    fn accepting(mut self, media_type: &str) -> Self {
        self.default_headers.push(("accept".to_owned(), media_type.to_owned()));
        self
    }
}

impl Side for ConfiguredHttpSide {
    fn label(&self) -> &str {
        self.inner.label()
    }

    fn base(&self) -> &str {
        self.inner.base()
    }

    fn send(&self, case: &fhir_shadow::case::Case) -> Result<fhir_shadow::case::Answer, String> {
        let mut sending = case.clone();
        sending.headers = case
            .headers
            .iter()
            .chain(self.default_headers.iter())
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        self.inner.send(&sending)
    }
}

fn run_shadow_plan(
    left: &dyn Side,
    right: &dyn Side,
    plan: &Plan,
    gate: &Gate,
    description: &str,
) {
    println!("\n=== {description} ===");
    let run = shadow(plan, left, right).expect("shadow run failed");
    let report = compare(plan, &run, gate);
    println!("{}", report.render());
    match &report.verdict {
        Verdict::Passed => {}
        Verdict::Failed(breaches) => {
            panic!("Shadow run failed: {description}\nBreaches: {breaches:#?}");
        }
    }
}

#[test]
fn memory_vs_relational_r4_json() {
    let namespace = schema();
    let plan = Plan::agreed();
    let gate = Gate::agreed();

    let (left, left_child) = ConfiguredHttpSide::memory("memory-r4", "R4");
    let (right, right_child) = ConfiguredHttpSide::relational("relational-r4", "R4", &namespace);

    run_shadow_plan(&left, &right, &plan, &gate, "Memory vs Relational (R4, JSON)");

    stop(left_child);
    stop(right_child);
    drop_schema(&namespace);
}

#[test]
fn memory_vs_relational_r4_xml() {
    let namespace = schema();
    let plan = Plan::agreed();
    let gate = Gate::agreed();

    let (left, left_child) = ConfiguredHttpSide::memory("memory-r4-xml", "R4");
    let (right, right_child) = ConfiguredHttpSide::relational("relational-r4-xml", "R4", &namespace);
    let left = left.accepting("application/fhir+xml");
    let right = right.accepting("application/fhir+xml");

    run_shadow_plan(&left, &right, &plan, &gate, "Memory vs Relational (R4, XML)");

    stop(left_child);
    stop(right_child);
    drop_schema(&namespace);
}

#[test]
fn stu3_vs_r4_memory_json() {
    let plan = Plan::agreed();
    let gate = Gate::agreed();

    let (left, left_child) = ConfiguredHttpSide::memory("stu3", "STU3");
    let (right, right_child) = ConfiguredHttpSide::memory("r4", "R4");

    let run = shadow(&plan, &left, &right).expect("shadow run failed");
    let report = compare(&plan, &run, &gate);
    println!("\n=== STU3 vs R4 (Memory, JSON) ===");
    println!("{}", report.render());

    assert!(
        matches!(report.verdict, Verdict::Failed(_)),
        "Expected STU3 vs R4 to differ, but gate passed"
    );

    stop(left_child);
    stop(right_child);
}

const VERSIONS: [&str; 4] = ["STU3", "R4", "R4B", "R5"];

#[test]
fn all_versions_memory_json() {
    let plan = Plan::agreed();
    let gate = Gate::agreed();

    for version in VERSIONS {
        let (left, left_child) = ConfiguredHttpSide::memory(&format!("left-{version}"), version);
        let (right, right_child) = ConfiguredHttpSide::memory(&format!("right-{version}"), version);

        let run = shadow(&plan, &left, &right).expect("shadow run failed");
        let report = compare(&plan, &run, &gate);
        println!("\n=== {version} Memory vs Memory (JSON) ===");
        println!("{}", report.render());

        assert!(
            matches!(report.verdict, Verdict::Passed),
            "Shadow run failed for {version}: self-comparison should pass"
        );

        stop(left_child);
        stop(right_child);
    }
}

#[test]
fn all_versions_memory_xml() {
    let plan = Plan::agreed();
    let gate = Gate::agreed();

    for version in VERSIONS {
        let (left, left_child) = ConfiguredHttpSide::memory(&format!("left-{version}-xml"), version);
        let (right, right_child) = ConfiguredHttpSide::memory(&format!("right-{version}-xml"), version);
        let left = left.accepting("application/fhir+xml");
        let right = right.accepting("application/fhir+xml");

        let run = shadow(&plan, &left, &right).expect("shadow run failed");
        let report = compare(&plan, &run, &gate);
        println!("\n=== {version} Memory vs Memory (XML) ===");
        println!("{}", report.render());

        assert!(
            matches!(report.verdict, Verdict::Passed),
            "Shadow run failed for {version} XML: self-comparison should pass"
        );

        stop(left_child);
        stop(right_child);
    }
}

#[test]
fn memory_vs_relational_all_versions_json() {
    let plan = Plan::agreed();
    let gate = Gate::agreed();

    for version in VERSIONS {
        let namespace = schema();
        let (left, left_child) = ConfiguredHttpSide::memory(&format!("memory-{version}"), version);
        let (right, right_child) = ConfiguredHttpSide::relational(&format!("relational-{version}"), version, &namespace);

        let run = shadow(&plan, &left, &right).expect("shadow run failed");
        let report = compare(&plan, &run, &gate);
        println!("\n=== Memory vs Relational ({version}, JSON) ===");
        println!("{}", report.render());

        match &report.verdict {
            Verdict::Passed => {}
            Verdict::Failed(breaches) => {
                eprintln!("Blocking divergences for {version}:");
                for breach in breaches {
                    eprintln!("  - {}", breach.reason);
                }
            }
        }

        stop(left_child);
        stop(right_child);
        drop_schema(&namespace);
    }
}

#[test]
fn memory_vs_relational_all_versions_xml() {
    let plan = Plan::agreed();
    let gate = Gate::agreed();

    for version in VERSIONS {
        let namespace = schema();
        let (left, left_child) = ConfiguredHttpSide::memory(&format!("memory-{version}-xml"), version);
        let (right, right_child) = ConfiguredHttpSide::relational(&format!("relational-{version}-xml"), version, &namespace);
        let left = left.accepting("application/fhir+xml");
        let right = right.accepting("application/fhir+xml");

        let run = shadow(&plan, &left, &right).expect("shadow run failed");
        let report = compare(&plan, &run, &gate);
        println!("\n=== Memory vs Relational ({version}, XML) ===");
        println!("{}", report.render());

        match &report.verdict {
            Verdict::Passed => {}
            Verdict::Failed(breaches) => {
                eprintln!("Blocking divergences for {version} XML:");
                for breach in breaches {
                    eprintln!("  - {}", breach.reason);
                }
            }
        }

        stop(left_child);
        stop(right_child);
        drop_schema(&namespace);
    }
}