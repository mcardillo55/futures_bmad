use std::fmt;

/// Fixed-point price representation using quarter-ticks.
///
/// Stores prices as `price * 4` in integer form, avoiding all floating-point
/// arithmetic on the hot path. For example, 4482.25 is stored as 17929.
///
/// All arithmetic operations use saturating behavior — they will never panic
/// or silently wrap on overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct FixedPrice(pub(crate) i64);

impl FixedPrice {
    pub fn new(raw: i64) -> Self {
        Self(raw)
    }

    pub fn raw(&self) -> i64 {
        self.0
    }

    pub fn saturating_add(&self, other: FixedPrice) -> FixedPrice {
        FixedPrice(self.0.saturating_add(other.0))
    }

    pub fn saturating_sub(&self, other: FixedPrice) -> FixedPrice {
        FixedPrice(self.0.saturating_sub(other.0))
    }

    pub fn saturating_mul(&self, scalar: i64) -> FixedPrice {
        FixedPrice(self.0.saturating_mul(scalar))
    }

    /// Convert from f64 price to FixedPrice using banker's rounding (round half to even).
    ///
    /// This should only be used at config load time or for display input parsing.
    pub fn from_f64(price: f64) -> FixedPrice {
        // Banker's rounding: round half to even
        let scaled = price * 4.0;
        let rounded = bankers_round(scaled);
        FixedPrice(rounded)
    }

    /// Convert to f64 for display purposes only.
    ///
    /// **WARNING**: Do not use this value for arithmetic. Use FixedPrice methods instead.
    pub fn to_f64(&self) -> f64 {
        self.0 as f64 / 4.0
    }
}

/// Banker's rounding: round half to even.
fn bankers_round(x: f64) -> i64 {
    let floor = x.floor();
    let frac = x - floor;
    let floor_i = floor as i64;

    if (frac - 0.5).abs() < f64::EPSILON {
        // Exactly half: round to even
        if floor_i % 2 == 0 { floor_i } else { floor_i + 1 }
    } else {
        x.round() as i64
    }
}

impl fmt::Display for FixedPrice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.0 as f64 / 4.0;
        write!(f, "{value:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_price_conversions() {
        assert_eq!(FixedPrice::from_f64(4482.25), FixedPrice(17929));
        assert_eq!(FixedPrice::from_f64(0.0), FixedPrice(0));
        assert_eq!(FixedPrice::from_f64(-100.50), FixedPrice(-402));
    }

    #[test]
    fn display_format() {
        assert_eq!(format!("{}", FixedPrice(17929)), "4482.25");
        assert_eq!(format!("{}", FixedPrice(0)), "0.00");
    }

    #[test]
    fn saturating_arithmetic() {
        let max = FixedPrice(i64::MAX);
        let one = FixedPrice(1);
        assert_eq!(max.saturating_add(one), FixedPrice(i64::MAX));

        let min = FixedPrice(i64::MIN);
        assert_eq!(min.saturating_sub(one), FixedPrice(i64::MIN));

        assert_eq!(max.saturating_mul(2), FixedPrice(i64::MAX));
    }

    #[test]
    fn default_is_zero() {
        assert_eq!(FixedPrice::default(), FixedPrice(0));
    }

    #[test]
    fn bankers_rounding() {
        // 0.5 rounds to 0 (even)
        assert_eq!(FixedPrice::from_f64(0.125), FixedPrice(0)); // 0.125 * 4 = 0.5 -> 0
        // 1.5 rounds to 2 (even)
        assert_eq!(FixedPrice::from_f64(0.375), FixedPrice(2)); // 0.375 * 4 = 1.5 -> 2
        // 2.5 rounds to 2 (even)
        assert_eq!(FixedPrice::from_f64(0.625), FixedPrice(2)); // 0.625 * 4 = 2.5 -> 2
    }
}
