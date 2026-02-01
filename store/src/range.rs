use fhir_core::Error;

pub const PARTITIONS: i32 = 256;

const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

pub fn partition(id: &str) -> i32 {
    let mut hash = OFFSET;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    (hash % PARTITIONS as u64) as i32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedRange {
    low: i32,
    high: i32,
}

impl Default for FeedRange {
    fn default() -> FeedRange {
        FeedRange::whole()
    }
}

impl FeedRange {
    pub fn whole() -> FeedRange {
        FeedRange {
            low: 0,
            high: PARTITIONS,
        }
    }

    pub fn new(low: i32, high: i32) -> Result<FeedRange, Error> {
        match low >= 0 && high > low && high <= PARTITIONS {
            true => Ok(FeedRange { low, high }),
            false => Err(Error::InvalidParameter(format!(
                "feed range [{low}, {high}) is outside [0, {PARTITIONS})"
            ))),
        }
    }

    pub fn low(&self) -> i32 {
        self.low
    }

    pub fn high(&self) -> i32 {
        self.high
    }

    pub fn holds(&self, id: &str) -> bool {
        let found = partition(id);
        found >= self.low && found < self.high
    }

    pub fn split(&self) -> Option<(FeedRange, FeedRange)> {
        let middle = self.low + (self.high - self.low) / 2;
        match middle > self.low {
            true => Some((
                FeedRange {
                    low: self.low,
                    high: middle,
                },
                FeedRange {
                    low: middle,
                    high: self.high,
                },
            )),
            false => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_id_always_lands_in_one_partition() {
        assert_eq!(partition("p1"), partition("p1"));
        assert!((0..PARTITIONS).contains(&partition("p1")));
        assert!((0..PARTITIONS).contains(&partition("")));
    }

    #[test]
    fn the_whole_range_holds_every_id() {
        let whole = FeedRange::whole();
        for id in ["p1", "p2", "observation-9", ""] {
            assert!(whole.holds(id), "{id}");
        }
    }

    #[test]
    fn the_halves_of_a_range_cover_it_exactly_once() {
        let (left, right) = FeedRange::whole().split().expect("a wide range splits");
        assert_eq!(left.low(), 0);
        assert_eq!(left.high(), right.low());
        assert_eq!(right.high(), PARTITIONS);
        for id in ["p1", "p2", "p3", "v1", "v2"] {
            assert_ne!(left.holds(id), right.holds(id), "{id}");
        }
    }

    #[test]
    fn one_partition_wide_is_as_narrow_as_a_range_goes() {
        let narrow = FeedRange::new(3, 4).expect("a single partition is a range");
        assert!(narrow.split().is_none());
        assert!(FeedRange::new(-1, 4).is_err());
        assert!(FeedRange::new(4, 4).is_err());
        assert!(FeedRange::new(0, PARTITIONS + 1).is_err());
    }
}
