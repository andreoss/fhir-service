use fhir_shadow::case::Plan;
use fhir_shadow::compare::compare;
use fhir_shadow::gate::{Gate, Verdict};
use fhir_shadow::runner::{address, shadow, HttpSide, Targets};

const USAGE: &str =
    "usage: shadow <candidate host:port[/path]> <incumbent host:port[/path]> [report path]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(left), Some(right)) = (args.first(), args.get(1)) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let (Some(left), Some(right)) = (address(left), address(right)) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let targets = match Targets::of((&left.0, left.1), (&right.0, right.1)) {
        Ok(targets) => targets,
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(2);
        }
    };
    let candidate = HttpSide::of("candidate", targets.left().0, targets.left().1).under(&left.2);
    let incumbent = HttpSide::of("incumbent", targets.right().0, targets.right().1).under(&right.2);
    let plan = Plan::agreed();
    let run = match shadow(&plan, &candidate, &incumbent) {
        Ok(run) => run,
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(2);
        }
    };
    let report = compare(&plan, &run, &Gate::agreed());
    let text = report.render();
    match args.get(2) {
        Some(path) => {
            if let Err(error) = std::fs::write(path, &text) {
                eprintln!("the report was not written: {error}");
                std::process::exit(2);
            }
        }
        None => println!("{text}"),
    }
    if matches!(report.verdict, Verdict::Failed(_)) {
        std::process::exit(1);
    }
}
