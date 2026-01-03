use crate::Error;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FhirInstant(String);

impl FhirInstant {
    pub fn parse(value: &str) -> Result<FhirInstant, Error> {
        validate_instant(value).map_err(|_| Error::InvalidInstant(value.to_owned()))?;
        Ok(FhirInstant(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for FhirInstant {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        FhirInstant::parse(value)
    }
}

impl TryFrom<&str> for FhirInstant {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Error> {
        FhirInstant::parse(value)
    }
}

impl fmt::Display for FhirInstant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn validate_instant(value: &str) -> Result<(), ()> {
    let (date, rest) = value.split_once('T').ok_or(())?;
    let index = rest.find(['Z', '+', '-']).ok_or(())?;
    let (time, zone) = rest.split_at(index);
    validate_date(date)?;
    validate_time(time)?;
    validate_zone(zone)
}

fn validate_date(date: &str) -> Result<(), ()> {
    let mut parts = date.split('-');
    let year = parts.next().ok_or(())?;
    let month = parts.next().ok_or(())?;
    let day = parts.next().ok_or(())?;
    if parts.next().is_some() {
        return Err(());
    }
    validate_digits(year, 4)?;
    let y = year.parse::<i64>().map_err(|_| ())?;
    if y == 0 {
        return Err(());
    }
    let m = parse_range(month, 1, 12)?;
    let d = parse_range(day, 1, 31)?;
    if m == 2 && d > 29 {
        return Err(());
    }
    if matches!(m, 4 | 6 | 9 | 11) && d > 30 {
        return Err(());
    }
    Ok(())
}

fn validate_time(time: &str) -> Result<(), ()> {
    let (base, fraction) = match time.split_once('.') {
        Some((base, frac)) => {
            if frac.is_empty() || !frac.chars().all(|c| c.is_ascii_digit()) {
                return Err(());
            }
            (base, Some(frac))
        }
        None => (time, None),
    };
    let mut parts = base.split(':');
    let hour = parts.next().ok_or(())?;
    let minute = parts.next().ok_or(())?;
    let second = parts.next().ok_or(())?;
    if parts.next().is_some() {
        return Err(());
    }
    parse_range(hour, 0, 23)?;
    parse_range(minute, 0, 59)?;
    let s = parse_range(second, 0, 60)?;
    if s == 60 && fraction.is_some() {
        return Err(());
    }
    Ok(())
}

fn validate_zone(zone: &str) -> Result<(), ()> {
    match zone {
        "Z" => Ok(()),
        _ => {
            let sign = zone.chars().next().ok_or(())?;
            if sign != '+' && sign != '-' {
                return Err(());
            }
            let offset = &zone[sign.len_utf8()..];
            let mut parts = offset.split(':');
            let hours = parse_range(parts.next().ok_or(())?, 0, 14)?;
            let mut minutes = 0;
            if let Some(minute_part) = parts.next() {
                minutes = parse_range(minute_part, 0, 59)?;
            }
            if parts.next().is_some() {
                return Err(());
            }
            if hours == 14 && minutes != 0 {
                return Err(());
            }
            Ok(())
        }
    }
}

fn validate_digits(value: &str, min_digits: usize) -> Result<(), ()> {
    if value.len() < min_digits || !value.chars().all(|c| c.is_ascii_digit()) {
        return Err(());
    }
    Ok(())
}

fn parse_range(value: &str, low: u32, high: u32) -> Result<u32, ()> {
    if value.len() != 2 {
        return Err(());
    }
    let number = value.parse::<u32>().map_err(|_| ())?;
    if number < low || number > high {
        return Err(());
    }
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_instants() {
        for value in [
            "2026-09-06T04:00:00.000Z",
            "2026-09-06T04:00:00Z",
            "1970-01-01T00:00:00-05:30",
            "2026-09-06T23:59:59+14:00",
            "2026-09-06T23:59:59+08:00",
            "2016-12-31T23:59:60Z",
            "2024-02-29T12:00:00.123456789Z",
        ] {
            let instant = FhirInstant::parse(value).expect("valid instant must parse");
            assert_eq!(instant.as_str(), value);
        }
    }

    #[test]
    fn rejects_malformed_instants() {
        for value in [
            "",
            "text",
            "2026-09-06",             
            "2026-09-06 04:00:00Z",   
            "2026-09-06T04:00:00",    
            "2026-13-01T04:00:00Z",   
            "2026-02-30T04:00:00Z",   
            "2026-09-06T25:00:00Z",   
            "2026-09-06T04:60:00Z",   
            "2026-09-06T04:00:61Z",   
            "2026-09-06T04:00:00+15:00",
            "2026-09-06T04:00:00+14:01",
            "2026-09-06T04:00:00.123+",
            "0000-01-01T00:00:00Z",
            "2026-9-06T04:00:00Z",
        ] {
            assert!(matches!(FhirInstant::parse(value), Err(Error::InvalidInstant(_))), "should reject {value:?}");
        }
    }

    #[test]
    fn display_round_trips_original_text() {
        let instant = FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap();
        assert_eq!(instant.to_string(), "2026-09-06T04:00:00.000Z");
    }
}