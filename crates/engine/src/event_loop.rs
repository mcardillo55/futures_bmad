use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_bmad_broker::FlattenRequest;
use futures_bmad_core::{Clock, EventWindowConfig, FixedPrice, OrderBook, Side, UnixNanos};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::buffer_monitor::{BufferMonitor, BufferState};
use crate::data_quality::{
    DataQualityGate, GateReason, GateState, SequenceGapDetector, StaleDataDetector,
};
use crate::order_book::apply_market_event;
use crate::order_manager::tracker::PositionTracker;
use crate::persistence::journal::{EngineEvent as JournalEvent, JournalSender, SystemEventRecord};
use crate::risk::circuit_breakers::AnomalyCheckOutcome;
use crate::risk::{CircuitBreakers, EventWindowManager, TradingRestriction};
use crate::spsc::MarketEventConsumer;

/// How often to check for stale data (in loop iterations).
const STALE_CHECK_INTERVAL: u64 = 1000;

/// Pre-Epic-6 cleanup D-2: source of "expected position" lookups for the
/// per-tick anomaly check.
///
/// Returns the sum of every active strategy's expected position for the
/// given `symbol_id`, or `0` when no strategy currently claims it. The
/// engine event loop calls this once per tracked symbol per anomaly-check
/// pass and forwards the value to
/// [`crate::risk::CircuitBreakers::check_position_anomaly`].
///
/// V1 (this commit) ships [`EmptyExpectedPositions`] — a default impl that
/// always returns `0`, which means any non-zero PositionTracker entry is
/// treated as "anomalous" until Epic 6's strategy orchestrator provides
/// the real impl. Decoupling the trait from Epic 6's strategy code keeps
/// this seam stable across the upcoming refactor.
pub trait ExpectedPositionSource: Send + 'static {
    fn expected_position(&self, symbol_id: u32) -> i32;

    /// Used by attach_anomaly_detection to log a safety warning when the default impl is wired bare.
    fn is_empty_default(&self) -> bool {
        false
    }
}

/// Compute the signed position from a [`futures_bmad_core::Position`]
/// (positive for long, negative for short, 0 for flat / no side).
fn signed_position(p: &futures_bmad_core::Position) -> i32 {
    // saturate at i32::MAX — quantities that large indicate corrupt state, but we should not silently flip sign
    let qty = i32::try_from(p.quantity).unwrap_or(i32::MAX);
    match p.side {
        Some(Side::Buy) => qty,
        Some(Side::Sell) => -qty,
        None => 0,
    }
}

/// Default `ExpectedPositionSource` returning zero for every symbol.
///
/// **SAFETY:** With this default attached, EVERY non-zero `PositionTracker` entry is
/// classified as anomalous and flatten requests are produced for it. Intended ONLY
/// for tests and for the seam Epic 6's strategy orchestrator will replace.
///
/// Production deployments MUST attach a real `ExpectedPositionSource` before
/// enabling live trading — otherwise the first held position triggers an immediate
/// flatten.
pub struct EmptyExpectedPositions;

impl ExpectedPositionSource for EmptyExpectedPositions {
    #[inline]
    fn expected_position(&self, _symbol_id: u32) -> i32 {
        0
    }

    #[inline]
    fn is_empty_default(&self) -> bool {
        true
    }
}

/// Default max-spread threshold used by the order book's
/// `is_tradeable()` check inside the event loop. Set when
/// `CircuitBreakers` wiring is attached; the value is the production
/// trading config's `max_spread_threshold` and is supplied by the
/// caller via `attach_circuit_breakers`.
#[derive(Debug, Clone, Copy)]
struct BookGuards {
    max_spread: FixedPrice,
}

