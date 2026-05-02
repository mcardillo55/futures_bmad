//! [`PaperTradingOrchestrator`] — Story 7.3.
//!
//! Wires together:
//!
//! * a [`futures_bmad_core::SystemClock`] (real wall-clock time, because
//!   data is live) — see Story 7.3 Task 4.1
//! * a [`futures_bmad_testkit::MockBrokerAdapter`] (no orders ever leave
//!   the host) — Task 2.2
//! * a [`super::MarketDataFeed`] (production: Rithmic TickerPlant; tests:
//!   in-memory `Vec`) — Task 3.1
//! * the same SPSC market / order / fill queues live trading uses
//! * the same in-process [`crate::replay::fill_sim::MockFillSimulator`]
//!   that the replay path uses — fill semantics MUST match between paper
//!   and replay so a green replay implies green paper for the same input
//!
//! No code-path branching beyond the trait-object adapter injection (Story
//! 7.3 AC). All downstream code (event loop, signals, regime, risk,
//! journal) is mode-agnostic.
//!
//! Story 7.4 will tag the journal entries this orchestrator produces with
//! a `paper` source label. The [`PaperTradingOrchestrator::attach_journal`]
//! seam is in place specifically so 7.4 can layer on tagging without
//! re-engineering this wiring.

use std::sync::Arc;

use futures_bmad_broker::{
    FillQueueConsumer, FillQueueProducer, OrderQueueConsumer, OrderQueueProducer,
    create_order_fill_queues,
};
use futures_bmad_core::{
    BrokerMode, Clock, OrderBook, OrderEvent, SystemClock, TradeSource, UnixNanos,
};
use futures_bmad_testkit::{MockBehavior, MockBrokerAdapter};
use tracing::{info, warn};

use crate::order_book::apply_market_event;
use crate::paper::data_feed::MarketDataFeed;
use crate::persistence::journal::{EngineEvent as JournalEvent, JournalSender, SystemEventRecord};
use crate::persistence::query::{JournalQuery, ReadinessReport};
use crate::replay::fill_sim::{FillModel, MockFillSimulator};
use crate::spsc::{
    MARKET_EVENT_QUEUE_CAPACITY, MarketEventConsumer, MarketEventProducer, market_event_queue,
};

/// Configuration for a paper-trading run (Story 7.3).
#[derive(Debug, Clone)]
pub struct PaperTradingConfig {
    /// Fill model used by the [`MockFillSimulator`]. V1 only ships
    /// [`FillModel::ImmediateAtMarket`] — every order fills in full at the
    /// current best bid (sells) / best ask (buys) of the engine's view of
    /// the live order book.
    pub fill_model: FillModel,
    /// Optional override for the engine→engine market-event SPSC capacity.
    /// `None` falls back to [`MARKET_EVENT_QUEUE_CAPACITY`].
    pub market_event_capacity: Option<usize>,
    /// Symbols to subscribe on the underlying [`MockBrokerAdapter`] at
    /// startup. Empty by default — production wiring adds the configured
    /// trading symbol via [`Self::with_subscriptions`].
    pub subscriptions: Vec<String>,
    /// Whether to log a "PAPER TRADING MODE" warn-level banner at startup
    /// (Story 7.3 Task 6.1). Defaults to `true`; tests that do not want
    /// the banner in their captured output can flip it off.
    pub emit_startup_banner: bool,
}

impl Default for PaperTradingConfig {
    fn default() -> Self {
        Self {
            fill_model: FillModel::ImmediateAtMarket,
            market_event_capacity: None,
            subscriptions: Vec::new(),
            emit_startup_banner: true,
        }
    }
}

