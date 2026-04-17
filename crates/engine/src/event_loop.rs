use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_bmad_core::{Clock, OrderBook};
use tracing::info;

use crate::buffer_monitor::{BufferMonitor, BufferState};
use crate::data_quality::{
    DataQualityGate, GateReason, GateState, SequenceGapDetector, StaleDataDetector,
};
use crate::order_book::apply_market_event;
use crate::spsc::MarketEventConsumer;

/// How often to check for stale data (in loop iterations).
const STALE_CHECK_INTERVAL: u64 = 1000;

/// Hot-path event loop that consumes MarketEvents from SPSC and updates OrderBook.
/// Runs on a dedicated pinned core. Never allocates after initialization.
pub struct EventLoop<C: Clock> {
    consumer: MarketEventConsumer,
    book: OrderBook,
    monitor: BufferMonitor,
    stale_detector: StaleDataDetector,
    seq_detector: SequenceGapDetector,
    gate: DataQualityGate,
    clock: C,
    events_processed: u64,
    tick_count: u64,
    running: Arc<AtomicBool>,
}

/// Handle for stopping the event loop from another thread.
#[derive(Clone)]
pub struct EventLoopHandle {
    running: Arc<AtomicBool>,
}

impl EventLoopHandle {
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }
}

impl<C: Clock> EventLoop<C> {
    pub fn new(consumer: MarketEventConsumer, clock: C, stale_threshold_secs: f64) -> Self {
        Self {
            consumer,
            book: OrderBook::empty(),
            monitor: BufferMonitor::new(),
            stale_detector: StaleDataDetector::new(stale_threshold_secs),
            seq_detector: SequenceGapDetector::new(),
            gate: DataQualityGate::new(),
            clock,
            events_processed: 0,
            tick_count: 0,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Get a handle that can stop the event loop from another thread.
    pub fn handle(&self) -> EventLoopHandle {
        EventLoopHandle {
            running: self.running.clone(),
        }
    }

    /// Pin the current thread to a specific CPU core.
    pub fn pin_to_core(core_id: usize) -> bool {
        let core = core_affinity::CoreId { id: core_id };
        core_affinity::set_for_current(core)
    }

    /// Run the event loop. Spins, polling the SPSC consumer.
    /// Returns when `stop()` is called via handle.
    pub fn run(&mut self) {
        self.running.store(true, Ordering::Release);
        info!("event loop started");

        while self.running.load(Ordering::Acquire) {
            self.tick();

            if self.consumer.is_empty() {
                std::hint::spin_loop();
            }
        }

        info!(
            events_processed = self.events_processed,
            "event loop stopped"
        );
    }

    /// Process one iteration: pop event, check thresholds, update book, check data quality.
    /// Returns the buffer state after this tick.
    pub fn tick(&mut self) -> BufferState {
        self.tick_count += 1;

        // Check buffer state
        let fill = self.consumer.fill_fraction();
        let state = self.monitor.update(fill);

        // Pop and process event
        if let Some(event) = self.consumer.try_pop() {
            let now = event.timestamp.as_nanos();

            // Update order book regardless of gate state
            apply_market_event(&mut self.book, &event);
            self.events_processed += 1;

            // Update stale detector with wall clock time (not event timestamp)
            // so freshness is measured from when we actually received data
            let wall_now = self.clock.now().as_nanos();
            self.stale_detector.on_tick(wall_now);

            // Check sequence continuity
            let seq_gap = self
                .seq_detector
                .check_sequence(event.symbol_id, event.sequence);
            if let Some((expected, received)) = seq_gap {
                self.gate.activate(
                    &GateReason::SequenceGap {
                        symbol_id: event.symbol_id,
                        expected,
                        received,
                    },
                    wall_now,
                );
            }

            // If gate was active and data is now fresh (no stale, no seq gap), auto-clear
            if !self.gate.is_open() && seq_gap.is_none() {
                let stale = self
                    .stale_detector
                    .check_stale(wall_now, self.clock.is_market_open());
                if stale.is_none() {
                    self.gate.clear(wall_now);
                }
            }
        }

        // Periodic stale check (every N iterations) to detect silence
        if self.tick_count % STALE_CHECK_INTERVAL == 0 {
            let now = self.clock.now().as_nanos();
            let market_open = self.clock.is_market_open();
            if let Some(gap_nanos) = self.stale_detector.check_stale(now, market_open) {
                self.gate
                    .activate(&GateReason::StaleData { gap_nanos }, now);
            }
        }

        state
    }

    pub fn order_book(&self) -> &OrderBook {
        &self.book
    }

    pub fn buffer_state(&self) -> BufferState {
        self.monitor.state()
    }

    pub fn gate_state(&self) -> GateState {
        self.gate.state()
    }

    pub fn is_data_quality_ok(&self) -> bool {
        self.gate.is_open()
    }

    pub fn events_processed(&self) -> u64 {
        self.events_processed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spsc::market_event_queue;
    use futures_bmad_core::{FixedPrice, MarketEvent, MarketEventType, Side, UnixNanos};
    use futures_bmad_testkit::SimClock;

    fn make_bid(price_raw: i64, size: u32, ts: u64) -> MarketEvent {
        MarketEvent {
            timestamp: UnixNanos::new(ts),
            symbol_id: 0,
            sequence: 0,
            event_type: MarketEventType::BidUpdate,
            price: FixedPrice::new(price_raw),
            size,
            side: Some(Side::Buy),
        }
    }

    fn make_ask(price_raw: i64, size: u32, ts: u64) -> MarketEvent {
        MarketEvent {
            timestamp: UnixNanos::new(ts),
            symbol_id: 0,
            sequence: 0,
            event_type: MarketEventType::AskUpdate,
            price: FixedPrice::new(price_raw),
            size,
            side: Some(Side::Sell),
        }
    }

    const NANOS_PER_SEC: u64 = 1_000_000_000;
    const BASE_TS: u64 = 1_000_000_000_000;

    #[test]
    fn event_loop_processes_events() {
        let (mut producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut event_loop = EventLoop::new(consumer, clock, 3.0);

        producer.try_push(make_bid(18000, 50, BASE_TS));
        producer.try_push(make_ask(18004, 30, BASE_TS));

        event_loop.tick();
        event_loop.tick();

        assert_eq!(event_loop.events_processed(), 2);
        assert_eq!(
            event_loop.order_book().best_bid().unwrap().price.raw(),
            18000
        );
        assert_eq!(
            event_loop.order_book().best_ask().unwrap().price.raw(),
            18004
        );
    }

    #[test]
    fn tick_returns_normal_when_buffer_low() {
        let (_producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut event_loop = EventLoop::new(consumer, clock, 3.0);

        let state = event_loop.tick();
        assert_eq!(state, BufferState::Normal);
    }

    #[test]
    fn buffer_state_escalates_with_fill() {
        let (mut producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut event_loop = EventLoop::new(consumer, clock, 3.0);

        for i in 0..13 {
            producer.try_push(make_bid(18000 + i, 10, BASE_TS));
        }

        let state = event_loop.tick();
        assert_eq!(state, BufferState::TradingDisabled);
    }

    #[test]
    fn handle_can_stop_loop() {
        let (_producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut event_loop = EventLoop::new(consumer, clock, 3.0);
        let handle = event_loop.handle();

        event_loop.running.store(true, Ordering::Release);
        assert!(event_loop.running.load(Ordering::Acquire));
        handle.stop();
        assert!(!event_loop.running.load(Ordering::Acquire));
    }

    #[test]
    fn stale_data_gates_then_clears() {
        let (mut producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut event_loop = EventLoop::new(consumer, clock, 3.0);

        // Send initial tick
        producer.try_push(make_bid(18000, 50, BASE_TS));
        event_loop.tick();
        assert!(event_loop.is_data_quality_ok());

        // Advance clock past stale threshold and trigger periodic check
        event_loop
            .clock
            .set_time(UnixNanos::new(BASE_TS + 5 * NANOS_PER_SEC));
        // Run enough ticks to trigger periodic stale check
        for _ in 0..STALE_CHECK_INTERVAL {
            event_loop.tick();
        }
        assert!(!event_loop.is_data_quality_ok());

        // Fresh data arrives — gate auto-clears
        let fresh_ts = BASE_TS + 6 * NANOS_PER_SEC;
        producer.try_push(make_bid(18001, 50, fresh_ts));
        event_loop.clock.set_time(UnixNanos::new(fresh_ts));
        event_loop.tick();
        assert!(event_loop.is_data_quality_ok());
    }

    #[test]
    fn stale_not_triggered_when_market_closed() {
        let (mut producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        clock.set_market_open(false);
        let mut event_loop = EventLoop::new(consumer, clock, 3.0);

        // Send tick
        producer.try_push(make_bid(18000, 50, BASE_TS));
        event_loop.tick();

        // Advance clock past threshold
        event_loop
            .clock
            .set_time(UnixNanos::new(BASE_TS + 10 * NANOS_PER_SEC));
        for _ in 0..STALE_CHECK_INTERVAL {
            event_loop.tick();
        }

        // Gate should NOT be activated — market is closed
        assert!(event_loop.is_data_quality_ok());
    }
}
