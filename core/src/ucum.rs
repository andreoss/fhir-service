










#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Mass,
    Length,
    Volume,
    Time,
}


struct Unit {
    code: &'static str,
    dimension: Dimension,
    base: f64,
}




const UNITS: &[Unit] = &[
    Unit {
        code: "g",
        dimension: Dimension::Mass,
        base: 1.0,
    },
    Unit {
        code: "kg",
        dimension: Dimension::Mass,
        base: 1_000.0,
    },
    Unit {
        code: "mg",
        dimension: Dimension::Mass,
        base: 0.001,
    },
    Unit {
        code: "ug",
        dimension: Dimension::Mass,
        base: 0.000_001,
    },
    Unit {
        code: "ng",
        dimension: Dimension::Mass,
        base: 0.000_000_001,
    },
    Unit {
        code: "[lb_av]",
        dimension: Dimension::Mass,
        base: 453.592_37,
    },
    Unit {
        code: "[oz_av]",
        dimension: Dimension::Mass,
        base: 28.349_523_125,
    },
    Unit {
        code: "m",
        dimension: Dimension::Length,
        base: 1.0,
    },
    Unit {
        code: "km",
        dimension: Dimension::Length,
        base: 1_000.0,
    },
    Unit {
        code: "cm",
        dimension: Dimension::Length,
        base: 0.01,
    },
    Unit {
        code: "mm",
        dimension: Dimension::Length,
        base: 0.001,
    },
    Unit {
        code: "[in_i]",
        dimension: Dimension::Length,
        base: 0.025_4,
    },
    Unit {
        code: "[ft_i]",
        dimension: Dimension::Length,
        base: 0.304_8,
    },
    Unit {
        code: "L",
        dimension: Dimension::Volume,
        base: 1.0,
    },
    Unit {
        code: "l",
        dimension: Dimension::Volume,
        base: 1.0,
    },
    Unit {
        code: "dL",
        dimension: Dimension::Volume,
        base: 0.1,
    },
    Unit {
        code: "mL",
        dimension: Dimension::Volume,
        base: 0.001,
    },
    Unit {
        code: "uL",
        dimension: Dimension::Volume,
        base: 0.000_001,
    },
    Unit {
        code: "s",
        dimension: Dimension::Time,
        base: 1.0,
    },
    Unit {
        code: "min",
        dimension: Dimension::Time,
        base: 60.0,
    },
    Unit {
        code: "h",
        dimension: Dimension::Time,
        base: 3_600.0,
    },
    Unit {
        code: "d",
        dimension: Dimension::Time,
        base: 86_400.0,
    },
    Unit {
        code: "wk",
        dimension: Dimension::Time,
        base: 604_800.0,
    },
    Unit {
        code: "a",
        dimension: Dimension::Time,
        base: 31_557_600.0,
    },
];




pub const UCUM: &str = "http://unitsofmeasure.org";

fn unit(code: &str) -> Option<&'static Unit> {
    UNITS.iter().find(|held| held.code == code)
}




pub fn canonical(
    value: f64,
    system: Option<&str>,
    code: Option<&str>,
) -> Option<(f64, &'static str)> {
    if system.is_some_and(|held| held != UCUM) {
        return None;
    }
    let held = unit(code?)?;
    let base = UNITS
        .iter()
        .find(|other| other.dimension == held.dimension && other.base == 1.0)?;
    Some((value * held.base, base.code))
}


pub fn comparable(one: &str, other: &str) -> bool {
    match (unit(one), unit(other)) {
        (Some(one), Some(other)) => one.dimension == other.dimension,
        _ => one == other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: f64, code: &str) -> Option<(f64, &'static str)> {
        canonical(value, Some(UCUM), Some(code))
    }

    #[test]
    fn a_kilogram_is_a_thousand_grams() {
        let (held, base) = at(2.0, "kg").unwrap();
        assert_eq!(base, "g");
        assert!((held - 2_000.0).abs() < 1e-9, "{held}");
    }

    #[test]
    fn a_pound_is_the_mass_it_is() {
        let (held, base) = at(1.0, "[lb_av]").unwrap();
        assert_eq!(base, "g");
        assert!((held - 453.592_37).abs() < 1e-6, "{held}");
    }

    #[test]
    fn every_dimension_has_a_base_of_its_own() {
        assert_eq!(at(1.0, "cm").unwrap().1, "m");
        assert_eq!(at(1.0, "mL").unwrap().1, "L");
        assert_eq!(at(1.0, "h").unwrap().1, "s");
    }

    #[test]
    fn a_unit_outside_the_table_is_left_alone() {
        assert!(at(1.0, "widgets").is_none());
        assert!(canonical(1.0, Some(UCUM), None).is_none());
    }

    #[test]
    fn a_code_from_another_system_is_not_a_ucum_code() {
        assert!(canonical(1.0, Some("urn:local"), Some("kg")).is_none());
        assert!(
            canonical(1.0, None, Some("kg")).is_some(),
            "a quantity naming no system is read as ucum, which is what the specification says"
        );
    }

    #[test]
    fn two_units_of_one_dimension_are_comparable_and_others_are_not() {
        assert!(comparable("kg", "g"));
        assert!(comparable("mL", "L"));
        assert!(!comparable("kg", "m"));
        assert!(comparable("widgets", "widgets"));
        assert!(!comparable("widgets", "kg"));
    }
}
