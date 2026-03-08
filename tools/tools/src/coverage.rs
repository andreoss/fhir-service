use fhir_core::Error;
use serde_json::Value;

pub const REQUIRED: f64 = 85.0;

pub fn lines_percent(text: &str) -> Result<f64, Error> {
    let refused = || Error::Config("the coverage report carries no line total".to_owned());
    let value: Value =
        serde_json::from_str(text).map_err(|error| Error::InvalidJson(error.to_string()))?;
    value
        .pointer("/data/0/totals/lines/percent")
        .and_then(Value::as_f64)
        .ok_or_else(refused)
}

pub fn clears(percent: f64, required: f64) -> bool {
    percent + f64::EPSILON >= required
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_without_totals_is_refused() {
        assert!(lines_percent("{\"data\":[{}]}").is_err());
        assert!(lines_percent("[]").is_err());
    }

    #[test]
    fn the_boundary_clears_and_anything_under_it_does_not() {
        assert!(clears(85.0, REQUIRED));
        assert!(clears(93.15, REQUIRED));
        assert!(!clears(84.999, REQUIRED));
    }
}
