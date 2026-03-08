use fhir_core::Error;
use fhir_tools::pipeline::Pipeline;
use std::process::Command;

const USAGE: &str = "usage: pipeline run [definition] | pipeline list [definition]";
const DEFINITION: &str = "ci/pipeline.yaml";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.get(1).map(String::as_str).unwrap_or(DEFINITION);
    let outcome = match args.first().map(String::as_str) {
        Some("run") => run(path),
        Some("list") => list(path),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(reason) = outcome {
        eprintln!("{reason}");
        std::process::exit(1);
    }
}

fn held(path: &str) -> Result<Pipeline, Error> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| Error::Config(format!("the definition cannot be read: {error}")))?;
    Pipeline::parse(&text)
}

fn list(path: &str) -> Result<(), Error> {
    for stage in held(path)?.stages {
        match stage.gate {
            Some(gate) => println!("{} {} gate {gate}", stage.name, stage.run),
            None => println!("{} {}", stage.name, stage.run),
        }
    }
    Ok(())
}

fn run(path: &str) -> Result<(), Error> {
    for stage in held(path)?.stages {
        println!("stage {}", stage.name);
        let status = Command::new("sh")
            .arg("-c")
            .arg(&stage.run)
            .status()
            .map_err(|error| Error::Internal(format!("{} cannot start: {error}", stage.name)))?;
        if !status.success() {
            return Err(Error::Internal(format!("stage {} failed", stage.name)));
        }
    }
    Ok(())
}
