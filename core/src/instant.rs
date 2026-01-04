use crate::Error;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FhirInstant(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstantKey {
    seconds: i64,
    nanos: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstantPeriod {
    low: InstantKey,
    high: InstantKey,
}

impl FhirInstant {
    pub fn parse(value: &str) -> Result<FhirInstant, Error> {
        instant_key(value).map_err(|_| Error::InvalidInstant(value.to_owned()))?;
        Ok(FhirInstant(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn key(&self) -> InstantKey {
        instant_key(&self.0).unwrap_or(InstantKey { seconds: 0, nanos: 0 })
    }
}

impl InstantPeriod {
    pub fn parse(value: &str) -> Result<InstantPeriod, Error> {
        period_of(value).map_err(|_| Error::InvalidInstant(value.to_owned()))
    }

    pub fn low(&self) -> InstantKey {
        self.low
    }

    pub fn high(&self) -> InstantKey {
        self.high
    }

    pub fn contains(&self, instant: &FhirInstant) -> bool {
        let key = instant.key();
        self.low <= key && key <= self.high
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

fn instant_key(value: &str) -> Result<InstantKey, ()> {
    let (date, rest) = value.split_once('T').ok_or(())?;
    let index = rest.find(['Z', '+', '-']).ok_or(())?;
    let (time, zone) = rest.split_at(index);
    let (year, month, day) = date_parts(date)?;
    let (hour, minute, second, nanos) = time_parts(time)?;
    let offset = zone_offset(zone)?;
    let seconds = days_from_civil(year, month, day) * 86_400
        + i64::from(hour) * 3_600
        + i64::from(minute) * 60
        + i64::from(second)
        - offset;
    Ok(InstantKey { seconds, nanos })
}

fn period_of(value: &str) -> Result<InstantPeriod, ()> {
    if value.contains('T') {
        let key = instant_key(value)?;
        return Ok(InstantPeriod { low: key, high: key });
    }
    let parts: Vec<&str> = value.split('-').collect();
    let head = parts.first().copied().ok_or(())?;
    if head.len() != 4 {
        return Err(());
    }
    validate_digits(head, 4)?;
    let year = head.parse::<i64>().map_err(|_| ())?;
    if year == 0 {
        return Err(());
    }
    let (start, end) = match parts.len() {
        1 => ((year, 1, 1), (year + 1, 1, 1)),
        2 => {
            let month = parse_range(parts[1], 1, 12)?;
            ((year, month, 1), next_month(year, month))
        }
        3 => {
            let (year, month, day) = date_parts(value)?;
            ((year, month, day), next_day(year, month, day))
        }
        _ => return Err(()),
    };
    Ok(InstantPeriod {
        low: InstantKey {
            seconds: days_from_civil(start.0, start.1, start.2) * 86_400,
            nanos: 0,
        },
        high: InstantKey {
            seconds: days_from_civil(end.0, end.1, end.2) * 86_400 - 1,
            nanos: 999_999_999,
        },
    })
}

fn next_month(year: i64, month: u32) -> (i64, u32, u32) {
    if month == 12 {
        (year + 1, 1, 1)
    } else {
        (year, month + 1, 1)
    }
}

fn next_day(year: i64, month: u32, day: u32) -> (i64, u32, u32) {
    if day < days_in_month(year, month) {
        (year, month, day + 1)
    } else {
        next_month(year, month)
    }
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = if shifted >= 0 { shifted } else { shifted - 399 } / 400;
    let year_of_era = shifted - era * 400;
    let shifted_month = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn date_parts(date: &str) -> Result<(i64, u32, u32), ()> {
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
    Ok((y, m, d))
}

fn time_parts(time: &str) -> Result<(u32, u32, u32, u32), ()> {
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
    let h = parse_range(hour, 0, 23)?;
    let min = parse_range(minute, 0, 59)?;
    let s = parse_range(second, 0, 60)?;
    if s == 60 && fraction.is_some() {
        return Err(());
    }
    Ok((h, min, s, nanos_of(fraction)))
}

fn nanos_of(fraction: Option<&str>) -> u32 {
    let Some(fraction) = fraction else { return 0 };
    let mut digits: String = fraction.chars().take(9).collect();
    while digits.len() < 9 {
        digits.push('0');
    }
    digits.parse::<u32>().unwrap_or_default()
}

fn zone_offset(zone: &str) -> Result<i64, ()> {
    match zone {
        "Z" => Ok(0),
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
            let magnitude = i64::from(hours) * 3_600 + i64::from(minutes) * 60;
            Ok(if sign == '-' { -magnitude } else { magnitude })
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
#[cfg(test)]
mod period_tests {
    use super::*;

    fn key(value: &str) -> InstantKey {
        FhirInstant::parse(value).unwrap().key()
    }

    #[test]
    fn keys_order_instants_chronologically() {
        assert!(key("2026-09-06T04:00:00Z") < key("2026-09-06T04:00:01Z"));
        assert!(key("2026-09-06T04:00:00.500Z") > key("2026-09-06T04:00:00.100Z"));
        assert!(key("2025-12-31T23:59:59Z") < key("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn keys_normalise_the_zone_offset() {
        assert_eq!(key("2026-09-06T04:00:00Z"), key("2026-09-06T06:00:00+02:00"));
        assert_eq!(key("2026-09-06T04:00:00Z"), key("2026-09-05T22:30:00-05:30"));
    }

    #[test]
    fn a_full_instant_is_a_point_period() {
        let period = InstantPeriod::parse("2026-09-06T04:00:00Z").unwrap();
        assert!(period.contains(&FhirInstant::parse("2026-09-06T04:00:00Z").unwrap()));
        assert!(!period.contains(&FhirInstant::parse("2026-09-06T04:00:01Z").unwrap()));
        assert_eq!(period.low(), key("2026-09-06T04:00:00Z"));
    }

    #[test]
    fn a_partial_date_covers_its_whole_span() {
        let day = InstantPeriod::parse("2026-09-06").unwrap();
        assert!(day.contains(&FhirInstant::parse("2026-09-06T00:00:00Z").unwrap()));
        assert!(day.contains(&FhirInstant::parse("2026-09-06T23:59:59.999Z").unwrap()));
        assert!(!day.contains(&FhirInstant::parse("2026-09-07T00:00:00Z").unwrap()));

        let month = InstantPeriod::parse("2026-02").unwrap();
        assert!(month.contains(&FhirInstant::parse("2026-02-28T23:00:00Z").unwrap()));
        assert!(!month.contains(&FhirInstant::parse("2026-03-01T00:00:00Z").unwrap()));

        let year = InstantPeriod::parse("2026").unwrap();
        assert!(year.contains(&FhirInstant::parse("2026-12-31T23:59:59Z").unwrap()));
        assert!(!year.contains(&FhirInstant::parse("2027-01-01T00:00:00Z").unwrap()));
    }

    #[test]
    fn a_leap_day_is_inside_february() {
        let month = InstantPeriod::parse("2024-02").unwrap();
        assert!(month.contains(&FhirInstant::parse("2024-02-29T12:00:00Z").unwrap()));
    }

    #[test]
    fn rejects_malformed_periods() {
        for value in ["", "text", "2026-13", "2026-02-30", "2026-09-06T04:00:00", "20260906"] {
            assert!(matches!(InstantPeriod::parse(value), Err(Error::InvalidInstant(_))), "should reject {value:?}");
        }
    }
}
