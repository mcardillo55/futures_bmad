use futures_bmad_core::{FixedPrice, MarketEvent, MarketEventType, OrderBook, Side, UnixNanos};
use futures_bmad_engine::{market_event_queue, BufferState, EventLoop};

fn make_event(
    event_type: MarketEventType,
    price_raw: i64,
    size: u32,
    side: Option<Side>,
    ts: u64,
) -> MarketEvent {
    MarketEvent {
        timestamp: UnixNanos::new(ts),
        symbol_id: 0,
        event_type,
        price: FixedPrice::new(price_raw),
        size,
        side,
    }
}

/// Integration test: feed a sequence of L1 updates through SPSC -> EventLoop -> OrderBook
/// and verify the book state at each step.
#[test]
fn l1_update_sequence_produces_correct_book() {
    let (mut producer, consumer) = market_event_queue(64);
    let mut event_loop = EventLoop::new(consumer);

    // Step 1: Empty book — not tradeable
    assert!(event_loop.order_book().best_bid().is_none());
    assert!(event_loop.order_book().best_ask().is_none());

    // Step 2: Add bid only — still not tradeable
    producer.try_push(make_event(
        MarketEventType::BidUpdate,
        18000,
        50,
        Some(Side::Buy),
        1,
    ));
    event_loop.tick();
    assert_eq!(
        event_loop.order_book().best_bid().unwrap().price.raw(),
        18000
    );
    assert!(event_loop.order_book().best_ask().is_none());

    // Step 3: Add ask — book now has both sides
    producer.try_push(make_event(
        MarketEventType::AskUpdate,
        18004,
        30,
        Some(Side::Sell),
        2,
    ));
    event_loop.tick();
    assert_eq!(
        event_loop.order_book().best_ask().unwrap().price.raw(),
        18004
    );
    assert_eq!(event_loop.order_book().spread().unwrap().raw(), 4);
    assert_eq!(event_loop.order_book().mid_price().unwrap().raw(), 18002);

    // Step 4: Trade event — doesn't change the book
    producer.try_push(make_event(
        MarketEventType::Trade,
        18002,
        10,
        Some(Side::Buy),
        3,
    ));
    event_loop.tick();
    assert_eq!(
        event_loop.order_book().best_bid().unwrap().price.raw(),
        18000
    );
    assert_eq!(
        event_loop.order_book().best_ask().unwrap().price.raw(),
        18004
    );

    // Step 5: Update bid with new price
    producer.try_push(make_event(
        MarketEventType::BidUpdate,
        18002,
        100,
        Some(Side::Buy),
        4,
    ));
    event_loop.tick();
    assert_eq!(
        event_loop.order_book().best_bid().unwrap().price.raw(),
        18002
    );
    assert_eq!(event_loop.order_book().best_bid().unwrap().size, 100);

    // Spread should now be 18004 - 18002 = 2
    assert_eq!(event_loop.order_book().spread().unwrap().raw(), 2);

    assert_eq!(event_loop.events_processed(), 4);
}

/// Test is_tradeable for various book states through the pipeline.
#[test]
fn is_tradeable_edge_cases_through_pipeline() {
    let (mut producer, consumer) = market_event_queue(64);
    let mut event_loop = EventLoop::new(consumer);
    let threshold = FixedPrice::new(100); // generous threshold

    // Empty book
    assert!(!event_loop.order_book().is_tradeable(threshold));

    // One-sided: bid only
    producer.try_push(make_event(
        MarketEventType::BidUpdate,
        18000,
        50,
        Some(Side::Buy),
        1,
    ));
    event_loop.tick();
    assert!(!event_loop.order_book().is_tradeable(threshold));

    // Both sides but only 1 level each (need >= 3 per side for is_tradeable)
    producer.try_push(make_event(
        MarketEventType::AskUpdate,
        18004,
        30,
        Some(Side::Sell),
        2,
    ));
    event_loop.tick();
    // is_tradeable requires >= 3 levels per side in the core OrderBook
    assert!(!event_loop.order_book().is_tradeable(threshold));
}

/// Test buffer state transitions during high load.
#[test]
fn buffer_state_under_load() {
    let (mut producer, consumer) = market_event_queue(16);
    let mut event_loop = EventLoop::new(consumer);

    // Normal state when empty
    let state = event_loop.tick();
    assert_eq!(state, BufferState::Normal);

    // Fill to ~80% (13 out of 16)
    for i in 0..13 {
        producer.try_push(make_event(
            MarketEventType::BidUpdate,
            18000 + i,
            10,
            Some(Side::Buy),
            i as u64,
        ));
    }

    let state = event_loop.tick();
    assert_eq!(state, BufferState::TradingDisabled);

    // Drain the buffer by consuming events
    for _ in 0..12 {
        event_loop.tick();
    }

    // After draining, buffer should eventually return to normal
    let state = event_loop.tick();
    assert_eq!(state, BufferState::Normal);
}

/// Test that SPSC drops events when full and counts correctly.
#[test]
fn spsc_drop_counting() {
    let (mut producer, _consumer) = market_event_queue(4);

    // Fill the buffer
    for i in 0..4 {
        assert!(producer.try_push(make_event(
            MarketEventType::Trade,
            18000 + i,
            10,
            None,
            i as u64
        )));
    }
    assert_eq!(producer.drop_count(), 0);

    // These should be dropped
    for _ in 0..5 {
        assert!(!producer.try_push(make_event(
            MarketEventType::Trade,
            18000,
            10,
            None,
            99
        )));
    }
    assert_eq!(producer.drop_count(), 5);
}