impl PaperTradingConfig {
    /// Builder helper — list of symbols to subscribe at startup.
    pub fn with_subscriptions<I, S>(mut self, symbols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.subscriptions = symbols.into_iter().map(Into::into).collect();
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PaperTradingError {
    /// Subscription via the in-process [`MockBrokerAdapter`] failed.
    /// Currently can never happen (the mock always returns Ok) but the
    /// error variant is retained so the production wiring (which uses a
    /// real adapter for market data) can surface real failures here.
    #[error("subscription failed for {symbol}: {source}")]
    Subscription {
        symbol: String,
        #[source]
        source: futures_bmad_core::BrokerError,
    },
}

/// Summary returned by [`PaperTradingOrchestrator::pump_until_idle`].
///
/// Mirrors the shape of [`crate::replay::ReplaySummary`] but in wall-clock
/// units rather than recorded-data units (paper mode does not have a
/// "recorded duration" — wall time IS the time).
#[derive(Debug, Clone, Copy)]
pub struct PaperTradingSummary {
    /// Number of market events consumed from the SPSC queue.
    pub events_consumed: u64,
    /// Number of orders pulled off the engine→broker queue.
    pub orders_processed: u64,
    /// Number of fills synthesized and pushed onto the broker→engine queue.
    pub fills_emitted: u64,
    /// Number of fills the engine consumed from the broker→engine queue.
    pub fills_consumed: u64,
}

/// Paper-trading orchestrator. See module docs for the wiring overview.
pub struct PaperTradingOrchestrator<F: MarketDataFeed, C: Clock = SystemClock> {
    config: PaperTradingConfig,
    clock: Arc<C>,
    broker: MockBrokerAdapter,
    feed: F,

    // SPSC queues — same types live trading uses.
    market_producer: MarketEventProducer,
    market_consumer: MarketEventConsumer,
    /// Engine→broker order queue producer. Story 7.3 owns the producer so
    /// the same SPSC plumbing live trading uses is in place; the actual
    /// strategy / order-manager wiring that pushes onto this queue lives
    /// outside this module.
    order_producer: OrderQueueProducer,
    order_consumer: OrderQueueConsumer,
    fill_producer: FillQueueProducer,
    fill_consumer: FillQueueConsumer,

    fill_sim: MockFillSimulator,
    book: OrderBook,
    last_event_ts: Option<UnixNanos>,
    events_pushed: u64,
    events_consumed: u64,
    orders_processed: u64,
    fills_consumed: u64,
    journal: Option<JournalSender>,
}

impl<F: MarketDataFeed> PaperTradingOrchestrator<F, SystemClock> {
    /// Construct a paper-trading orchestrator with the production
    /// [`SystemClock`] (Story 7.3 Task 4.1).
    pub fn new(config: PaperTradingConfig, feed: F) -> Self {
        Self::with_clock(config, feed, Arc::new(SystemClock))
    }
}

impl<F: MarketDataFeed, C: Clock> PaperTradingOrchestrator<F, C> {
    /// Construct with an explicit clock — used by tests to inject a
    /// deterministic clock for assertions on time-derived state. Production
    /// callers go through [`Self::new`] which always picks
    /// [`SystemClock`].
    pub fn with_clock(config: PaperTradingConfig, feed: F, clock: Arc<C>) -> Self {
        // Story 7.4 — mock broker is constructed with the Paper source tag
        // so any wiring code that inspects `broker.source()` sees the right
        // value at a glance. Actual record stamping happens via the
        // JournalSender override applied in [`Self::attach_journal`].
        let broker = MockBrokerAdapter::with_source(MockBehavior::Fill, TradeSource::Paper);
        let capacity = config
            .market_event_capacity
            .unwrap_or(MARKET_EVENT_QUEUE_CAPACITY);
        let (market_producer, market_consumer) = market_event_queue(capacity);
        let (order_producer, order_consumer, fill_producer, fill_consumer) =
            create_order_fill_queues();
        let fill_sim = MockFillSimulator::new(config.fill_model);

        Self {
            config,
            clock,
            broker,
            feed,
            market_producer,
            market_consumer,
            order_producer,
            order_consumer,
            fill_producer,
            fill_consumer,
            fill_sim,
            book: OrderBook::empty(),
            last_event_ts: None,
            events_pushed: 0,
            events_consumed: 0,
            orders_processed: 0,
            fills_consumed: 0,
            journal: None,
        }
    }

    /// Attach a [`JournalSender`] so paper-trading runs are recorded into
    /// the same audit trail live trading uses (Story 7.3 Task 8.4).
    ///
    /// Story 7.4 — the supplied sender is re-tagged with
    /// [`TradeSource::Paper`] before being stored, so every trade /
    /// order-state record routed through this orchestrator (and any
    /// downstream component that received a clone of this sender) is
    /// stamped `source = "paper"` in the journal. Callers that want a
    /// non-paper tag (rare; only for crossover tests) should clone and
    /// override the source themselves before attaching.
    ///
    /// Optional: when no journal is attached the orchestrator still pumps
    /// events / orders / fills, but no SQLite persistence happens.
    pub fn attach_journal(&mut self, journal: JournalSender) {
        self.journal = Some(journal.with_source(TradeSource::Paper));
    }

    /// Hand a paper-tagged clone of the orchestrator's [`JournalSender`] to
    /// downstream components (order manager, bracket manager) so their own
    /// emitted records inherit the same source override. Returns `None` when
    /// no journal has been attached yet.
    pub fn journal_sender(&self) -> Option<JournalSender> {
        self.journal.clone()
    }

    /// Subscribe to every symbol listed in [`PaperTradingConfig::subscriptions`]
    /// via the in-process [`MockBrokerAdapter`]. Idempotent — calling twice is
    /// safe but redundant.
    ///
    /// Returns immediately on the first subscription error so callers can
    /// surface the failure (production live-data wiring uses this same
    /// path with a real adapter; tests use the in-process mock).
    pub async fn subscribe_all(&mut self) -> Result<(), PaperTradingError> {
        use futures_bmad_core::BrokerAdapter;
        for symbol in self.config.subscriptions.clone() {
            self.broker.subscribe(&symbol).await.map_err(|source| {
                PaperTradingError::Subscription {
                    symbol: symbol.clone(),
                    source,
                }
            })?;
        }
        Ok(())
    }

    /// Story 7.4 Task 6.3 — log the paper-trading readiness summary at the
    /// end of a session.
    ///
    /// `conn` is a (read-only) [`rusqlite::Connection`] open against the
    /// same journal database the orchestrator's [`JournalSender`] writes
    /// to. The orchestrator does not own this connection (that lives on the
    /// journal worker thread); callers pass one in once the journal worker
    /// has flushed.
    ///
    /// The summary is informational — the go/no-go decision is human-made,
    /// per Story 7.4 Task 6.4. A `SystemEvent` is also written to the
    /// journal so the audit trail records the final readiness snapshot at
    /// session boundary.
    pub fn emit_readiness_summary(
        &self,
        conn: &rusqlite::Connection,
    ) -> Result<ReadinessReport, crate::persistence::JournalError> {
        let report = JournalQuery::new(conn).paper_readiness_report()?;
        info!(
            target: "paper",
            summary = %report.fmt_one_line(),
            "paper trading readiness summary"
        );
        if let Some(journal) = self.journal.as_ref() {
            let rec = SystemEventRecord {
                timestamp: self.clock.now(),
                category: "paper_readiness_summary".to_string(),
                message: report.fmt_one_line(),
            };
            let _ = journal.send(JournalEvent::SystemEvent(rec));
        }
        Ok(report)
    }

    /// Story 7.3 Task 6.1 — emit the startup banner.
    ///
    /// Logs at `warn` level so the line is visible in default operator
    /// configurations. Also journals a [`SystemEventRecord`] (when a
    /// journal is attached) so the audit trail records the mode at the
    /// session boundary — Story 7.4 will rely on this to disambiguate
    /// paper-source events from live-source events.
    pub fn emit_startup_banner(&self) {
        if !self.config.emit_startup_banner {
            return;
        }
        warn!(
            target: "paper",
            mode = BrokerMode::Paper.as_str(),
            "Starting in PAPER TRADING mode — orders will NOT be sent to exchange"
        );
        info!(
            target: "paper",
            adapter = "MockBrokerAdapter",
            "BrokerAdapter: MockBrokerAdapter (paper)"
        );
        let now = self.clock.now();
        if let Some(journal) = self.journal.as_ref() {
            let rec = SystemEventRecord {
                timestamp: now,
                category: "paper_mode_start".to_string(),
                message: "paper trading mode active — orders routed to MockBrokerAdapter"
                    .to_string(),
            };
            let _ = journal.send(JournalEvent::SystemEvent(rec));
        }
    }

    /// Reference to the in-process [`MockBrokerAdapter`].
    pub fn broker(&self) -> &MockBrokerAdapter {
        &self.broker
    }

    /// Mutable access to the in-process [`MockBrokerAdapter`] — tests use
    /// this to inspect / drive submitted orders directly. Production
    /// callers should rely on the SPSC order queue.
    pub fn broker_mut(&mut self) -> &mut MockBrokerAdapter {
        &mut self.broker
    }

    pub fn config(&self) -> &PaperTradingConfig {
        &self.config
    }

    /// Reference to the engine's view of the order book — useful for tests
    /// that need to inspect book state mid-run.
    pub fn book(&self) -> &OrderBook {
        &self.book
    }

    /// Shared handle to the orchestrator's clock so other engine
    /// components (signals, regime, risk) read time from the same source.
    /// Always [`SystemClock`] in production.
    pub fn clock(&self) -> Arc<C> {
        self.clock.clone()
    }

    pub fn events_consumed(&self) -> u64 {
        self.events_consumed
    }

    pub fn orders_processed(&self) -> u64 {
        self.orders_processed
    }

    pub fn fills_emitted(&self) -> u64 {
        self.fill_sim.fills_emitted()
    }

    pub fn fills_consumed(&self) -> u64 {
        self.fills_consumed
    }

    /// Hand the engine-side market event consumer to a downstream owner
    /// (e.g. an [`crate::EventLoop`]) before calling [`Self::pump_until_idle`].
    /// Production callers wire the consumer into the event loop at startup;
    /// tests use this to peek at events directly.
    #[cfg(test)]
    pub fn market_consumer_mut(&mut self) -> &mut MarketEventConsumer {
        &mut self.market_consumer
    }

    /// Hand the engine→broker order producer to a downstream owner
    /// (the order manager). Tests use this directly to inject test orders.
    pub fn order_producer_mut(&mut self) -> &mut OrderQueueProducer {
        &mut self.order_producer
    }

    /// Hand the broker→engine fill consumer to a downstream owner. Tests
    /// use this to assert on the synthesized fills directly.
    pub fn fill_consumer_mut(&mut self) -> &mut FillQueueConsumer {
        &mut self.fill_consumer
    }

    /// Pump events until the data feed reports no more events AND every
    /// SPSC queue has been fully drained.
    ///
    /// Single-thread loop, mirroring the replay orchestrator:
    ///
    ///   1. Pull next event from the feed.
    ///   2. Push the event into the market-event SPSC.
    ///   3. Drain the consumer side, applying each event to the engine's
    ///      view of the order book (the same code path live trading uses).
    ///   4. Drain the order-queue (engine → broker), simulate a fill via
    ///      [`MockFillSimulator`], push the fill into the fill-queue.
    ///   5. Drain the fill-queue (broker → engine) into the fills counter.
    ///
    /// No `sleep()` between steps — events flow as fast as the host can
    /// process them. In production the data feed itself is the throttle:
    /// the underlying Rithmic stream blocks on the next available message.
    pub fn pump_until_idle(&mut self) -> PaperTradingSummary {
        info!(target: "paper", "paper trading pump started");

        while let Some(event) = self.feed.next_event() {
            self.last_event_ts = Some(event.timestamp);

            // Step 2 — push onto market SPSC.
            if !self.market_producer.try_push(event) {
                self.drain_market_consumer();
                if !self.market_producer.try_push(event) {
                    warn!(target: "paper", "SPSC full even after draining — skipping event");
                    continue;
                }
            }
            self.events_pushed += 1;

            // Step 3 — drain consumer side immediately so the engine view
            // of the book stays in lockstep with the producer.
            self.drain_market_consumer();

            // Steps 4+5 — pump orders→fills→engine.
            self.drain_orders_and_simulate_fills();
            self.drain_fills();
        }

        // Final drain after feed exhaustion.
        self.drain_market_consumer();
        self.drain_orders_and_simulate_fills();
        self.drain_fills();

        PaperTradingSummary {
            events_consumed: self.events_consumed,
            orders_processed: self.orders_processed,
            fills_emitted: self.fill_sim.fills_emitted(),
            fills_consumed: self.fills_consumed,
        }
    }

    /// Single-iteration pump variant — drives one round of "drain feed +
    /// pump SPSCs". Production callers running the engine event loop on
    /// the consumer side use this from their tick loop so the orchestrator
    /// does not own the entire blocking pump.
    pub fn tick(&mut self) {
        if let Some(event) = self.feed.next_event() {
            self.last_event_ts = Some(event.timestamp);
            if !self.market_producer.try_push(event) {
                self.drain_market_consumer();
                if !self.market_producer.try_push(event) {
                    warn!(target: "paper", "SPSC full even after draining — skipping event (tick)");
                }
            } else {
                self.events_pushed += 1;
            }
        }
        self.drain_market_consumer();
        self.drain_orders_and_simulate_fills();
        self.drain_fills();
    }

    fn drain_market_consumer(&mut self) {
        while let Some(ev) = self.market_consumer.try_pop() {
            apply_market_event(&mut self.book, &ev);
            self.events_consumed += 1;
        }
    }

    /// Pull every pending [`OrderEvent`] off the engine→broker SPSC and
    /// simulate a fill via [`MockFillSimulator`]. Public so production
    /// callers running their own outer loop can pump orders manually
    /// (and so tests can assert on fill output without competing with
    /// the orchestrator's own fill drain).
    pub fn drain_orders_and_simulate_fills(&mut self) {
        while let Some(order) = self.order_consumer.try_pop() {
            let order: OrderEvent = order;
            self.orders_processed += 1;
            self.fill_sim
                .simulate(&order, &self.book, &mut self.fill_producer);
        }
    }

    fn drain_fills(&mut self) {
        while self.fill_consumer.try_pop().is_some() {
            self.fills_consumed += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paper::data_feed::VecMarketDataFeed;
    use futures_bmad_core::{
        BrokerAdapter, FillType, FixedPrice, MarketEvent, MarketEventType, OrderType, Side,
    };

    fn bid(ts: u64, price: i64) -> MarketEvent {
        MarketEvent {
            timestamp: UnixNanos::new(ts),
            symbol_id: 0,
            sequence: 0,
            event_type: MarketEventType::BidUpdate,
            price: FixedPrice::new(price),
            size: 100,
            side: Some(Side::Buy),
        }
    }

    fn ask(ts: u64, price: i64) -> MarketEvent {
        MarketEvent {
            timestamp: UnixNanos::new(ts),
            symbol_id: 0,
            sequence: 0,
            event_type: MarketEventType::AskUpdate,
            price: FixedPrice::new(price),
            size: 100,
            side: Some(Side::Sell),
        }
    }

    /// 7.2 — paper mode wires `MockBrokerAdapter` (not `RithmicAdapter`).
    /// Construction never touches the network — verified by the simple
    /// fact that it succeeds with no env vars set.
    #[test]
    fn new_wires_mock_broker_adapter() {
        let feed = VecMarketDataFeed::new(vec![bid(1, 100)]);
        let orch = PaperTradingOrchestrator::new(PaperTradingConfig::default(), feed);
        // The broker is the in-process MockBrokerAdapter — order count is 0
        // and no network I/O occurred during construction.
        assert_eq!(orch.broker().order_count(), 0);
        assert_eq!(orch.events_consumed(), 0);
    }

    /// 7.3 — paper mode uses [`SystemClock`] (real time), NOT [`SimClock`].
    /// We assert the clock advances by reading it twice — `SystemClock`
    /// returns wall-clock nanos which are non-decreasing, while `SimClock`
    /// would return the exact same value across both reads.
    #[test]
    fn uses_system_clock_real_time() {
        let feed = VecMarketDataFeed::new(vec![bid(1, 100)]);
        let orch = PaperTradingOrchestrator::new(PaperTradingConfig::default(), feed);
        let clock = orch.clock();
        let t1 = clock.now().as_nanos();
        // Yield a moment to give wall-clock time a chance to move.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let t2 = clock.now().as_nanos();
        assert!(
            t2 > t1,
            "SystemClock must advance between reads (t1={t1} t2={t2})"
        );
        // SystemClock returns the current Unix time — much greater than
        // the in-memory test's recorded timestamp of 1.
        let jan_2020_nanos: u64 = 1_577_836_800_000_000_000;
        assert!(
            t1 > jan_2020_nanos,
            "SystemClock should report real wall time, got {t1}"
        );
    }

    /// 7.4 — orders pushed onto the order queue produce simulated fills via
    /// the same [`MockFillSimulator`] the replay path uses. Buy fills lift
    /// the best ask; sell fills hit the best bid.
    #[test]
    fn order_submitted_during_paper_pump_yields_fill() {
        let feed = VecMarketDataFeed::new(vec![bid(1, 18_000), ask(2, 18_004)]);
        let mut orch = PaperTradingOrchestrator::new(PaperTradingConfig::default(), feed);
        // First pump establishes the book.
        let _ = orch.pump_until_idle();
        assert_eq!(orch.book().best_bid().unwrap().price.raw(), 18_000);
        assert_eq!(orch.book().best_ask().unwrap().price.raw(), 18_004);

        // Submit an order via the same SPSC the order manager would use.
        let order = OrderEvent {
            order_id: 42,
            symbol_id: 0,
            side: Side::Buy,
            quantity: 3,
            order_type: OrderType::Market,
            decision_id: 7,
            timestamp: UnixNanos::new(3),
        };
        assert!(orch.order_producer_mut().try_push(order));
        // Pump the order through the fill simulator only — we don't call
        // the full `tick` because that drains the fill queue itself, which
        // would leave nothing for the test to inspect.
        orch.drain_orders_and_simulate_fills();

        let fill = orch.fill_consumer_mut().try_pop().unwrap();
        assert_eq!(fill.fill_price.raw(), 18_004, "buy lifts best ask");
        assert_eq!(fill.fill_size, 3);
        assert_eq!(fill.decision_id, 7, "decision_id propagates through fill");
        assert!(matches!(fill.fill_type, FillType::Full));
    }

    /// 6.1 — startup banner journals a `paper_mode_start` system event
    /// (when a journal is attached). The banner itself goes to tracing —
    /// asserted via the journal record so the test remains deterministic.
    #[test]
    fn startup_banner_journals_paper_mode_start() {
        let feed = VecMarketDataFeed::new(Vec::new());
        let mut orch = PaperTradingOrchestrator::new(PaperTradingConfig::default(), feed);
        let (sender, receiver) = crate::persistence::EventJournal::channel();
        orch.attach_journal(sender);
        orch.emit_startup_banner();

        let mut found = false;
        for _ in 0..16 {
            match receiver.try_recv_for_test() {
                Some(JournalEvent::SystemEvent(rec)) if rec.category == "paper_mode_start" => {
                    found = true;
                    break;
                }
                Some(_) => continue,
                None => break,
            }
        }
        assert!(found, "expected paper_mode_start system event on banner");
    }

    /// 6.1 — banner can be suppressed (test-only knob).
    #[test]
    fn startup_banner_suppressed_when_disabled() {
        let cfg = PaperTradingConfig {
            emit_startup_banner: false,
            ..PaperTradingConfig::default()
        };
        let feed = VecMarketDataFeed::new(Vec::new());
        let mut orch = PaperTradingOrchestrator::new(cfg, feed);
        let (sender, receiver) = crate::persistence::EventJournal::channel();
        orch.attach_journal(sender);
        orch.emit_startup_banner();
        assert!(receiver.try_recv_for_test().is_none());
    }

    /// 8.1 — paper-mode pump_until_idle drains every event from the feed
    /// and produces a non-zero events_consumed count.
    #[test]
    fn pump_drains_full_feed() {
        let feed = VecMarketDataFeed::new(vec![
            bid(1, 100),
            ask(2, 105),
            bid(3, 101),
            ask(4, 106),
            bid(5, 102),
        ]);
        let mut orch = PaperTradingOrchestrator::new(PaperTradingConfig::default(), feed);
        let summary = orch.pump_until_idle();
        assert_eq!(summary.events_consumed, 5);
        assert_eq!(summary.orders_processed, 0);
        assert_eq!(summary.fills_emitted, 0);
    }

    /// 2.4 — subscription via the configured symbol list reaches the in-
    /// process MockBrokerAdapter. The actual call uses the BrokerAdapter
    /// trait — the same surface live trading uses — so any future swap to
    /// a real adapter does not require changes here.
    #[tokio::test]
    async fn subscribe_all_routes_through_broker_trait() {
        let cfg = PaperTradingConfig::default()
            .with_subscriptions(["MES", "MNQ"])
            .clone();
        // .clone() on the builder return value to keep ownership semantics
        // intuitive in this test even though it isn't strictly necessary.
        let feed = VecMarketDataFeed::new(Vec::new());
        let mut orch = PaperTradingOrchestrator::new(cfg, feed);
        orch.subscribe_all().await.unwrap();
        assert!(orch.broker().was_subscribed("MES"));
        assert!(orch.broker().was_subscribed("MNQ"));
        assert!(!orch.broker().was_subscribed("ES"));
    }

    /// 5.4 — every other [`BrokerAdapter`] surface (cancel / query) works
    /// against the in-process mock without contacting any network. This
    /// guards against a regression where paper mode tries to hit the live
    /// broker for a side-channel query.
    #[tokio::test]
    async fn broker_query_surface_works_without_network() {
        let feed = VecMarketDataFeed::new(Vec::new());
        let mut orch = PaperTradingOrchestrator::new(PaperTradingConfig::default(), feed);
        let broker = orch.broker_mut();
        broker.cancel_order(1).await.unwrap();
        let positions = broker.query_positions().await.unwrap();
        assert!(positions.is_empty());
        let open = broker.query_open_orders().await.unwrap();
        assert!(open.is_empty());
    }
}
