use async_trait::async_trait;
use fhir_core::Error;
use fhir_store::{BulkStore, JobId, Output};
use std::path::{Path, PathBuf};

fn failed(context: &str, error: std::io::Error) -> Error {
    Error::Internal(format!("{context}: {error}"))
}

fn rows(body: &[u8]) -> u64 {
    body.split(|byte| *byte == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .count() as u64
}

fn kind_of(name: &str) -> String {
    name.rsplit('/')
        .next()
        .unwrap_or(name)
        .split('.')
        .next()
        .unwrap_or(name)
        .to_owned()
}

fn under(root: &Path, name: &str) -> Result<PathBuf, Error> {
    let refused = || Error::InvalidParameter(format!("output name {name:?}"));
    if name.is_empty() || name.starts_with('/') {
        return Err(refused());
    }
    let mut path = root.to_path_buf();
    for part in name.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(refused());
        }
        path.push(part);
    }
    Ok(path)
}

fn walk(root: &Path, base: &Path, found: &mut Vec<Output>) -> Result<(), Error> {
    let listing = match std::fs::read_dir(base) {
        Ok(listing) => listing,
        Err(_) => return Ok(()),
    };
    for entry in listing {
        let entry = entry.map_err(|error| failed("listing the directory", error))?;
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, found)?;
            continue;
        }
        let name = path
            .strip_prefix(root)
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let body = std::fs::read(&path).map_err(|error| failed("reading an output", error))?;
        found.push(Output {
            name: name.clone(),
            kind: kind_of(&name),
            count: rows(&body),
            size: body.len(),
        });
    }
    Ok(())
}

pub struct DirectoryOutputs {
    root: PathBuf,
}

impl DirectoryOutputs {
    pub fn open(root: impl Into<PathBuf>) -> Result<DirectoryOutputs, Error> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|error| failed("preparing the output directory", error))?;
        Ok(DirectoryOutputs { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[async_trait]
impl BulkStore for DirectoryOutputs {
    async fn write(&self, _job: &JobId, output: &Output, body: &[u8]) -> Result<(), Error> {
        let path = under(&self.root, &output.name)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| failed("preparing the output directory", error))?;
        }
        std::fs::write(&path, body).map_err(|error| failed("writing an output", error))
    }

    async fn read(&self, _job: &JobId, name: &str) -> Result<Vec<u8>, Error> {
        let path = under(&self.root, name)?;
        std::fs::read(&path).map_err(|_| Error::NotFound)
    }

    async fn list(&self, _job: &JobId) -> Result<Vec<Output>, Error> {
        let mut found = Vec::new();
        walk(&self.root, &self.root, &mut found)?;
        found.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(found)
    }

    async fn purge(&self, job: &JobId) -> Result<usize, Error> {
        let held = self.list(job).await?;
        for output in &held {
            let path = under(&self.root, &output.name)?;
            let _ = std::fs::remove_file(path);
        }
        Ok(held.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default();
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("scratch")
            .join(format!("{name}-{stamp}"))
    }

    #[tokio::test]
    async fn a_written_file_reads_back_and_lists() {
        let root = root("outputs");
        let sink = DirectoryOutputs::open(&root).expect("the sink opens");
        let job = JobId::parse("one").expect("a valid id");
        let output = Output::new("box/Patient.ndjson", "Patient", 2);
        sink.write(&job, &output, b"{}\n{}\n")
            .await
            .expect("the write lands");

        assert_eq!(
            sink.read(&job, "box/Patient.ndjson").await.unwrap(),
            b"{}\n{}\n"
        );
        let listed = sink.list(&job).await.expect("the listing reads");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "box/Patient.ndjson");
        assert_eq!(listed[0].kind, "Patient");
        assert_eq!(listed[0].count, 2);

        assert_eq!(sink.purge(&job).await.expect("the purge runs"), 1);
        assert!(sink.read(&job, "box/Patient.ndjson").await.is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_name_that_leaves_the_directory_is_refused() {
        let root = root("escape");
        let sink = DirectoryOutputs::open(&root).expect("the sink opens");
        let job = JobId::parse("one").expect("a valid id");
        for name in ["../escaped", "/absolute", ""] {
            let output = Output::new(name, "Patient", 0);
            assert!(sink.write(&job, &output, b"{}").await.is_err(), "{name}");
        }
        assert!(sink.read(&job, "absent.ndjson").await.is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
