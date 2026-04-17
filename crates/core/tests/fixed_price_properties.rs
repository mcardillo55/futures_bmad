use futures_bmad_core::FixedPrice;
use proptest::prelude::*;

proptest! {
    #[test]
    fn saturating_add_never_panics(a: i64, b: i64) {
        let fa = FixedPrice::new(a);
        let fb = FixedPrice::new(b);
        let _ = fa.saturating_add(fb);
    }

    #[test]
    fn saturating_sub_never_panics(a: i64, b: i64) {
        let fa = FixedPrice::new(a);
        let fb = FixedPrice::new(b);
        let _ = fa.saturating_sub(fb);
    }

    #[test]
    fn saturating_mul_never_panics(a: i64, b: i64) {
        let fa = FixedPrice::new(a);
        let _ = fa.saturating_mul(b);
    }

    #[test]
    fn add_sub_roundtrip_no_overflow(
        a in -1_000_000_000i64..1_000_000_000i64,
        b in -1_000_000_000i64..1_000_000_000i64,
    ) {
        let fa = FixedPrice::new(a);
        let fb = FixedPrice::new(b);
        let result = fa.saturating_add(fb).saturating_sub(fb);
        prop_assert_eq!(result, fa);
    }

    #[test]
    fn from_f64_roundtrip_quarter_ticks(
        ticks in -10_000_000i64..10_000_000i64,
    ) {
        // Generate prices that are exact quarter-tick multiples
        let price = ticks as f64 * 0.25;
        let fp = FixedPrice::from_f64(price).unwrap();
        let back = fp.to_f64();
        prop_assert!((back - price).abs() < 1e-10, "round-trip failed: {price} -> {back}");
    }
}
