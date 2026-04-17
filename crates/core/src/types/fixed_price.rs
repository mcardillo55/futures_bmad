use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Error returned when a non-finite f64 (NaN, +Inf, -Inf) is passed to `from_f64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonFinitePrice;

impl fmt::Display for NonFinitePrice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "price must be a finite number (not NaN or Infinity)")
    }
}

impl std::error::Error for NonFinitePrice {}

/// Fixed-point price representation using quarter-ticks.
///
/// Stores prices as `price * 4` in integer form, avoiding all floating-point
/// arithmetic on the hot path. For example, 4482.25 is stored as 17929.
///
/// All arithmetic operations use saturating behavior — they will never panic
/// or silently wrap on overflow.
///
/// Serializes/deserializes as f64 for config file compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct FixedPrice(pub(crate) i64);

impl Serialize for FixedPrice {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.to_f64())
    }
}

impl<'de> Deserialize<'de> for FixedPrice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f64::deserialize(deserializer)?;
        FixedPrice::from_f64(value).map_err(serde::de::Error::custom)
    }
}

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
    pub fn from_f64(price: f64) -> Result<FixedPrice, NonFinitePrice> {
        if !price.is_finite() {
            return Err(NonFinitePrice);
        }
        // Banker's rounding: round half to even
        let scaled = price * 4.0;
        if !scaled.is_finite() {
            return Err(NonFinitePrice);
        }
        let rounded = bankers_round(scaled);
        Ok(FixedPrice(rounded))
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
        assert_eq!(FixedPrice::from_f64(4482.25).unwrap(), FixedPrice(17929));
        assert_eq!(FixedPrice::from_f64(0.0).unwrap(), FixedPrice(0));
        assert_eq!(FixedPrice::from_f64(-100.50).unwrap(), FixedPrice(-402));
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
        assert_eq!(FixedPrice::from_f64(0.125).unwrap(), FixedPrice(0)); // 0.125 * 4 = 0.5 -> 0
        // 1.5 rounds to 2 (even)
        assert_eq!(FixedPrice::from_f64(0.375).unwrap(), FixedPrice(2)); // 0.375 * 4 = 1.5 -> 2
        // 2.5 rounds to 2 (even)
        assert_eq!(FixedPrice::from_f64(0.625).unwrap(), FixedPrice(2)); // 0.625 * 4 = 2.5 -> 2
    }

    #[test]
    fn non_finite_rejected() {
        assert_eq!(FixedPrice::from_f64(f64::NAN), Err(NonFinitePrice));
        assert_eq!(FixedPrice::from_f64(f64::INFINITY), Err(NonFinitePrice));
        assert_eq!(FixedPrice::from_f64(f64::NEG_INFINITY), Err(NonFinitePrice));
    }
}
