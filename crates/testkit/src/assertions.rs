use futures_bmad_core::FixedPrice;

/// Check if two prices are within epsilon quarter-ticks of each other.
pub fn price_eq_epsilon(actual: FixedPrice, expected: FixedPrice, epsilon_ticks: i64) -> bool {
    (actual.raw() - expected.raw()).abs() <= epsilon_ticks
}

/// Assert two prices are within epsilon quarter-ticks. Panics with descriptive message.
pub fn assert_price_eq_epsilon(actual: FixedPrice, expected: FixedPrice, epsilon_ticks: i64) {
    assert!(
        price_eq_epsilon(actual, expected, epsilon_ticks),
        "price mismatch: actual={actual} (raw={}), expected={expected} (raw={}), epsilon={epsilon_ticks} ticks",
        actual.raw(),
        expected.raw()
    );
}

/// Check if a signal value is within [min, max].
pub fn signal_in_range(value: f64, min: f64, max: f64) -> bool {
    value >= min && value <= max
}

/// Assert a signal value is within [min, max]. Panics with descriptive message.
pub fn assert_signal_in_range(value: f64, min: f64, max: f64) {
    assert!(
        signal_in_range(value, min, max),
        "signal out of range: {value} not in [{min}, {max}]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_within_epsilon() {
        let a = FixedPrice::new(100);
        let b = FixedPrice::new(102);
        assert!(price_eq_epsilon(a, b, 2));
        assert!(!price_eq_epsilon(a, b, 1));
    }

    #[test]
    fn signal_range_check() {
        assert!(signal_in_range(0.5, 0.0, 1.0));
        assert!(!signal_in_range(1.5, 0.0, 1.0));
        assert!(signal_in_range(0.0, 0.0, 1.0)); // inclusive
        assert!(signal_in_range(1.0, 0.0, 1.0)); // inclusive
    }

    #[test]
    #[should_panic(expected = "price mismatch")]
    fn assert_price_panics_on_mismatch() {
        assert_price_eq_epsilon(FixedPrice::new(100), FixedPrice::new(200), 5);
    }

    #[test]
    #[should_panic(expected = "signal out of range")]
    fn assert_signal_panics_on_out_of_range() {
        assert_signal_in_range(2.0, 0.0, 1.0);
    }
}
