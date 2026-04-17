use futures_bmad_core::{FixedPrice, OrderBook, Signal};
use futures_bmad_engine::signals::ObiSignal;
use futures_bmad_testkit::{OrderBookBuilder, SimClock, assert_signal_in_range};

const EPSILON: f64 = 1e-10;
const MAX_SPREAD: i64 = 100; // generous spread threshold for tests

fn sim_clock() -> SimClock {
    SimClock::new(1_000_000_000)
}

/// Build a tradeable book (>= 3 levels each side) with specified best-level sizes.
/// Additional levels get size=10 as filler for is_tradeable() requirement.
fn tradeable_book(bid_sizes: &[u32], ask_sizes: &[u32]) -> OrderBook {
    let mut builder = OrderBookBuilder::new();
    // Bids descending from 100.00
    for (i, &size) in bid_sizes.iter().enumerate() {
        builder = builder.bid(100.0 - (i as f64) * 0.25, size);
    }
    // Asks ascending from 100.25
    for (i, &size) in ask_sizes.iter().enumerate() {
        builder = builder.ask(100.25 + (i as f64) * 0.25, size);
    }
    builder.build()
}

#[test]
fn balanced_book_produces_zero_obi() {
    let clock = sim_clock();
    let mut obi = ObiSignal::new(1, FixedPrice::new(MAX_SPREAD));
    let book = tradeable_book(&[10, 10, 10], &[10, 10, 10]);

    let result = obi.update(&book, None, &clock);
    assert!(result.is_some());
    let val = result.unwrap();
    assert!((val - 0.0).abs() < EPSILON, "expected 0.0, got {val}");
}

#[test]
fn heavy_bid_pressure_positive_obi() {
    let clock = sim_clock();
    let mut obi = ObiSignal::new(1, FixedPrice::new(MAX_SPREAD));
    // Bids much larger than asks
    let book = tradeable_book(&[100, 100, 100], &[1, 1, 1]);

    let result = obi.update(&book, None, &clock).unwrap();
    assert!(result > 0.0, "expected positive OBI, got {result}");
    assert_signal_in_range(result, 0.0, 1.0);
    // (300-3)/(300+3) = 297/303 ≈ 0.9802
    assert!((result - 297.0 / 303.0).abs() < EPSILON);
}

#[test]
fn heavy_ask_pressure_negative_obi() {
    let clock = sim_clock();
    let mut obi = ObiSignal::new(1, FixedPrice::new(MAX_SPREAD));
    let book = tradeable_book(&[1, 1, 1], &[100, 100, 100]);

    let result = obi.update(&book, None, &clock).unwrap();
    assert!(result < 0.0, "expected negative OBI, got {result}");
    assert_signal_in_range(result, -1.0, 0.0);
    // (3-300)/(3+300) = -297/303 ≈ -0.9802
    assert!((result - (-297.0 / 303.0)).abs() < EPSILON);
}

#[test]
fn is_valid_false_before_warmup() {
    let clock = sim_clock();
    let mut obi = ObiSignal::new(3, FixedPrice::new(MAX_SPREAD));
    let book = tradeable_book(&[10, 10, 10], &[10, 10, 10]);

    assert!(!obi.is_valid());

    // First update — warmup not reached
    let r1 = obi.update(&book, None, &clock);
    assert!(r1.is_none()); // not yet warm
    assert!(!obi.is_valid());

    // Second update
    let r2 = obi.update(&book, None, &clock);
    assert!(r2.is_none());
    assert!(!obi.is_valid());

    // Third update — warmup reached
    let r3 = obi.update(&book, None, &clock);
    assert!(r3.is_some());
    assert!(obi.is_valid());
}

#[test]
fn reset_clears_state() {
    let clock = sim_clock();
    let mut obi = ObiSignal::new(1, FixedPrice::new(MAX_SPREAD));
    let book = tradeable_book(&[10, 10, 10], &[10, 10, 10]);

    obi.update(&book, None, &clock);
    assert!(obi.is_valid());

    obi.reset();
    assert!(!obi.is_valid());
}