/// Hot-path event loop that consumes MarketEvents from SPSC and updates OrderBook.
/// Runs on a dedicated pinned core. Never allocates after initialization.
///
/// Story 5.5 introduced the [`EventWindowManager`] field. The event-window
/// layer is a wall-clock-scheduled trading restriction that is evaluated
/// AFTER the circuit-breaker gate (story 5.1) — see
/// [`Self::current_trading_restriction`]. Event windows do NOT trip
/// breakers; they are an orthogonal restriction that shrinks the set of
/// permitted actions for the duration of a high-impact news release.
pub struct EventLoop<C: Clock> {
    consumer: MarketEventConsumer,
    book: OrderBook,
    monitor: BufferMonitor,
    stale_detector: StaleDataDetector,
    seq_detector: SequenceGapDetector,
    gate: DataQualityGate,
    event_windows: EventWindowManager,
    clock: C,
    events_processed: u64,
    tick_count: u64,
    running: Arc<AtomicBool>,
    /// Optional wiring into the unified circuit-breaker framework.
    /// Story 5.4 plumbs the buffer-occupancy and data-quality signals
    /// into [`CircuitBreakers`] when this is `Some`. When `None`, the
    /// legacy [`BufferMonitor`] / [`DataQualityGate`] paths still run
    /// untouched (used by the existing tests).
    breakers: Option<CircuitBreakers>,
    /// Captured guards for the breaker plumbing — currently just the
    /// max-spread threshold used by `OrderBook::is_tradeable()`.
    book_guards: Option<BookGuards>,
    /// Pre-Epic-6 cleanup D-2: shared `PositionTracker` driving the
    /// per-tick anomaly check. `None` when anomaly detection is not
    /// attached. Held as `Arc` because reads-only — the writer side is
    /// the engine's `OrderManager`.
    position_tracker: Option<Arc<PositionTracker>>,
    /// Pre-Epic-6 cleanup D-2: source of "expected" positions per symbol.
    expected_positions: Option<Box<dyn ExpectedPositionSource>>,
    /// Pre-Epic-6 cleanup D-2: producer side of the
    /// `tokio::sync::mpsc::Sender<FlattenRequest>` channel. The matching
    /// `Receiver` and the async `handle_anomaly` consumer task are owned
    /// by Story 8.2.
    flatten_tx: Option<mpsc::Sender<FlattenRequest>>,
    /// Pre-Epic-6 cleanup D-2: journal sender used to record
    /// `flatten_request_dropped` system events when the producer's
    /// `try_send` returns `Full`.
    anomaly_journal: Option<JournalSender>,
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
        Self::with_event_windows(consumer, clock, stale_threshold_secs, &[])
    }

    /// Construct with a populated event-window manager.
    ///
    /// `events` is the `events: Vec<EventWindowConfig>` field from
    /// `TradingConfig`. Pass `&[]` (or use [`Self::new`]) when no
    /// wall-clock event restrictions are configured.
    pub fn with_event_windows(
        consumer: MarketEventConsumer,
        clock: C,
        stale_threshold_secs: f64,
        events: &[EventWindowConfig],
    ) -> Self {
        Self {
            consumer,
            book: OrderBook::empty(),
            monitor: BufferMonitor::new(),
            stale_detector: StaleDataDetector::new(stale_threshold_secs),
            seq_detector: SequenceGapDetector::new(),
            gate: DataQualityGate::new(),
            event_windows: EventWindowManager::new(events),
            clock,
            events_processed: 0,
            tick_count: 0,
            running: Arc::new(AtomicBool::new(false)),
            breakers: None,
            book_guards: None,
            position_tracker: None,
            expected_positions: None,
            flatten_tx: None,
            anomaly_journal: None,
        }
    }

    /// Attach a [`CircuitBreakers`] instance so this event loop drives the
    /// unified breaker framework (story 5.4). When attached, every tick
    /// will:
    ///
    ///   * call `update_buffer_occupancy` with the consumer's current
    ///     length and capacity (Task 6.1)
    ///   * call `update_data_quality` after each order-book update with
    ///     the live `is_tradeable()` / gap / staleness flags (Task 6.2)
    ///
    /// `max_spread` is the [`FixedPrice`] threshold used by
    /// `OrderBook::is_tradeable()` — supplied here so the breaker plumbing
    /// has the production trading-config value without the event loop
    /// needing to own a full [`futures_bmad_core::TradingConfig`].
    pub fn attach_circuit_breakers(&mut self, breakers: CircuitBreakers, max_spread: FixedPrice) {
        self.breakers = Some(breakers);
        self.book_guards = Some(BookGuards { max_spread });
    }

    /// Pre-Epic-6 cleanup D-2: attach the per-tick anomaly-detection
    /// producer.
    ///
    /// On each [`Self::tick`], if all three seams are present the loop
    /// will:
    ///
    ///   1. Iterate every non-flat symbol in the attached
    ///      [`PositionTracker`] snapshot.
    ///   2. Look up the strategy-expected position via
    ///      [`ExpectedPositionSource::expected_position`] (default impl
    ///      returns 0 ⇒ any non-flat position is anomalous).
    ///   3. Call [`CircuitBreakers::check_position_anomaly`]; on
    ///      `Anomalous`, build a [`FlattenRequest`] (side opposite of
    ///      the existing position, quantity = `|current_position|`) and
    ///      `try_send` it on `flatten_tx`.
    ///   4. On `try_send` returning `Full`, emit a `tracing::warn!` and
    ///      journal a [`SystemEventRecord`] with
    ///      `category = "flatten_request_dropped"`. Never panics, never
    ///      blocks.
    ///
    /// `journal` is the `JournalSender` used to record dropped flatten
    /// requests; pass the same handle the rest of the engine uses so the
    /// audit trail is unified.
    ///
    /// The matching `Receiver<FlattenRequest>` and the async
    /// `handle_anomaly` consumer task are NOT owned by this commit —
    /// Story 8.2 will install them on the broker runtime.
    pub fn attach_anomaly_detection(
        &mut self,
        tracker: Arc<PositionTracker>,
        expected: Box<dyn ExpectedPositionSource>,
        sender: mpsc::Sender<FlattenRequest>,
        journal: JournalSender,
    ) {
        debug_assert!(
            self.flatten_tx.is_none(),
            "attach_anomaly_detection called twice — prior wires would be silently dropped"
        );
        if expected.is_empty_default() {
            tracing::warn!(
                "attach_anomaly_detection: using EmptyExpectedPositions default — every held position will be flagged anomalous; intended for tests/Epic-6-seam only"
            );
        }
        self.position_tracker = Some(tracker);
        self.expected_positions = Some(expected);
        self.flatten_tx = Some(sender);
        self.anomaly_journal = Some(journal);
    }

    /// Read access to the wired-in circuit-breaker framework, when present.
    pub fn breakers(&self) -> Option<&CircuitBreakers> {
        self.breakers.as_ref()
    }

    /// Mutable read access. Tests / wiring code use this to manually
    /// trip / reset breakers during the lifetime of the event loop.
    pub fn breakers_mut(&mut self) -> Option<&mut CircuitBreakers> {
        self.breakers.as_mut()
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

        // Check buffer state (legacy monitor — retained for back-compat).
        let fill = self.consumer.fill_fraction();
        let state = self.monitor.update(fill);

        // Story 5.4 wiring (Task 6.1): drive the unified circuit-breaker
        // framework's buffer-occupancy check from the same SPSC stats.
        if let Some(breakers) = self.breakers.as_mut() {
            let used = self.consumer.available();
            let capacity = self.consumer.capacity();
            breakers.update_buffer_occupancy(used, capacity, self.clock.now());
        }

        // Pop and process event
        let mut event_processed_this_tick = false;
        let mut had_seq_gap = false;
        if let Some(event) = self.consumer.try_pop() {
            event_processed_this_tick = true;
            let _now = event.timestamp.as_nanos();

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
                had_seq_gap = true;
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

        // Story 5.4 wiring (Task 6.2): after the order-book update,
        // refresh the data-quality gate from the live book + stale/gap
        // signals. Only run when an event was actually processed this
        // tick — otherwise we'd flap the gate based on a stale view of
        // the book.
        if event_processed_this_tick
            && let (Some(breakers), Some(guards)) = (self.breakers.as_mut(), self.book_guards)
        {
            let wall_now = self.clock.now().as_nanos();
            let market_open = self.clock.is_market_open();
            let is_stale = self
                .stale_detector
                .check_stale(wall_now, market_open)
                .is_some();
            let is_tradeable = self.book.is_tradeable(guards.max_spread);
            breakers.update_data_quality(is_tradeable, had_seq_gap, is_stale, self.clock.now());
        }

        // Pre-Epic-6 cleanup D-2: per-tick anomaly check producer. Runs
        // when all three seams are attached AND a CircuitBreakers
        // instance is wired in (anomaly state lives there).
        self.check_anomalies_and_publish();

        // Periodic stale check (every N iterations) to detect silence
        if self.tick_count.is_multiple_of(STALE_CHECK_INTERVAL) {
            let now = self.clock.now().as_nanos();
            let market_open = self.clock.is_market_open();
            if let Some(gap_nanos) = self.stale_detector.check_stale(now, market_open) {
                self.gate
                    .activate(&GateReason::StaleData { gap_nanos }, now);
                // Mirror into the breaker framework as well.
                if let (Some(breakers), Some(guards)) = (self.breakers.as_mut(), self.book_guards) {
                    let is_tradeable = self.book.is_tradeable(guards.max_spread);
                    let _ = gap_nanos; // surface; structure is logged by gate.activate
                    breakers.update_data_quality(is_tradeable, false, true, self.clock.now());
                }
            }
        }

        state
    }

    /// Pre-Epic-6 cleanup D-2: per-tick anomaly-detection producer
    /// implementation. Iterates the attached `PositionTracker` snapshot,
    /// calls `check_position_anomaly`, and `try_send`s `FlattenRequest`
    /// on trip. Full-channel case logs warn + journals
    /// `flatten_request_dropped`; never blocks or panics.
    ///
    /// No-op when any of the four seams (tracker / expected /
    /// flatten_tx / breakers) is missing — keeps existing tests that
    /// only attach `CircuitBreakers` from changing semantics.
    fn check_anomalies_and_publish(&mut self) {
        // All four wires must be present for the producer to run.
        let (Some(tracker), Some(expected), Some(sender), Some(breakers)) = (
            self.position_tracker.as_ref(),
            self.expected_positions.as_ref(),
            self.flatten_tx.as_ref(),
            self.breakers.as_mut(),
        ) else {
            return;
        };

        let now_ts = self.clock.now();

        // Snapshot iter: PositionTracker::iter yields (&u32, &Position)
        // for every tracked symbol (flat-and-pruned entries are already
        // dropped). We materialize symbol_id + signed-position pairs into
        // a Vec to release the borrow on `tracker` before the
        // `breakers.check_position_anomaly` mutable borrow.
        let snapshot: Vec<(u32, i32)> = tracker
            .iter()
            .filter_map(|(symbol_id, pos)| {
                let signed = signed_position(pos);
                if signed == 0 {
                    None
                } else {
                    Some((*symbol_id, signed))
                }
            })
            .collect();

        for (symbol_id, current_position) in snapshot {
            let strategy_expected = expected.expected_position(symbol_id);
            let outcome = breakers.check_position_anomaly(
                symbol_id,
                current_position,
                strategy_expected,
                now_ts,
            );
            if let AnomalyCheckOutcome::Anomalous {
                current_position: cur,
                ..
            } = outcome
            {
                // Build flatten request: side opposite of existing
                // position; quantity = |current_position|. Saturate
                // negative-i32 clamp before `unsigned_abs` is fine.
                let qty: u32 = cur.unsigned_abs();
                if qty == 0 {
                    // Defensive: anomaly with zero qty shouldn't happen
                    // but skip rather than emit a 0-qty flatten.
                    continue;
                }
                let flatten_side = if cur > 0 { Side::Sell } else { Side::Buy };
                let req = FlattenRequest {
                    // 0 is a sentinel meaning "consumer side allocates".
                    // Story 8.2's consumer is responsible for assigning a
                    // unique engine-side order_id before submission.
                    order_id: 0,
                    symbol_id,
                    side: flatten_side,
                    quantity: qty,
                    decision_id: 0,
                    timestamp: now_ts,
                };
                match sender.try_send(req) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        warn!(
                            target: "event_loop",
                            symbol_id,
                            current_position = cur,
                            strategy_expected,
                            "FlattenRequest channel full — request dropped, journaling"
                        );
                        if let Some(journal) = self.anomaly_journal.as_ref() {
                            let rec = SystemEventRecord {
                                timestamp: now_ts,
                                category: "flatten_request_dropped".to_string(),
                                message: format!(
                                    "anomaly detected on symbol {symbol_id} \
                                     (current={cur}, expected={strategy_expected}) \
                                     but flatten channel full — request dropped"
                                ),
                            };
                            if !journal.send(JournalEvent::SystemEvent(rec)) {
                                tracing::error!(
                                    target = "audit_loss",
                                    category = "flatten_request_dropped",
                                    "journal queue full — audit record lost"
                                );
                            }
                        }
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        warn!(
                            target: "event_loop",
                            symbol_id,
                            "FlattenRequest channel closed — consumer task gone"
                        );
                        if let Some(journal) = self.anomaly_journal.as_ref() {
                            let rec = SystemEventRecord {
                                timestamp: now_ts,
                                category: "flatten_request_dropped".to_string(),
                                message: format!(
                                    "anomaly detected on symbol {symbol_id} \
                                     (current={cur}, expected={strategy_expected}) \
                                     but flatten channel closed — request dropped"
                                ),
                            };
                            if !journal.send(JournalEvent::SystemEvent(rec)) {
                                tracing::error!(
                                    target = "audit_loss",
                                    category = "flatten_request_dropped",
                                    "journal queue full — audit record lost"
                                );
                            }
                        }
                        self.flatten_tx = None;
                        tracing::warn!(
                            "FlattenRequest channel closed — anomaly producer disabled until reattached"
                        );
                        break; // stop iterating positions; no consumer to receive any more
                    }
                }
            }
        }
    }

    /// Tick-time fee-staleness check. Story 5.4 task 6.3 says this should
    /// be invoked once per minute (not per tick) — callers schedule it
    /// from a timer thread or the periodic-task scheduler.
    pub fn check_fee_staleness(
        &mut self,
        fee_schedule_date: chrono::NaiveDate,
        current_date: chrono::NaiveDate,
        timestamp: UnixNanos,
    ) {
        if let Some(breakers) = self.breakers.as_mut() {
            breakers.check_fee_staleness(fee_schedule_date, current_date, timestamp);
        }
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

    /// Most-restrictive event-window restriction currently in effect, or
    /// `None` when no event window is active.
    ///
    /// Story 5.5 contract — called by the (future) signal-evaluation step
    /// AFTER the circuit-breaker `permits_trading` gate. Event windows do
    /// not trip breakers; they restrict what new trades are permitted
    /// while wall-clock-scheduled high-impact news releases are live.
    ///
    /// This DOES drive the manager's edge-triggered activation /
    /// resumption logging — call once per evaluation pass, not once per
    /// signal.
    pub fn current_trading_restriction(&mut self) -> Option<TradingRestriction> {
        self.event_windows.get_trading_restriction(&self.clock)
    }

    /// Number of configured event windows. `0` when no `[[events]]` array
    /// was present in the trading config.
    pub fn event_window_count(&self) -> usize {
        self.event_windows.len()
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
        let event_loop = EventLoop::new(consumer, clock, 3.0);
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

    // ----- story 5.5 — event-window integration -----

    use chrono::{NaiveDate, TimeZone, Utc};
    use futures_bmad_core::{EventAction, EventWindowConfig};

    fn fomc_window() -> EventWindowConfig {
        EventWindowConfig {
            name: "FOMC".into(),
            start: NaiveDate::from_ymd_opt(2026, 4, 16)
                .unwrap()
                .and_hms_opt(14, 0, 0)
                .unwrap(),
            end: None,
            duration_minutes: Some(120),
            action: EventAction::SitOut,
        }
    }

    /// Default `EventLoop::new` constructor uses an empty event-window manager.
    #[test]
    fn default_event_loop_has_no_event_windows() {
        let (_producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut event_loop = EventLoop::new(consumer, clock, 3.0);
        assert_eq!(event_loop.event_window_count(), 0);
        assert_eq!(event_loop.current_trading_restriction(), None);
    }

    /// `with_event_windows` accepts a config slice and counts loaded windows.
    #[test]
    fn with_event_windows_loads_configs() {
        let (_producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let event_loop = EventLoop::with_event_windows(consumer, clock, 3.0, &[fomc_window()]);
        assert_eq!(event_loop.event_window_count(), 1);
    }

    /// 6.1/6.2 — when an event window is active, `current_trading_restriction`
    /// surfaces the configured action so the (future) signal evaluator can
    /// short-circuit / reduce limits.
    #[test]
    fn restriction_surfaced_when_window_active() {
        let (_producer, consumer) = market_event_queue(16);
        let inside = Utc.with_ymd_and_hms(2026, 4, 16, 14, 30, 0).unwrap();
        let nanos = inside.timestamp_nanos_opt().unwrap() as u64;
        let clock = SimClock::new(nanos);

        let mut event_loop = EventLoop::with_event_windows(consumer, clock, 3.0, &[fomc_window()]);
        assert_eq!(
            event_loop.current_trading_restriction(),
            Some(TradingRestriction::SitOut)
        );
    }

    /// 6.3/6.4 — event windows are an orthogonal restriction layer; they
    /// neither emit data-quality gate events nor flip the buffer state.
    /// The event-loop tick path is unchanged when an event window is
    /// active — only the consumer-side `current_trading_restriction`
    /// query changes.
    #[test]
    fn active_window_does_not_affect_data_quality_or_buffer() {
        let (mut producer, consumer) = market_event_queue(16);
        let inside = Utc.with_ymd_and_hms(2026, 4, 16, 14, 30, 0).unwrap();
        let nanos = inside.timestamp_nanos_opt().unwrap() as u64;
        let clock = SimClock::new(nanos);

        let mut event_loop = EventLoop::with_event_windows(consumer, clock, 3.0, &[fomc_window()]);

        producer.try_push(make_bid(18000, 50, nanos));
        let state = event_loop.tick();
        assert_eq!(state, BufferState::Normal);
        assert!(event_loop.is_data_quality_ok());
        assert_eq!(
            event_loop.current_trading_restriction(),
            Some(TradingRestriction::SitOut)
        );
    }

    // -------------------------------------------------------------------
    // Story 5.4 — wiring tests for the unified circuit-breaker framework.
    //
    // These cover Task 6: each tick must drive
    // `update_buffer_occupancy` (Task 6.1) and `update_data_quality`
    // (Task 6.2) on the attached `CircuitBreakers`.
    // -------------------------------------------------------------------

    use crate::risk::CircuitBreakers;
    use crossbeam_channel::unbounded;
    use futures_bmad_core::{BreakerState, BreakerType, TradingConfig};

    fn breakers_for_test() -> CircuitBreakers {
        use crate::persistence::journal::EventJournal;
        use crate::risk::panic_mode::PanicMode;
        use std::sync::Arc;

        let cfg = TradingConfig {
            symbol: "ES".into(),
            max_position_size: 2,
            max_daily_loss_ticks: 1000,
            max_consecutive_losses: 3,
            max_trades_per_day: 30,
            edge_multiple_threshold: 1.5,
            session_start: "09:30".into(),
            session_end: "16:00".into(),
            max_spread_threshold: FixedPrice::new(4),
            fee_schedule_date: chrono::Utc::now().date_naive(),
            events: Vec::new(),
        };
        let (tx, _rx) = unbounded();
        let (ptx, _prx) = EventJournal::channel();
        let panic_mode = Arc::new(PanicMode::new(ptx));
        CircuitBreakers::new(&cfg, tx, panic_mode)
    }

    #[test]
    fn breakers_buffer_occupancy_drives_data_quality_gate_at_80_pct() {
        // Capacity 16 ⇒ 13 events = 81.25% which crosses the 80% gate but
        // stays below the 95% breaker.
        let (mut producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut ev = EventLoop::new(consumer, clock, 3.0);
        ev.attach_circuit_breakers(breakers_for_test(), FixedPrice::new(4));

        for i in 0..13 {
            producer.try_push(make_bid(18000 + i, 10, BASE_TS));
        }

        ev.tick();
        let breakers = ev.breakers().unwrap();
        assert_eq!(
            breakers.state(BreakerType::DataQuality),
            BreakerState::Tripped,
            "80% buffer should trip DataQuality gate"
        );
        assert_eq!(
            breakers.state(BreakerType::BufferOverflow),
            BreakerState::Active,
            "BufferOverflow should NOT trip below 95%"
        );
    }

    #[test]
    fn breakers_buffer_occupancy_drives_buffer_overflow_at_95_pct() {
        // Capacity 32 ⇒ 31 events = 96.875% (≥95%, also ≥80%).
        let (mut producer, consumer) = market_event_queue(32);
        let clock = SimClock::new(BASE_TS);
        let mut ev = EventLoop::new(consumer, clock, 3.0);
        ev.attach_circuit_breakers(breakers_for_test(), FixedPrice::new(4));

        for i in 0..31 {
            producer.try_push(make_bid(18000 + i, 10, BASE_TS));
        }

        ev.tick();
        let breakers = ev.breakers().unwrap();
        assert_eq!(
            breakers.state(BreakerType::BufferOverflow),
            BreakerState::Tripped
        );
        assert_eq!(
            breakers.state(BreakerType::DataQuality),
            BreakerState::Tripped,
            "95% also trips the 80% gate (both states must coexist)"
        );
    }

    #[test]
    fn breakers_check_fee_staleness_passthrough() {
        let (_producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut ev = EventLoop::new(consumer, clock, 3.0);
        ev.attach_circuit_breakers(breakers_for_test(), FixedPrice::new(4));

        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let stale = today - chrono::Duration::days(90);
        ev.check_fee_staleness(stale, today, UnixNanos::new(1));

        let breakers = ev.breakers().unwrap();
        assert_eq!(
            breakers.state(BreakerType::FeeStaleness),
            BreakerState::Tripped
        );
    }

    #[test]
    fn breakers_disabled_when_not_attached() {
        // Without `attach_circuit_breakers`, the loop runs as before with
        // no breaker side effects.
        let (mut producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut ev = EventLoop::new(consumer, clock, 3.0);

        for i in 0..13 {
            producer.try_push(make_bid(18000 + i, 10, BASE_TS));
        }
        ev.tick();
        assert!(ev.breakers().is_none());
    }

    // -------------------------------------------------------------------
    // Pre-Epic-6 cleanup D-2 — anomaly producer wiring tests.
    //
    // These cover the new `attach_anomaly_detection` seam: when all four
    // wires are present, a non-flat PositionTracker entry that diverges
    // from the strategy-expected position trips the AnomalousPosition
    // breaker AND publishes a FlattenRequest on the channel.
    // -------------------------------------------------------------------

    use crate::order_manager::tracker::PositionTracker;
    use crate::persistence::journal::{EngineEvent as TestJournalEvent, EventJournal};
    use futures_bmad_core::FillType;

    fn fill_buy(qty: u32) -> futures_bmad_core::FillEvent {
        futures_bmad_core::FillEvent {
            order_id: 1,
            fill_price: FixedPrice::new(18000),
            fill_size: qty,
            timestamp: UnixNanos::new(1),
            side: Side::Buy,
            decision_id: 1,
            fill_type: FillType::Full,
        }
    }

    /// D-2 acceptance: divergence between PositionTracker and
    /// ExpectedPositionSource trips the AnomalousPosition breaker AND
    /// publishes a FlattenRequest on the channel.
    #[test]
    fn anomaly_divergence_trips_and_publishes_flatten_request() {
        let (_producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut ev = EventLoop::new(consumer, clock, 3.0);
        ev.attach_circuit_breakers(breakers_for_test(), FixedPrice::new(4));

        // Build a PositionTracker holding a long-3 position on symbol 1.
        let (jtx, _jrx) = EventJournal::channel();
        let mut tracker = PositionTracker::new(jtx.clone());
        tracker.apply_fill(&fill_buy(3), 1);
        let tracker = Arc::new(tracker);

        // EmptyExpectedPositions: any non-flat position is anomalous.
        let expected: Box<dyn ExpectedPositionSource> = Box::new(EmptyExpectedPositions);
        let (flatten_tx, mut flatten_rx) = mpsc::channel::<FlattenRequest>(8);
        ev.attach_anomaly_detection(tracker, expected, flatten_tx, jtx);

        ev.tick();

        // Breaker tripped.
        let breakers = ev.breakers().unwrap();
        assert_eq!(
            breakers.state(BreakerType::AnomalousPosition),
            BreakerState::Tripped,
            "AnomalousPosition breaker must trip on divergence"
        );

        // FlattenRequest published.
        let req = flatten_rx
            .try_recv()
            .expect("flatten request must be on channel");
        assert_eq!(req.symbol_id, 1);
        assert_eq!(req.side, Side::Sell, "flatten side opposite of long");
        assert_eq!(req.quantity, 3);
    }

    /// D-2 negative: when tracker is consistent (flat AND expected flat),
    /// no FlattenRequest is published.
    #[test]
    fn anomaly_no_divergence_publishes_nothing() {
        let (_producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut ev = EventLoop::new(consumer, clock, 3.0);
        ev.attach_circuit_breakers(breakers_for_test(), FixedPrice::new(4));

        // Empty PositionTracker — no positions at all.
        let (jtx, _jrx) = EventJournal::channel();
        let tracker = Arc::new(PositionTracker::new(jtx.clone()));
        let expected: Box<dyn ExpectedPositionSource> = Box::new(EmptyExpectedPositions);
        let (flatten_tx, mut flatten_rx) = mpsc::channel::<FlattenRequest>(8);
        ev.attach_anomaly_detection(tracker, expected, flatten_tx, jtx);

        ev.tick();

        let breakers = ev.breakers().unwrap();
        assert_eq!(
            breakers.state(BreakerType::AnomalousPosition),
            BreakerState::Active
        );
        assert!(flatten_rx.try_recv().is_err());
    }

    /// D-2: when the strategy explicitly claims the position
    /// (`ExpectedPositionSource` matches the tracker), no anomaly trips.
    #[test]
    fn anomaly_strategy_claim_matches_tracker_no_trip() {
        struct MatchingExpected;
        impl ExpectedPositionSource for MatchingExpected {
            fn expected_position(&self, symbol_id: u32) -> i32 {
                if symbol_id == 1 { 3 } else { 0 }
            }
        }

        let (_producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut ev = EventLoop::new(consumer, clock, 3.0);
        ev.attach_circuit_breakers(breakers_for_test(), FixedPrice::new(4));

        let (jtx, _jrx) = EventJournal::channel();
        let mut tracker = PositionTracker::new(jtx.clone());
        tracker.apply_fill(&fill_buy(3), 1);
        let tracker = Arc::new(tracker);

        let expected: Box<dyn ExpectedPositionSource> = Box::new(MatchingExpected);
        let (flatten_tx, mut flatten_rx) = mpsc::channel::<FlattenRequest>(8);
        ev.attach_anomaly_detection(tracker, expected, flatten_tx, jtx);

        ev.tick();

        let breakers = ev.breakers().unwrap();
        assert_eq!(
            breakers.state(BreakerType::AnomalousPosition),
            BreakerState::Active,
            "strategy-claimed position must NOT trigger anomaly"
        );
        assert!(flatten_rx.try_recv().is_err());
    }

    /// D-2 acceptance: when the FlattenRequest channel is full, the
    /// producer logs warn and journals a `flatten_request_dropped`
    /// SystemEventRecord WITHOUT panicking or blocking.
    #[test]
    fn anomaly_full_channel_logs_and_journals_no_panic() {
        let (_producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut ev = EventLoop::new(consumer, clock, 3.0);
        ev.attach_circuit_breakers(breakers_for_test(), FixedPrice::new(4));

        let (jtx, jrx) = EventJournal::channel();
        let mut tracker = PositionTracker::new(jtx.clone());
        tracker.apply_fill(&fill_buy(3), 1);
        let tracker = Arc::new(tracker);

        let expected: Box<dyn ExpectedPositionSource> = Box::new(EmptyExpectedPositions);
        // Capacity 1 channel — fill it up first so the next try_send
        // returns Full.
        let (flatten_tx, _flatten_rx) = mpsc::channel::<FlattenRequest>(1);
        flatten_tx
            .try_send(FlattenRequest {
                order_id: 0,
                symbol_id: 99,
                side: Side::Buy,
                quantity: 1,
                decision_id: 0,
                timestamp: UnixNanos::new(0),
            })
            .expect("priming send must succeed");

        ev.attach_anomaly_detection(tracker, expected, flatten_tx, jtx);

        // Must not panic.
        ev.tick();

        // Drain the journal looking for the dropped marker.
        let mut found = false;
        for _ in 0..16 {
            match jrx.try_recv_for_test() {
                Some(TestJournalEvent::SystemEvent(rec))
                    if rec.category == "flatten_request_dropped" =>
                {
                    found = true;
                    break;
                }
                Some(_) => continue,
                None => break,
            }
        }
        assert!(
            found,
            "expected SystemEventRecord with category=flatten_request_dropped"
        );
    }

    /// Patch 1: `signed_position` saturates at `i32::MAX` rather than
    /// silently sign-flipping when a corrupt `Position::quantity = u32::MAX`
    /// is fed in.
    #[test]
    fn signed_position_saturates_at_i32_max_for_corrupt_quantity() {
        use futures_bmad_core::Position;

        let long = Position {
            symbol_id: 1,
            side: Some(Side::Buy),
            quantity: u32::MAX,
            avg_entry_price: FixedPrice::default(),
            unrealized_pnl: 0,
            realized_pnl: 0,
        };
        assert_eq!(
            signed_position(&long),
            i32::MAX,
            "long-side u32::MAX must saturate to i32::MAX, not negative wrap"
        );

        let short = Position {
            symbol_id: 1,
            side: Some(Side::Sell),
            quantity: u32::MAX,
            avg_entry_price: FixedPrice::default(),
            unrealized_pnl: 0,
            realized_pnl: 0,
        };
        assert_eq!(
            signed_position(&short),
            -i32::MAX,
            "short-side u32::MAX must saturate to -i32::MAX, not positive wrap"
        );
    }

    /// D-2: when none of the four anomaly wires are attached, the
    /// existing tick path is unchanged (no panic, no surprising
    /// AnomalousPosition trips).
    #[test]
    fn anomaly_unattached_no_op_path_preserved() {
        let (_producer, consumer) = market_event_queue(16);
        let clock = SimClock::new(BASE_TS);
        let mut ev = EventLoop::new(consumer, clock, 3.0);
        ev.attach_circuit_breakers(breakers_for_test(), FixedPrice::new(4));

        // No `attach_anomaly_detection` call.
        ev.tick();
        assert_eq!(
            ev.breakers().unwrap().state(BreakerType::AnomalousPosition),
            BreakerState::Active
        );
    }
}
