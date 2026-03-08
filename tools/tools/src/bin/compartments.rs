use fhir_core::Error;
use fhir_tools::compartments;
use std::path::Path;

const USAGE: &str = "usage: compartments <artifacts> <into>";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    match run(&args) {
        Ok(report) => println!("{report}"),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

fn run(args: &[String]) -> Result<String, Error> {
    let held = compartments::read(Path::new(&args[0]))?;
    let source = compartments::source(&held);
    std::fs::write(&args[1], &source)
        .map_err(|error| Error::Internal(format!("the source cannot be written: {error}")))?;
    let counted: usize = held.values().map(BTreeMapLen::len_of).sum();
    Ok(format!(
        "generated {} compartments, {counted} memberships, into {}",
        held.len(),
        args[1]
    ))
}

trait BTreeMapLen {
    fn len_of(&self) -> usize;
}

impl<K, V> BTreeMapLen for std::collections::BTreeMap<K, V> {
    fn len_of(&self) -> usize {
        self.len()
    }
}
