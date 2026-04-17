use futures_bmad_core::{
    FixedPrice, MarketEvent, MarketEventType, OrderBook, Side, Signal, UnixNanos,
};
use futures_bmad_engine::signals::VpinSignal;
use futures_bmad_testkit::SimClock;

const EPSILON: f64 = 1e-10;

fn sim_clock() -> SimClock {
    SimClock::new(1_000_000_000)
}

fn empty_book() -> OrderBook {
    OrderBook::empty()
}

fn make_trade(price: f64, size: u32, side: Option<Side>) -> MarketEvent {
    MarketEvent {
        timestamp: UnixNanos::default(),
        symbol_id: 1,
        sequence: 0,
        event_type: MarketEventType::Trade,
        price: FixedPrice::from_f64(price).unwrap(),
        size,
        side,
    }
}

/// Feed N buy trades of given size into the signal.
fn feed_buys(vpin: &mut VpinSignal, clock: &SimClock, book: &OrderBook, count: usize, size: u32) {
    for _ in 0..count {
        let trade = make_trade(100.0, size, Some(Side::Buy));
        clock.advance_by(1_000_000);
        vpin.update(book, Some(&trade), clock);
    }
}

/// Feed N sell trades of given size into the signal.
fn feed_sells(
    vpin: &mut VpinSignal,
    clock: &SimClock,
    book: &OrderBook,
    count: usize,
    size: u32,
) {
    for _ in 0..count {
        let trade = make_trade(100.0, size, Some(Side::Sell));
        clock.advance_by(1_000_000);
        vpin.update(book, Some(&trade), clock);
    }
}

#[test]
fn all_buy_volume_produces_vpin_one() {
    let clock = sim_clock();
    let book = empty_book();
    // bucket_size=100, num_buckets=2 -> need 200 volume to fill 2 buckets
    let mut vpin = VpinSignal::new(100, 2);

    // Fill 2 buckets entirely with buy volume
    feed_buys(&mut vpin, &clock, &book, 20, 10); // 200 total

    assert!(vpin.is_valid());
    let snap = vpin.snapshot();
    let val = snap.value.unwrap();
    // All buy, no sell -> |100-0|/100 + |100-0|/100 = 200/200 = 1.0
    assert!((val - 1.0).abs() < EPSILON, "expected 1.0, got {val}");
}

#[test]
fn equal_buy_sell_produces_vpin_zero() {
    let clock = sim_clock();
    let book = empty_book();
    let mut vpin = VpinSignal::new(100, 2);

    // Fill each bucket with 50 buy + 50 sell = 100 total
    for _ in 0..2 {
        for _ in 0..5 {
            let buy = make_trade(100.0, 10, Some(Side::Buy));
            clock.advance_by(1_000_000);
            vpin.update(&book, Some(&buy), &clock);

            let sell = make_trade(100.0, 10, Some(Side::Sell));
            clock.advance_by(1_000_000);
            vpin.update(&book, Some(&sell), &clock);
        }
    }

    assert!(vpin.is_valid());
    let val = vpin.snapshot().value.unwrap();
    assert!((val - 0.0).abs() < EPSILON, "expected 0.0, got {val}");
}

#[test]
fn mixed_sequence_expected_vpin() {
    let clock = sim_clock();
    let book = empty_book();
    // bucket_size=10, num_buckets=2
    let mut vpin = VpinSignal::new(10, 2);

    // Bucket 1: 8 buy + 2 sell = 10 total, imbalance = |8-2| = 6
    let buy = make_trade(100.0, 8, Some(Side::Buy));
    clock.advance_by(1_000_000);
    vpin.update(&book, Some(&buy), &clock);
    let sell = make_trade(100.0, 2, Some(Side::Sell));
    clock.advance_by(1_000_000);
    vpin.update(&book, Some(&sell), &clock);

    // Bucket 2: 3 buy + 7 sell = 10 total, imbalance = |3-7| = 4
    let buy = make_trade(100.0, 3, Some(Side::Buy));
    clock.advance_by(1_000_000);
    vpin.update(&book, Some(&buy), &clock);
    let sell = make_trade(100.0, 7, Some(Side::Sell));
    clock.advance_by(1_000_000);
    let result = vpin.update(&book, Some(&sell), &clock);

    assert!(vpin.is_valid());
    // VPIN = (6 + 4) / (10 + 10) = 10/20 = 0.5
    let val = result.unwrap();
    assert!((val - 0.5).abs() < EPSILON, "expected 0.5, got {val}");
}

#[test]
fn is_valid_false_before_num_buckets_filled() {
    let clock = sim_clock();
    let book = empty_book();
    let mut vpin = VpinSignal::new(100, 3);

    assert!(!vpin.is_valid());

    // Fill 1 bucket
    feed_buys(&mut vpin, &clock, &book, 10, 10);
    assert!(!vpin.is_valid());

    // Fill 2 buckets
    feed_buys(&mut vpin, &clock, &book, 10, 10);
    assert!(!vpin.is_valid());

    // Fill 3rd bucket -> valid
    feed_buys(&mut vpin, &clock, &book, 10, 10);
    assert!(vpin.is_valid());
}

#[test]
fn reset_clears_state() {
    let clock = sim_clock();
    let book = empty_book();
    let mut vpin = VpinSignal::new(100, 2);

    feed_buys(&mut vpin, &clock, &book, 20, 10);
    assert!(vpin.is_valid());

    vpin.reset();
    assert!(!vpin.is_valid());
    assert!(vpin.snapshot().value.is_none());
}

