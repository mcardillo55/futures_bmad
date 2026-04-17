use futures_bmad_core::{FixedPrice, MarketEvent, MarketEventType, Side, UnixNanos};
use futures_bmad_engine::{BufferState, EventLoop, market_event_queue};
use futures_bmad_testkit::SimClock;

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
        sequence: 0,
        event_type,
        price: FixedPrice::new(price_raw),
        size,
        side,
    }
}

const BASE_TS: u64 = 1_000_000_000_000;

#[test]
fn l1_update_sequence_produces_correct_book() {
    let (mut producer, consumer) = market_event_queue(64);
    let clock = SimClock::new(BASE_TS);
    let mut event_loop = EventLoop::new(consumer, clock, 3.0);

    assert!(event_loop.order_book().best_bid().is_none());
    assert!(event_loop.order_book().best_ask().is_none());

    producer.try_push(make_event(
        MarketEventType::BidUpdate,
        18000,
        50,
        Some(Side::Buy),
        BASE_TS,
    ));
    event_loop.tick();
    assert_eq!(
        event_loop.order_book().best_bid().unwrap().price.raw(),
        18000
    );
    assert!(event_loop.order_book().best_ask().is_none());

    producer.try_push(make_event(
        MarketEventType::AskUpdate,
        18004,
        30,
        Some(Side::Sell),
        BASE_TS + 1,
    ));
    event_loop.tick();
    assert_eq!(
        event_loop.order_book().best_ask().unwrap().price.raw(),
        18004
    );
    assert_eq!(event_loop.order_book().spread().unwrap().raw(), 4);

    producer.try_push(make_event(
        MarketEventType::Trade,
        18002,
        10,
        Some(Side::Buy),
        BASE_TS + 2,
    ));
    event_loop.tick();
    assert_eq!(
        event_loop.order_book().best_bid().unwrap().price.raw(),
        18000
    );

    producer.try_push(make_event(
        MarketEventType::BidUpdate,
        18002,
        100,
        Some(Side::Buy),
        BASE_TS + 3,
    ));
    event_loop.tick();
    assert_eq!(
        event_loop.order_book().best_bid().unwrap().price.raw(),
        18002
    );
    assert_eq!(event_loop.order_book().spread().unwrap().raw(), 2);
    assert_eq!(event_loop.events_processed(), 4);
}

#[test]
fn is_tradeable_edge_cases_through_pipeline() {
    let (mut producer, consumer) = market_event_queue(64);
    let clock = SimClock::new(BASE_TS);
    let mut event_loop = EventLoop::new(consumer, clock, 3.0);
    let threshold = FixedPrice::new(100);

    assert!(!event_loop.order_book().is_tradeable(threshold));

    producer.try_push(make_event(
        MarketEventType::BidUpdate,
        18000,
        50,
        Some(Side::Buy),
        BASE_TS,
    ));
    event_loop.tick();
    assert!(!event_loop.order_book().is_tradeable(threshold));

    producer.try_push(make_event(
        MarketEventType::AskUpdate,
        18004,
        30,
        Some(Side::Sell),
        BASE_TS + 1,
    ));
    event_loop.tick();
    assert!(!event_loop.order_book().is_tradeable(threshold));
}

#[test]
fn buffer_state_under_load() {
    let (mut producer, consumer) = market_event_queue(16);
    let clock = SimClock::new(BASE_TS);
    let mut event_loop = EventLoop::new(consumer, clock, 3.0);

    let state = event_loop.tick();
    assert_eq!(state, BufferState::Normal);

    for i in 0..13 {
        producer.try_push(make_event(
            MarketEventType::BidUpdate,
            18000 + i,
            10,
            Some(Side::Buy),
            BASE_TS + i as u64,
        ));
    }

    let state = event_loop.tick();
    assert_eq!(state, BufferState::TradingDisabled);

    for _ in 0..12 {
        event_loop.tick();
    }

    let state = event_loop.tick();
    assert_eq!(state, BufferState::Normal);
}

#[test]
fn spsc_drop_counting() {
    let (mut producer, _consumer) = market_event_queue(4);

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

    for _ in 0..5 {
        assert!(!producer.try_push(make_event(MarketEventType::Trade, 18000, 10, None, 99)));
    }
    assert_eq!(producer.drop_count(), 5);
}
