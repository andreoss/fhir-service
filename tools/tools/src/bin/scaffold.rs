use fhir_core::Error;
use fhir_tools::scaffold::Scaffold;
use std::path::Path;

const USAGE: &str = "usage: scaffold <Name> <job kind> <directory>";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    match run(&args[0], &args[1], &args[2]) {
        Ok(report) => println!("{report}"),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

fn run(name: &str, kind: &str, directory: &str) -> Result<String, Error> {
    let plan = Scaffold::plan(name, kind)?;
    let written = plan.write(Path::new(directory))?;
    Ok(format!("{}\n{}", written.join("\n"), plan.registration))
}
