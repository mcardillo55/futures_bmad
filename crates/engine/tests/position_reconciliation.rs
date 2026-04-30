//! Integration tests for the Story 4-5 PositionTracker + reconciliation
//! engine wired against a MockBrokerAdapter.
//!
//! These exercise the full end-to-end path Story 4-5 specifies:
//!   1. local engine processes fills -> PositionTracker.apply_fill
//!   2. reconciliation triggered -> BrokerAdapter::query_positions invoked
//!   3. PositionTracker::reconcile_and_handle compares local vs broker
//!   4. on mismatch, circuit breaker callback fires + trading halts
//!   5. on consistency, normal operation continues
//!
//! Reconciliation triggers (startup / reconnection / periodic 60s) are owned
//! by the lifecycle FSM (story 8-2 / 8-4) — the seams here let those stories
//! plug in without redesign.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_bmad_core::{
    BrokerAdapter, BrokerPosition, FillEvent, FillType, FixedPrice, Side, UnixNanos,
};
use futures_bmad_engine::order_manager::CircuitBreakerCallback;
use futures_bmad_engine::order_manager::tracker::{
    PositionTracker, ReconciliationResult, ReconciliationTrigger,
};
use futures_bmad_engine::persistence::journal::EventJournal;
use futures_bmad_testkit::mock_broker::{MockBehavior, MockBrokerAdapter};

fn make_fill(side: Side, price_raw: i64, size: u32) -> FillEvent {
    FillEvent {
        order_id: 1,
        fill_price: FixedPrice::new(price_raw),
        fill_size: size,
        timestamp: UnixNanos::new(1),
        side,
        decision_id: 0,
        fill_type: FillType::Full,
    }
}

#[tokio::test]
async fn reconcile_against_mock_broker_consistent_path() {
    let (journal, _rx) = EventJournal::channel();
    let mut tracker = PositionTracker::new(journal);
    tracker.apply_fill(&make_fill(Side::Buy, 100, 2), 1);

    let mut broker = MockBrokerAdapter::new(MockBehavior::Fill);
    broker.set_positions(vec![BrokerPosition {
        symbol_id: 1,
        side: Some(Side::Buy),
        quantity: 2,
        avg_entry_price: FixedPrice::new(100),
    }]);

    let view = broker
        .query_positions()
        .await
        .expect("mock returns Ok by default");
    let result =
        tracker.reconcile_and_handle(&view, ReconciliationTrigger::Startup, UnixNanos::default());
    assert!(result.is_consistent());
    assert!(!tracker.trading_halted());
}

#[tokio::test]
async fn reconcile_against_mock_broker_phantom_position_halts_trading() {
    let trips = Arc::new(AtomicUsize::new(0));
    let cb_trips = trips.clone();
    let cb: CircuitBreakerCallback = Arc::new(move |_| {
        cb_trips.fetch_add(1, Ordering::SeqCst);
    });

    let (journal, _rx) = EventJournal::channel();
    let mut tracker = PositionTracker::new(journal).with_circuit_breaker(cb);
    // Local engine thinks we hold 2 contracts.
    tracker.apply_fill(&make_fill(Side::Buy, 100, 2), 1);

    // Broker reports flat — phantom position scenario.
    let broker = MockBrokerAdapter::new(MockBehavior::Fill);
    let view = broker.query_positions().await.unwrap();
    let result =
        tracker.reconcile_and_handle(&view, ReconciliationTrigger::Periodic, UnixNanos::default());
    match result {
        ReconciliationResult::Mismatch(_) => {}
        other => panic!("expected mismatch, got {other:?}"),
    }
    assert!(tracker.trading_halted());
    assert_eq!(trips.load(Ordering::SeqCst), 1);

    // CRITICAL: local position is unchanged — NO silent correction (NFR16).
    let p = tracker.local_position(1).unwrap();
    assert_eq!(p.quantity, 2);
    assert_eq!(p.side, Some(Side::Buy));
}

#[tokio::test]
async fn reconcile_against_mock_broker_missed_fill_halts_trading() {
    let trips = Arc::new(AtomicUsize::new(0));
    let cb_trips = trips.clone();
    let cb: CircuitBreakerCallback = Arc::new(move |_| {
        cb_trips.fetch_add(1, Ordering::SeqCst);
    });

    let (journal, _rx) = EventJournal::channel();
    let mut tracker = PositionTracker::new(journal).with_circuit_breaker(cb);
    // Local engine has no positions at all.

    // Broker reports a 1-lot short — a missed fill.
    let mut broker = MockBrokerAdapter::new(MockBehavior::Fill);
    broker.set_positions(vec![BrokerPosition {
        symbol_id: 1,
        side: Some(Side::Sell),
        quantity: 1,
        avg_entry_price: FixedPrice::new(200),
    }]);
    let view = broker.query_positions().await.unwrap();
    let result = tracker.reconcile_and_handle(
        &view,
        ReconciliationTrigger::Reconnection,
        UnixNanos::default(),
    );
    assert!(!result.is_consistent());
    assert!(tracker.trading_halted());
    assert_eq!(trips.load(Ordering::SeqCst), 1);

    // Local stays empty — silent correction is forbidden.
    assert!(tracker.local_position(1).is_none());
}