#[test]
fn snapshot_captures_state() {
    let clock = sim_clock();
    let mut obi = ObiSignal::new(1, FixedPrice::new(MAX_SPREAD));
    let book = tradeable_book(&[10, 10, 10], &[10, 10, 10]);

    // Before any update
    let snap = obi.snapshot();
    assert_eq!(snap.name, "obi");
    assert!(snap.value.is_none());
    assert!(!snap.valid);

    // After update
    obi.update(&book, None, &clock);
    let snap = obi.snapshot();
    assert_eq!(snap.name, "obi");
    assert!(snap.value.is_some());
    assert!(snap.valid);
    assert!((snap.value.unwrap() - 0.0).abs() < EPSILON);
}

#[test]
fn non_tradeable_book_returns_none() {
    let clock = sim_clock();
    let mut obi = ObiSignal::new(1, FixedPrice::new(MAX_SPREAD));

    // Only 2 levels per side — is_tradeable requires 3
    let book = OrderBookBuilder::new()
        .bid(100.0, 10)
        .bid(99.75, 10)
        .ask(100.25, 10)
        .ask(100.50, 10)
        .build();

    assert!(obi.update(&book, None, &clock).is_none());
    assert!(!obi.is_valid());
}

#[test]
fn empty_book_returns_none() {
    let clock = sim_clock();
    let mut obi = ObiSignal::new(1, FixedPrice::new(MAX_SPREAD));
    let book = OrderBook::empty();

    assert!(obi.update(&book, None, &clock).is_none());
    assert!(!obi.is_valid());
}

#[test]
fn name_returns_obi() {
    let obi = ObiSignal::new(1, FixedPrice::new(MAX_SPREAD));
    assert_eq!(obi.name(), "obi");
}

#[test]
fn asymmetric_levels_correct_obi() {
    let clock = sim_clock();
    let mut obi = ObiSignal::new(1, FixedPrice::new(MAX_SPREAD));
    // 5 bid levels, 3 ask levels — different depths
    let book = OrderBookBuilder::new()
        .bid(100.0, 20)
        .bid(99.75, 15)
        .bid(99.50, 10)
        .bid(99.25, 5)
        .bid(99.0, 5)
        .ask(100.25, 10)
        .ask(100.50, 10)
        .ask(100.75, 10)
        .build();

    let result = obi.update(&book, None, &clock).unwrap();
    // total_bid = 55, total_ask = 30
    // OBI = (55 - 30) / (55 + 30) = 25/85
    let expected = 25.0 / 85.0;
    assert!((result - expected).abs() < EPSILON, "expected {expected}, got {result}");
    assert_signal_in_range(result, -1.0, 1.0);
}

#[test]
fn valid_to_non_tradeable_invalidates_signal() {
    let clock = sim_clock();
    let mut obi = ObiSignal::new(1, FixedPrice::new(MAX_SPREAD));
    let good_book = tradeable_book(&[10, 10, 10], &[10, 10, 10]);

    // Warm up — signal becomes valid
    obi.update(&good_book, None, &clock);
    assert!(obi.is_valid());
    assert!(obi.snapshot().value.is_some());

    // Non-tradeable book — signal must invalidate
    let bad_book = OrderBookBuilder::new()
        .bid(100.0, 10)
        .bid(99.75, 10)
        .ask(100.25, 10)
        .ask(100.50, 10)
        .build();

    let result = obi.update(&bad_book, None, &clock);
    assert!(result.is_none());
    assert!(!obi.is_valid());
    assert!(obi.snapshot().value.is_none());
}

#[test]
fn warmup_zero_emits_on_first_update() {
    let clock = sim_clock();
    let mut obi = ObiSignal::new(0, FixedPrice::new(MAX_SPREAD));
    let book = tradeable_book(&[10, 10, 10], &[10, 10, 10]);

    let result = obi.update(&book, None, &clock);
    assert!(result.is_some());
    assert!(obi.is_valid());
}
