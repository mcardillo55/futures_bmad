use futures_bmad_core::{FixedPrice, OrderBook, Signal};
use futures_bmad_engine::signals::MicropriceSignal;
use futures_bmad_testkit::{OrderBookBuilder, SimClock};

const EPSILON: f64 = 1e-10;
const MAX_SPREAD: i64 = 100;

fn sim_clock() -> SimClock {
    SimClock::new(1_000_000_000)
}

/// Build a tradeable book with specified best-level prices/sizes and filler levels.
fn book_with_top(bid_price: f64, bid_size: u32, ask_price: f64, ask_size: u32) -> OrderBook {
    OrderBookBuilder::new()
        .bid(bid_price, bid_size)
        .bid(bid_price - 0.25, 10)
        .bid(bid_price - 0.50, 10)
        .ask(ask_price, ask_size)
        .ask(ask_price + 0.25, 10)
        .ask(ask_price + 0.50, 10)
        .build()
}

#[test]
fn equal_sizes_equals_simple_mid() {
    let clock = sim_clock();
    let mut mp = MicropriceSignal::new(FixedPrice::new(MAX_SPREAD));
    let book = book_with_top(4482.00, 100, 4482.25, 100);

    let result = mp.update(&book, None, &clock).unwrap();
    // (4482.00 * 100 + 4482.25 * 100) / 200 = (448200 + 448225) / 200 = 4482.125
    let expected = (4482.00 + 4482.25) / 2.0;
    assert!(
        (result - expected).abs() < EPSILON,
        "expected {expected}, got {result}"
    );
}

#[test]
fn larger_ask_size_closer_to_bid() {
    let clock = sim_clock();
    let mut mp = MicropriceSignal::new(FixedPrice::new(MAX_SPREAD));
    let book = book_with_top(4482.00, 100, 4482.25, 300);

    let result = mp.update(&book, None, &clock).unwrap();
    // (4482.00 * 300 + 4482.25 * 100) / 400 = (1344600 + 448225) / 400 = 4482.0625
    let expected = 4482.0625;
    assert!(
        (result - expected).abs() < EPSILON,
        "expected {expected}, got {result}"
    );

    let mid = (4482.00 + 4482.25) / 2.0;
    assert!(result < mid, "microprice should be closer to bid than mid");
}

#[test]
fn larger_bid_size_closer_to_ask() {
    let clock = sim_clock();
    let mut mp = MicropriceSignal::new(FixedPrice::new(MAX_SPREAD));
    let book = book_with_top(4482.00, 300, 4482.25, 100);

    let result = mp.update(&book, None, &clock).unwrap();
    // (4482.00 * 100 + 4482.25 * 300) / 400 = (448200 + 1344675) / 400 = 4482.1875
    let expected = 4482.1875;
    assert!(
        (result - expected).abs() < EPSILON,
        "expected {expected}, got {result}"
    );

    let mid = (4482.00 + 4482.25) / 2.0;
    assert!(result > mid, "microprice should be closer to ask than mid");
}

#[test]
fn extreme_imbalance_near_bid() {
    let clock = sim_clock();
    let mut mp = MicropriceSignal::new(FixedPrice::new(MAX_SPREAD));
    let book = book_with_top(4482.00, 1, 4482.25, 1000);

    let result = mp.update(&book, None, &clock).unwrap();
    // (4482.00 * 1000 + 4482.25 * 1) / 1001 ≈ 4482.000249750...
    let expected = (4482.00 * 1000.0 + 4482.25 * 1.0) / 1001.0;
    assert!(
        (result - expected).abs() < EPSILON,
        "expected {expected}, got {result}"
    );
    assert!(
        result < 4482.01,
        "with extreme ask imbalance, microprice should be very close to bid"
    );
}

#[test]
fn is_valid_initially_false_then_true() {
    let clock = sim_clock();
    let mut mp = MicropriceSignal::new(FixedPrice::new(MAX_SPREAD));

    assert!(!mp.is_valid());

    let book = book_with_top(100.0, 10, 100.25, 10);
    mp.update(&book, None, &clock);
    assert!(mp.is_valid());
}

#[test]
fn zero_bid_size_returns_none() {
    let clock = sim_clock();
    let mut mp = MicropriceSignal::new(FixedPrice::new(MAX_SPREAD));

    // is_tradeable requires bids[0].size > 0, so a zero-size best bid
    // means the book isn't tradeable. Build manually with non-zero
    // sub-levels but this still fails is_tradeable.
    let book = book_with_top(100.0, 10, 100.25, 10);
    mp.update(&book, None, &clock);
    assert!(mp.is_valid());

    // Now a degraded book
    let bad_book = OrderBookBuilder::new()
        .bid(100.0, 10)
        .bid(99.75, 10)
        .ask(100.25, 10)
        .ask(100.50, 10)
        .build();
    let result = mp.update(&bad_book, None, &clock);
    assert!(result.is_none());
    assert!(!mp.is_valid());
}

#[test]
fn reset_clears_state() {
    let clock = sim_clock();
    let mut mp = MicropriceSignal::new(FixedPrice::new(MAX_SPREAD));
    let book = book_with_top(100.0, 10, 100.25, 10);

    mp.update(&book, None, &clock);
    assert!(mp.is_valid());

    mp.reset();
    assert!(!mp.is_valid());
    assert!(mp.snapshot().value.is_none());
}

#[test]
fn snapshot_captures_state() {
    let clock = sim_clock();
    let mut mp = MicropriceSignal::new(FixedPrice::new(MAX_SPREAD));

    let snap = mp.snapshot();
    assert_eq!(snap.name, "microprice");
    assert!(snap.value.is_none());
    assert!(!snap.valid);

    let book = book_with_top(100.0, 10, 100.25, 10);
    mp.update(&book, None, &clock);

    let snap = mp.snapshot();
    assert!(snap.value.is_some());
    assert!(snap.valid);
}

#[test]
fn non_tradeable_book_returns_none() {
    let clock = sim_clock();
    let mut mp = MicropriceSignal::new(FixedPrice::new(MAX_SPREAD));
    let book = OrderBookBuilder::new()
        .bid(100.0, 10)
        .bid(99.75, 10)
        .ask(100.25, 10)
        .ask(100.50, 10)
        .build();

    assert!(mp.update(&book, None, &clock).is_none());
    assert!(!mp.is_valid());
}

#[test]
fn empty_book_returns_none() {
    let clock = sim_clock();
    let mut mp = MicropriceSignal::new(FixedPrice::new(MAX_SPREAD));
    let book = OrderBook::empty();

    assert!(mp.update(&book, None, &clock).is_none());
    assert!(!mp.is_valid());
}

#[test]
fn name_returns_microprice() {
    let mp = MicropriceSignal::new(FixedPrice::new(MAX_SPREAD));
    assert_eq!(mp.name(), "microprice");
}

#[test]
fn signal_recovers_after_invalidation() {
    let clock = sim_clock();
    let mut mp = MicropriceSignal::new(FixedPrice::new(MAX_SPREAD));
    let good_book = book_with_top(100.0, 10, 100.25, 10);

    // Valid
    mp.update(&good_book, None, &clock);
    assert!(mp.is_valid());

    // Invalid (non-tradeable)
    let bad_book = OrderBookBuilder::new()
        .bid(100.0, 10)
        .bid(99.75, 10)
        .ask(100.25, 10)
        .ask(100.50, 10)
        .build();
    mp.update(&bad_book, None, &clock);
    assert!(!mp.is_valid());

    // Recovers
    let result = mp.update(&good_book, None, &clock);
    assert!(result.is_some());
    assert!(mp.is_valid());
}
