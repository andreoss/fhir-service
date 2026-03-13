use fhir_core::Error;

pub fn size(raw: &str) -> Result<usize, Error> {
    let held = raw.trim();
    let (number, unit) = held
        .find(|held: char| !held.is_ascii_digit())
        .map(|at| held.split_at(at))
        .unwrap_or((held, ""));
    let number = number
        .parse::<usize>()
        .map_err(|_| Error::Config(format!("{raw:?} does not begin with a number of bytes")))?;
    let scale = match unit.trim() {
        "" | "b" | "B" => 1,
        "kB" => 1_000,
        "KiB" => 1_024,
        "MB" => 1_000_000,
        "MiB" => 1_024 * 1_024,
        other => {
            return Err(Error::Config(format!(
                "{other:?} is no unit; write b, kB, KiB, MB or MiB"
            )))
        }
    };
    number
        .checked_mul(scale)
        .ok_or_else(|| Error::Config(format!("{raw:?} is larger than this machine can address")))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    body: Option<usize>,
    entries: Option<usize>,
}

impl Limits {
    pub fn unbounded() -> Limits {
        Limits::default()
    }

    pub fn new(body: Option<usize>, entries: Option<usize>) -> Result<Limits, Error> {
        if body == Some(0) || entries == Some(0) {
            return Err(Error::Config(
                "a bound of zero would refuse every write".to_owned(),
            ));
        }
        Ok(Limits { body, entries })
    }

    pub fn body(&self) -> Option<usize> {
        self.body
    }

    pub fn entries(&self) -> Option<usize> {
        self.entries
    }

    pub fn admits_body(&self, held: usize) -> Result<(), Error> {
        match self.body {
            Some(bound) if held > bound => Err(Error::TooLarge(format!(
                "this instance takes a body of {bound} bytes and this one is {held}"
            ))),
            _ => Ok(()),
        }
    }

    pub fn admits_entries(&self, held: usize) -> Result<(), Error> {
        match self.entries {
            Some(bound) if held > bound => Err(Error::Unprocessable(format!(
                "this instance processes a bundle of {bound} entries and this one carries {held}"
            ))),
            _ => Ok(()),
        }
    }
}

pub async fn bounded(
    axum::extract::State(state): axum::extract::State<crate::app::AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let Some(bound) = state.limits.body() else {
        return next.run(request).await;
    };

    let declared = request
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if let Some(held) = declared {
        if let Err(error) = state.limits.admits_body(held) {
            return crate::handlers::AppError::from(error).into_response_now();
        }
    }

    let (parts, body) = request.into_parts();
    let held = match axum::body::to_bytes(body, bound.saturating_add(1)).await {
        Err(_) => return refused(bound, None),
        Ok(held) if held.len() > bound => return refused(bound, Some(held.len())),
        Ok(held) => held,
    };
    next.run(axum::extract::Request::from_parts(
        parts,
        axum::body::Body::from(held),
    ))
    .await
}

fn refused(bound: usize, held: Option<usize>) -> axum::response::Response {
    let said = match held {
        Some(held) => format!("this instance takes a body of {bound} bytes and this one is {held}"),
        None => format!("this instance takes a body of {bound} bytes and this one is larger"),
    };
    crate::handlers::AppError::from(Error::TooLarge(said)).into_response_now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_read_with_or_without_a_unit() {
        assert_eq!(size("900").unwrap(), 900);
        assert_eq!(size("1kB").unwrap(), 1_000);
        assert_eq!(size("1KiB").unwrap(), 1_024);
        assert_eq!(size("1MiB").unwrap(), 1_048_576);
        assert_eq!(size(" 2 MB ").unwrap(), 2_000_000);
    }

    #[test]
    fn what_is_no_size_is_refused() {
        assert!(size("big").is_err());
        assert!(size("10GB").is_err());
    }

    #[test]
    fn an_unbounded_instance_admits_everything() {
        let held = Limits::unbounded();
        assert!(held.admits_body(usize::MAX).is_ok());
        assert!(held.admits_entries(100_000).is_ok());
    }

    #[test]
    fn a_bound_admits_what_is_within_it() {
        let held = Limits::new(Some(100), Some(2)).expect("a bound");
        assert!(held.admits_body(100).is_ok());
        assert!(held.admits_body(101).is_err());
        assert!(held.admits_entries(2).is_ok());
        assert!(held.admits_entries(3).is_err());
    }

    #[test]
    fn a_bound_of_zero_is_refused() {
        assert!(Limits::new(Some(0), None).is_err());
        assert!(Limits::new(None, Some(0)).is_err());
    }
}