#[test]
fn trade_none_returns_cached_vpin() {
    let clock = sim_clock();
    let book = empty_book();
    let mut vpin = VpinSignal::new(100, 2);

    feed_buys(&mut vpin, &clock, &book, 20, 10);
    assert!(vpin.is_valid());
    let cached = vpin.snapshot().value.unwrap();

    // Book-only update should return cached value
    let result = vpin.update(&book, None, &clock);
    assert!(result.is_some());
    assert!((result.unwrap() - cached).abs() < EPSILON);
}

#[test]
fn trade_none_before_any_trades_returns_none() {
    let clock = sim_clock();
    let book = empty_book();
    let mut vpin = VpinSignal::new(100, 2);

    let result = vpin.update(&book, None, &clock);
    assert!(result.is_none());
}

#[test]
fn bucket_rollover_evicts_oldest() {
    let clock = sim_clock();
    let book = empty_book();
    // 2 buckets, size 10
    let mut vpin = VpinSignal::new(10, 2);

    // Bucket 1: all buy (10 buy, 0 sell) -> imbalance 10
    feed_buys(&mut vpin, &clock, &book, 10, 1);

    // Bucket 2: all buy (10 buy, 0 sell) -> imbalance 10
    feed_buys(&mut vpin, &clock, &book, 10, 1);
    assert!(vpin.is_valid());
    let val = vpin.snapshot().value.unwrap();
    assert!((val - 1.0).abs() < EPSILON);

    // Bucket 3: all sell (0 buy, 10 sell) -> imbalance 10
    // This evicts bucket 1. Window is now: bucket2 (all buy) + bucket3 (all sell)
    feed_sells(&mut vpin, &clock, &book, 10, 1);
    let val = vpin.snapshot().value.unwrap();
    // VPIN = (10 + 10) / (10 + 10) = 1.0 (both buckets fully one-sided)
    assert!((val - 1.0).abs() < EPSILON);
}

#[test]
fn snapshot_captures_state() {
    let clock = sim_clock();
    let book = empty_book();
    let mut vpin = VpinSignal::new(100, 2);

    let snap = vpin.snapshot();
    assert_eq!(snap.name, "vpin");
    assert!(snap.value.is_none());
    assert!(!snap.valid);

    feed_buys(&mut vpin, &clock, &book, 20, 10);

    let snap = vpin.snapshot();
    assert_eq!(snap.name, "vpin");
    assert!(snap.value.is_some());
    assert!(snap.valid);
}

#[test]
fn zero_volume_trade_ignored() {
    let clock = sim_clock();
    let book = empty_book();
    let mut vpin = VpinSignal::new(100, 2);

    let trade = make_trade(100.0, 0, Some(Side::Buy));
    let result = vpin.update(&book, Some(&trade), &clock);
    assert!(result.is_none());
    assert!(!vpin.is_valid());
}

#[test]
fn tick_rule_fallback_classifies_correctly() {
    let clock = sim_clock();
    let book = empty_book();
    // bucket_size=10, num_buckets=1
    let mut vpin = VpinSignal::new(10, 1);

    // First trade with explicit side establishes price baseline
    let t1 = make_trade(100.0, 3, Some(Side::Buy));
    clock.advance_by(1_000_000);
    vpin.update(&book, Some(&t1), &clock);

    // Up-tick (no side) -> classified as buy
    let t2 = make_trade(100.25, 3, None);
    clock.advance_by(1_000_000);
    vpin.update(&book, Some(&t2), &clock);

    // Down-tick (no side) -> classified as sell
    let t3 = make_trade(99.75, 4, None);
    clock.advance_by(1_000_000);
    let result = vpin.update(&book, Some(&t3), &clock);

    // Bucket: buy=6, sell=4, total=10, imbalance=2
    // VPIN = 2/10 = 0.2
    assert!(result.is_some());
    assert!((result.unwrap() - 0.2).abs() < EPSILON);
}

#[test]
fn name_returns_vpin() {
    let vpin = VpinSignal::new(100, 2);
    assert_eq!(vpin.name(), "vpin");
}

#[test]
fn large_trade_fills_multiple_buckets() {
    let clock = sim_clock();
    let book = empty_book();
    // bucket_size=10, num_buckets=2
    let mut vpin = VpinSignal::new(10, 2);

    // Single trade of 25 -> fills 2 complete buckets (10 each), 5 overflow
    let trade = make_trade(100.0, 25, Some(Side::Buy));
    clock.advance_by(1_000_000);
    let result = vpin.update(&book, Some(&trade), &clock);

    // Both buckets are all-buy -> VPIN = 1.0
    assert!(result.is_some());
    assert!((result.unwrap() - 1.0).abs() < EPSILON);
}

#[test]
fn overflow_with_mixed_sides_no_underflow() {
    let clock = sim_clock();
    let book = empty_book();
    // bucket_size=100, num_buckets=1
    let mut vpin = VpinSignal::new(100, 1);

    // Fill bucket with 90 sell
    feed_sells(&mut vpin, &clock, &book, 9, 10);
    // Now add 20 buy -> total=110, overflow=10, buy=20 sell=90
    // The overflow (10) should be trimmed from buy side (min(10,20)=10)
    // Completed: buy=10, sell=90
    let buy = make_trade(100.0, 20, Some(Side::Buy));
    clock.advance_by(1_000_000);
    let result = vpin.update(&book, Some(&buy), &clock);

    assert!(result.is_some());
    // Bucket: buy=10, sell=90, total=100, imbalance=80
    // VPIN = 80/100 = 0.8
    let val = result.unwrap();
    assert!((val - 0.8).abs() < EPSILON, "expected 0.8, got {val}");
}

#[test]
#[should_panic(expected = "bucket_size must be > 0")]
fn bucket_size_zero_panics() {
    VpinSignal::new(0, 2);
}

#[test]
#[should_panic(expected = "num_buckets must be > 0")]
fn num_buckets_zero_panics() {
    VpinSignal::new(100, 0);
}
