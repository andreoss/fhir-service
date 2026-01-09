#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexFailure {
    pub resource: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexReport {
    pub url: String,
    pub backfilled: bool,
    pub indexed: usize,
    pub values: usize,
    pub overflow: usize,
    pub failures: Vec<IndexFailure>,
}

impl IndexReport {
    pub fn empty(url: &str) -> IndexReport {
        IndexReport {
            url: url.to_owned(),
            backfilled: false,
            indexed: 0,
            values: 0,
            overflow: 0,
            failures: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_index_reports_nothing_indexed() {
        let report = IndexReport::empty("urn:p:a");
        assert_eq!(report.url, "urn:p:a");
        assert!(!report.backfilled);
        assert_eq!(report.indexed, 0);
        assert_eq!(report.values, 0);
        assert_eq!(report.overflow, 0);
        assert!(report.failures.is_empty());
    }
}
