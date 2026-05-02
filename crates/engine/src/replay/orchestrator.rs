//! [`ReplayOrchestrator`] — Story 7.1.
//!
//! Owns the replay-side wiring of:
//!
//! * a [`futures_bmad_testkit::SimClock`] (deterministic, advanced to each
//!   recorded `MarketEvent::timestamp`)
//! * a [`futures_bmad_testkit::MockBrokerAdapter`] (no network)
//! * a [`super::ParquetReplaySource`] (own-format Parquet files)
//! * the same SPSC queues live trading uses
//!   ([`crate::spsc::market_event_queue`] and
//!   [`futures_bmad_broker::create_order_fill_queues`])
//! * the same engine [`crate::EventLoop`] (no replay-specific branching)
//! * the same [`crate::persistence::EventJournal`] (the journal does not
//!   know it is recording replay data)
//!
//! After `run()` returns, the caller has a [`ReplaySummary`] containing the
//! event/trade counts, P&L, win rate, max drawdown, and replay speed
//! multiple. The journal contains every event written during the run.

use std::path::PathBuf;
use std::time::Instant;

use futures_bmad_broker::{
    FillQueueConsumer, FillQueueProducer, OrderQueueConsumer, OrderQueueProducer,
    create_order_fill_queues,
};
use futures_bmad_core::{
    Bar, Clock, FillType, FixedPrice, MarketEvent, MarketEventType, OrderBook, OrderEvent,
    RegimeDetector, RegimeState, SignalSnapshot, TradeSource, UnixNanos,
};
use futures_bmad_testkit::{MockBehavior, MockBrokerAdapter, SimClock};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::data::data_source::DataSource;
use crate::order_book::apply_market_event;
use crate::persistence::journal::{EngineEvent as JournalEvent, JournalSender, SystemEventRecord};
use crate::regime::ThresholdRegimeConfig;
use crate::regime::ThresholdRegimeDetector;
use crate::replay::data_source::{ParquetReplaySource, ReplaySourceError};
use crate::replay::determinism::{
    DEFAULT_SIGNAL_EPSILON, DeterminismReport, FixedPriceMismatch, RegimeMismatch, SignalMismatch,
    SnapshotMismatch,
};
use crate::replay::fill_sim::{FillModel, MockFillSimulator};
use crate::signals::SignalPipeline;
use crate::spsc::{
    MARKET_EVENT_QUEUE_CAPACITY, MarketEventConsumer, MarketEventProducer, market_event_queue,
};

/// Configuration for a replay run (Story 7.1 Task 1.3, extended Story 7.2).
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    /// Single `.parquet` file or directory of date-partitioned files.
    pub parquet_path: PathBuf,
    /// Fill model used by the [`MockFillSimulator`]. V1: only
    /// [`FillModel::ImmediateAtMarket`].
    pub fill_model: FillModel,
    /// Whether the orchestrator should log a [`ReplaySummary`] at completion.
    /// `run()` returns the summary regardless; this only controls the
    /// info-level log line.
    pub summary_output: bool,
    /// Optional SPSC market-event queue capacity override. Defaults to
    /// [`MARKET_EVENT_QUEUE_CAPACITY`].
    pub market_event_capacity: Option<usize>,
    /// Story 7.2 Task 4.1 — number of events between [`Signal::snapshot`]
    /// captures during [`ReplayOrchestrator::run_with_capture`]. `None`
    /// disables snapshot capture; `Some(N)` snapshots every Nth event
    /// (`N >= 1`).
    pub snapshot_interval: Option<u64>,
    /// Story 7.2 Task 2 — signal-pipeline construction parameters used by
    /// [`ReplayOrchestrator::run_with_capture`]. The standard `run()` path
    /// does not instrument signals; only the capture path does.
    pub signal_instrumentation: SignalInstrumentationConfig,
    /// Story 7.2 Task 2 — regime-detector construction parameters. `None`
    /// disables regime instrumentation; `Some(...)` builds a
    /// [`ThresholdRegimeDetector`] and feeds it bars synthesized from the
    /// replayed trade events.
    pub regime_instrumentation: Option<RegimeInstrumentationConfig>,
}

/// Construction parameters for the [`SignalPipeline`] instantiated by
/// [`ReplayOrchestrator::run_with_capture`].
///
/// The pipeline is rebuilt fresh on every `run_with_capture` call so each
/// run starts from identical initial state (Story 7.2 AC: signal reset()
/// semantics). All units mirror the pipeline constructor:
/// [`SignalPipeline::new`].
#[derive(Debug, Clone)]
pub struct SignalInstrumentationConfig {
    pub obi_warmup: u64,
    pub max_spread_qt: i64,
    pub vpin_bucket_size: u64,
    pub vpin_num_buckets: usize,
}

impl Default for SignalInstrumentationConfig {
    fn default() -> Self {
        Self {
            obi_warmup: 10,
            // 16 quarter-ticks = 4 ticks; well above typical futures spreads.
            max_spread_qt: 16,
            vpin_bucket_size: 100,
            vpin_num_buckets: 10,
        }
    }
}

/// Configuration for the regime detector used by
/// [`ReplayOrchestrator::run_with_capture`].
///
/// `bar_interval_nanos` controls how the orchestrator partitions trade
/// events into bars before forwarding them to the detector — the
/// orchestrator does not consume external bar streams in V1.
#[derive(Debug, Clone)]
pub struct RegimeInstrumentationConfig {
    pub detector: ThresholdRegimeConfig,
    /// Bar period in nanoseconds (e.g. 60_000_000_000 = 1 minute).
    pub bar_interval_nanos: u64,
}

impl Default for RegimeInstrumentationConfig {
    fn default() -> Self {
        Self {
            detector: ThresholdRegimeConfig::default(),
            bar_interval_nanos: 60_000_000_000,
        }
    }
}

impl ReplayConfig {
    pub fn new(parquet_path: PathBuf) -> Self {
        Self {
            parquet_path,
            fill_model: FillModel::ImmediateAtMarket,
            summary_output: true,
            market_event_capacity: None,
            snapshot_interval: None,
            signal_instrumentation: SignalInstrumentationConfig::default(),
            regime_instrumentation: None,
        }
    }
}

/// Serializable mirror of [`SignalSnapshot`] used in [`ReplayResult`] (Story
/// 7.2). The core [`SignalSnapshot`] uses `&'static str` for `name` which
/// would force ownership tricks on serde; here we hold a `String` instead so
/// snapshots round-trip cleanly through `insta`/JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignalSnapshotSerde {
    pub name: String,
    pub value: Option<f64>,
    pub valid: bool,
    pub timestamp_nanos: u64,
}

impl From<&SignalSnapshot> for SignalSnapshotSerde {
    fn from(s: &SignalSnapshot) -> Self {
        Self {
            name: s.name.to_string(),
            value: s.value,
            valid: s.valid,
            timestamp_nanos: s.timestamp.as_nanos(),
        }
    }
}

/// Serializable triple of pipeline snapshots — one per signal in the
/// pipeline (`obi`, `vpin`, `microprice`). Matches [`SignalPipeline`] order
/// so reviewers reading a captured snapshot file know which row is which.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PipelineSnapshotSerde {
    pub obi: SignalSnapshotSerde,
    pub vpin: SignalSnapshotSerde,
    pub microprice: SignalSnapshotSerde,
}

/// Story 7.2 — the complete capture set produced by
/// [`ReplayOrchestrator::run_with_capture`].
///
/// All `FixedPrice`-derived values are stored as raw quarter-tick `i64` so
/// Tier 1 comparisons remain bit-identical without depending on the
/// (lossy, f64-based) [`FixedPrice`] serde representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplayResult {
    /// Number of recorded events processed end-to-end.
    pub total_events: u64,
    /// Number of round-trip trades (one per fully-closed leg).
    pub total_trades: u64,
    /// Net P&L in quarter-ticks (Tier 1 — bit-identical).
    pub net_pnl_qt: i64,
    /// Per-trade P&L sequence in quarter-ticks (Tier 1 — bit-identical).
    pub trade_pnls_qt: Vec<i64>,
    /// Signal values captured every event for the OBI signal (Tier 2).
    pub obi_values: Vec<Option<f64>>,
    /// Signal values captured every event for the VPIN signal (Tier 2).
    pub vpin_values: Vec<Option<f64>>,
    /// Signal values captured every event for the microprice signal (Tier 2).
    pub microprice_values: Vec<Option<f64>>,
    /// Regime state after each completed bar (Tier 3).
    pub regime_states: Vec<RegimeState>,
    /// Periodic pipeline snapshots — present iff
    /// [`ReplayConfig::snapshot_interval`] was set.
    pub snapshots: Vec<PipelineSnapshotSerde>,
}

impl ReplayResult {
    /// Apply the three-tier comparison and return a [`DeterminismReport`].
    ///
    /// * Tier 1 — `total_trades`, `net_pnl_qt`, `trade_pnls_qt` element-wise.
    /// * Tier 2 — `obi_values`, `vpin_values`, `microprice_values` (epsilon
    ///   = [`DEFAULT_SIGNAL_EPSILON`], NaN-aware).
    /// * Tier 3 — `regime_states` element-wise variant equality.
    /// * Snapshots — if both runs captured snapshots, fields are compared
    ///   per-tier (`value` => Tier 2, `valid`/`timestamp` => exact).
    pub fn compare(&self, other: &ReplayResult) -> DeterminismReport {
        self.compare_with_epsilon(other, DEFAULT_SIGNAL_EPSILON)
    }

    /// Variant of [`Self::compare`] that takes an explicit epsilon. Useful
    /// in tests asserting that a tightened/relaxed tolerance still detects
    /// the expected mismatches.
    pub fn compare_with_epsilon(&self, other: &ReplayResult, epsilon: f64) -> DeterminismReport {
        let mut report = DeterminismReport::default();

        // Length parity is itself a determinism violation — record it as a
        // Tier 1 mismatch using sentinel index `usize::MAX` so reviewers see
        // the structural drift even when individual indices line up.
        if self.total_trades != other.total_trades {
            report.tier1_mismatches.push(FixedPriceMismatch {
                index: usize::MAX,
                expected_raw: self.total_trades as i64,
                actual_raw: other.total_trades as i64,
            });
        }
        if self.net_pnl_qt != other.net_pnl_qt {
            report.tier1_mismatches.push(FixedPriceMismatch {
                index: usize::MAX - 1,
                expected_raw: self.net_pnl_qt,
                actual_raw: other.net_pnl_qt,
            });
        }
        let n_trades = self.trade_pnls_qt.len().min(other.trade_pnls_qt.len());
        for i in 0..n_trades {
            if self.trade_pnls_qt[i] != other.trade_pnls_qt[i] {
                report.tier1_mismatches.push(FixedPriceMismatch {
                    index: i,
                    expected_raw: self.trade_pnls_qt[i],
                    actual_raw: other.trade_pnls_qt[i],
                });
            }
        }

        compare_signal_series(&self.obi_values, &other.obi_values, epsilon, &mut report);
        compare_signal_series(&self.vpin_values, &other.vpin_values, epsilon, &mut report);
        compare_signal_series(
            &self.microprice_values,
            &other.microprice_values,
            epsilon,
            &mut report,
        );

        let n_regime = self.regime_states.len().min(other.regime_states.len());
        for i in 0..n_regime {
            if self.regime_states[i] != other.regime_states[i] {
                report.tier3_mismatches.push(RegimeMismatch {
                    index: i,
                    expected: self.regime_states[i],
                    actual: other.regime_states[i],
                });
            }
        }

        let n_snap = self.snapshots.len().min(other.snapshots.len());
        for i in 0..n_snap {
            compare_snapshot(
                &self.snapshots[i],
                &other.snapshots[i],
                i,
                epsilon,
                &mut report,
            );
        }

        report.finalize();
        report
    }
}

fn compare_signal_series(
    a: &[Option<f64>],
    b: &[Option<f64>],
    epsilon: f64,
    report: &mut DeterminismReport,
) {
    let n = a.len().min(b.len());
    for i in 0..n {
        match (a[i], b[i]) {
            (None, None) => {}
            (Some(x), Some(y)) => {
                let abs_diff = (x - y).abs();
                let nan_either = x.is_nan() || y.is_nan();
                if nan_either || abs_diff > epsilon {
                    report.tier2_mismatches.push(SignalMismatch {
                        index: i,
                        expected: x,
                        actual: y,
                        abs_diff,
                    });
                }
            }
            (Some(x), None) => {
                report.tier2_mismatches.push(SignalMismatch {
                    index: i,
                    expected: x,
                    actual: f64::NAN,
                    abs_diff: f64::INFINITY,
                });
            }
            (None, Some(y)) => {
                report.tier2_mismatches.push(SignalMismatch {
                    index: i,
                    expected: f64::NAN,
                    actual: y,
                    abs_diff: f64::INFINITY,
                });
            }
        }
    }
}

fn compare_snapshot(
    a: &PipelineSnapshotSerde,
    b: &PipelineSnapshotSerde,
    snapshot_index: usize,
    epsilon: f64,
    report: &mut DeterminismReport,
) {
    for (sig_a, sig_b) in [
        (&a.obi, &b.obi),
        (&a.vpin, &b.vpin),
        (&a.microprice, &b.microprice),
    ] {
        if sig_a.name != sig_b.name {
            report.snapshot_mismatches.push(SnapshotMismatch {
                snapshot_index,
                signal: sig_a.name.clone(),
                field: "name".to_string(),
                detail: format!("a={a} vs b={b}", a = sig_a.name, b = sig_b.name),
            });
            continue;
        }
        if sig_a.valid != sig_b.valid {
            report.snapshot_mismatches.push(SnapshotMismatch {
                snapshot_index,
                signal: sig_a.name.clone(),
                field: "valid".to_string(),
                detail: format!("a={} vs b={}", sig_a.valid, sig_b.valid),
            });
        }
        if sig_a.timestamp_nanos != sig_b.timestamp_nanos {
            report.snapshot_mismatches.push(SnapshotMismatch {
                snapshot_index,
                signal: sig_a.name.clone(),
                field: "timestamp".to_string(),
                detail: format!("a={} vs b={}", sig_a.timestamp_nanos, sig_b.timestamp_nanos),
            });
        }
        match (sig_a.value, sig_b.value) {
            (None, None) => {}
            (Some(x), Some(y)) => {
                let abs_diff = (x - y).abs();
                if x.is_nan() || y.is_nan() || abs_diff > epsilon {
                    report.snapshot_mismatches.push(SnapshotMismatch {
                        snapshot_index,
                        signal: sig_a.name.clone(),
                        field: "value".to_string(),
                        detail: format!("a={x} vs b={y} (|diff|={abs_diff:e})"),
                    });
                }
            }
            (Some(x), None) | (None, Some(x)) => {
                report.snapshot_mismatches.push(SnapshotMismatch {
                    snapshot_index,
                    signal: sig_a.name.clone(),
                    field: "value".to_string(),
                    detail: format!("a/b mismatch (one None, one Some({x}))"),
                });
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error(transparent)]
    DataSource(#[from] ReplaySourceError),
}

/// Replay completion summary (Story 7.1 Task 7.2).
///
/// All P&L values are quarter-tick raw integers (FixedPrice::raw()) — convert
/// at the display layer, not here. `win_rate_pct` is already a percentage in
/// `[0.0, 100.0]`. `replay_speed_multiple` is the ratio of recorded duration
/// to wall-clock duration; e.g. 7312.0 means "replayed 6.5 hours of data in
/// ~3.2 seconds".
#[derive(Debug, Clone, Copy)]
pub struct ReplaySummary {
    pub total_events: u64,
    pub total_trades: u64,
    pub net_pnl_qt: i64,
    pub win_rate_pct: f64,
    pub max_drawdown_qt: i64,
    pub elapsed_wall_clock_nanos: u128,
    pub recorded_duration_nanos: u64,
    pub replay_speed_multiple: f64,
}

impl ReplaySummary {
    fn fmt_one_line(&self) -> String {
        format!(
            "events={} trades={} net_pnl_qt={} win_rate={:.1}% max_dd_qt={} \
             wall_ms={} recorded_s={:.3} speed={:.1}x",
            self.total_events,
            self.total_trades,
            self.net_pnl_qt,
            self.win_rate_pct,
            self.max_drawdown_qt,
            self.elapsed_wall_clock_nanos / 1_000_000,
            self.recorded_duration_nanos as f64 / 1e9,
            self.replay_speed_multiple,
        )
    }
}

/// Trade outcome accumulator. Replay tracks every full-fill event and pairs
/// opposite-side fills into round-trip trades for win/loss accounting.
#[derive(Default)]
struct TradeStats {
    total_trades: u64,
    wins: u64,
    losses: u64,
    cumulative_pnl_qt: i64,
    peak_pnl_qt: i64,
    max_drawdown_qt: i64,
    /// FIFO queue of (side, qty, fill_price) for open positions.
    open_legs: Vec<OpenLeg>,
    /// Story 7.2 — per-trade P&L sequence in quarter-ticks. Captured during
    /// `run_with_capture` so [`ReplayResult::compare`] can apply Tier 1
    /// (bit-identical) verification element-wise.
    per_trade_pnls_qt: Vec<i64>,
}

#[derive(Clone, Copy)]
struct OpenLeg {
    side: futures_bmad_core::Side,
    qty: u32,
    price: FixedPrice,
}

impl TradeStats {
    /// Apply a fill in FIFO order: opposite-side fills close existing legs
    /// (computing per-trade P&L), same-side fills append a new leg.
    fn apply_fill(&mut self, fill: &futures_bmad_core::FillEvent) {
        if !matches!(fill.fill_type, FillType::Full | FillType::Partial { .. }) {
            return;
        }
        if fill.fill_size == 0 {
            return;
        }

        let mut remaining = fill.fill_size;
        // Drain legs of the opposite side first.
        while remaining > 0
            && self
                .open_legs
                .first()
                .map(|leg| leg.side != fill.side)
                .unwrap_or(false)
        {
            let leg = self.open_legs.first_mut().unwrap();
            let close_qty = remaining.min(leg.qty);
            // P&L: long leg => (exit - entry); short leg => (entry - exit).
            let leg_pnl_qt = match leg.side {
                futures_bmad_core::Side::Buy => fill.fill_price.raw() - leg.price.raw(),
                futures_bmad_core::Side::Sell => leg.price.raw() - fill.fill_price.raw(),
            } * close_qty as i64;
            self.cumulative_pnl_qt = self.cumulative_pnl_qt.saturating_add(leg_pnl_qt);
            self.total_trades += 1;
            self.per_trade_pnls_qt.push(leg_pnl_qt);
            if leg_pnl_qt > 0 {
                self.wins += 1;
            } else if leg_pnl_qt < 0 {
                self.losses += 1;
            }
            // Drawdown bookkeeping.
            self.peak_pnl_qt = self.peak_pnl_qt.max(self.cumulative_pnl_qt);
            let dd = self.peak_pnl_qt - self.cumulative_pnl_qt;
            self.max_drawdown_qt = self.max_drawdown_qt.max(dd);

            leg.qty -= close_qty;
            remaining -= close_qty;
            if leg.qty == 0 {
                self.open_legs.remove(0);
            }
        }
        // Anything left after closing opposing legs opens a new same-side leg.
        if remaining > 0 {
            self.open_legs.push(OpenLeg {
                side: fill.side,
                qty: remaining,
                price: fill.fill_price,
            });
        }
    }

    fn win_rate_pct(&self) -> f64 {
        let decided = self.wins + self.losses;
        if decided == 0 {
            0.0
        } else {
            (self.wins as f64 / decided as f64) * 100.0
        }
    }
}

/// Replay orchestrator. See module docs for the wiring overview.
pub struct ReplayOrchestrator {
    config: ReplayConfig,
    sim_clock: SimClock,
    broker: MockBrokerAdapter,
    source: ParquetReplaySource,

    // SPSC queues — same types live trading uses.
    market_producer: MarketEventProducer,
    market_consumer: MarketEventConsumer,
    /// Engine→broker order queue producer. Story 7.1's V1 wiring does not
    /// itself submit orders (Story 7.3 paper mode wires real strategy
    /// signal evaluation through here); we own the producer so the same
    /// SPSC plumbing live trading uses is in place.
    #[allow(dead_code)]
    order_producer: OrderQueueProducer,
    order_consumer: OrderQueueConsumer,
    fill_producer: FillQueueProducer,
    fill_consumer: FillQueueConsumer,

    fill_sim: MockFillSimulator,
    book: OrderBook,
    last_event_ts: Option<UnixNanos>,
    first_event_ts: Option<UnixNanos>,
    events_pushed: u64,
    events_consumed: u64,
    journal: Option<JournalSender>,
    trade_stats: TradeStats,

    // Story 7.2 — instrumented capture state. The pipeline + regime
    // detector are owned across `run_with_capture*` calls so the
    // `run_with_capture_no_reset` test path actually observes residual
    // state from a prior run. Lazily constructed on first capture call.
    pipeline: Option<SignalPipeline>,
    regime_detector: Option<ThresholdRegimeDetector>,
}

impl ReplayOrchestrator {
    /// Construct a new orchestrator from `config` (Story 7.1 Task 3.1).
    ///
    /// Opens the Parquet source, allocates SPSC queues, and wires the
    /// SimClock + MockBrokerAdapter. Returns immediately — call
    /// [`ReplayOrchestrator::run`] to drive the replay.
    pub fn new(config: ReplayConfig) -> Result<Self, ReplayError> {
        let source = ParquetReplaySource::open(&config.parquet_path)?;
        let sim_clock = SimClock::new(0);
        // For replay, the market is always considered open — the recorded
        // timestamps describe a live trading window, and is_market_open()
        // gates the stale-data detector which is irrelevant in replay.
        sim_clock.set_market_open(true);

        // Story 7.4 Task 2.3 — replay-tagged mock broker. Same rationale
        // as the paper orchestrator: actual journal stamping happens via
        // the JournalSender override in `attach_journal`, but the adapter
        // exposes its source so the wiring is self-documenting.
        let broker = MockBrokerAdapter::with_source(MockBehavior::Fill, TradeSource::Replay);
        let capacity = config
            .market_event_capacity
            .unwrap_or(MARKET_EVENT_QUEUE_CAPACITY);
        let (market_producer, market_consumer) = market_event_queue(capacity);
        let (order_producer, order_consumer, fill_producer, fill_consumer) =
            create_order_fill_queues();
        let fill_sim = MockFillSimulator::new(config.fill_model);

        Ok(Self {
            config,
            sim_clock,
            broker,
            source,
            market_producer,
            market_consumer,
            order_producer,
            order_consumer,
            fill_producer,
            fill_consumer,
            fill_sim,
            book: OrderBook::empty(),
            last_event_ts: None,
            first_event_ts: None,
            events_pushed: 0,
            events_consumed: 0,
            journal: None,
            trade_stats: TradeStats::default(),
            pipeline: None,
            regime_detector: None,
        })
    }

    /// Attach a [`JournalSender`] so the orchestrator (a) records system
    /// events as the replay progresses and (b) flows trade events into the
    /// same audit trail live trading uses (Story 7.1 Task 7.3).
    ///
    /// Story 7.4 — the supplied sender is re-tagged with
    /// [`TradeSource::Replay`] before being stored, so every trade /
    /// order-state record routed through this orchestrator is stamped
    /// `source = "replay"` in the journal.
    ///
    /// Optional: when no journal is attached the orchestrator still
    /// computes summary statistics, but no SQLite persistence happens.
    pub fn attach_journal(&mut self, journal: JournalSender) {
        self.journal = Some(journal.with_source(TradeSource::Replay));
    }

    /// Get a clone-able handle to the SimClock so other engine components
    /// (signals, regime detectors, risk checks) read time from the same
    /// source. Use this when constructing the [`crate::EventLoop`].
    pub fn clock(&self) -> &SimClock {
        &self.sim_clock
    }

    pub fn broker(&self) -> &MockBrokerAdapter {
        &self.broker
    }

    pub fn broker_mut(&mut self) -> &mut MockBrokerAdapter {
        &mut self.broker
    }

    pub fn config(&self) -> &ReplayConfig {
        &self.config
    }

    /// Reference to the engine's view of the current order book — useful
    /// for tests and for future stories that need to peek at book state
    /// during replay.
    pub fn book(&self) -> &OrderBook {
        &self.book
    }

    pub fn events_pushed(&self) -> u64 {
        self.events_pushed
    }

    pub fn events_consumed(&self) -> u64 {
        self.events_consumed
    }

    pub fn fills_emitted(&self) -> u64 {
        self.fill_sim.fills_emitted()
    }

    /// Hand the engine-side market event consumer to a downstream owner
    /// (e.g. an [`crate::EventLoop`]) BEFORE calling [`Self::run`]. Replay
    /// drives the event loop in-process during `run`, so production callers
    /// do not normally need this; tests use it to peek at events.
    #[cfg(test)]
    pub fn market_consumer_mut(&mut self) -> &mut MarketEventConsumer {
        &mut self.market_consumer
    }

    #[cfg(test)]
    pub fn order_producer_mut(&mut self) -> &mut OrderQueueProducer {
        &mut self.order_producer
    }

    #[cfg(test)]
    pub fn fill_consumer_mut(&mut self) -> &mut FillQueueConsumer {
        &mut self.fill_consumer
    }

    /// Run the replay end-to-end and return a [`ReplaySummary`].
    ///
    /// Single-thread loop:
    ///
    ///   1. Pull next event from the data source.
    ///   2. Advance [`SimClock`] to `event.timestamp` (with monotonicity
    ///      guard — non-monotonic timestamps are logged and skipped per
    ///      Task 4.2).
    ///   3. Push the event into the market-event SPSC.
    ///   4. Drain the consumer side, applying each event to the engine's
    ///      view of the order book (the same code path live trading uses).
    ///   5. Drain the order-queue (engine → broker), simulate a fill via
    ///      [`MockFillSimulator`], push the fill into the fill-queue.
    ///   6. Drain the fill-queue (broker → engine) into the trade
    ///      statistics aggregator.
    ///
    /// No `sleep()` between steps — events flow as fast as the host can
    /// process them (Task 4.3).
    pub fn run(&mut self) -> ReplaySummary {
        let started = Instant::now();
        info!(target: "replay", path = %self.config.parquet_path.display(), "replay started");
        self.journal_system_event(
            "replay_start",
            format!("replay started: {}", self.config.parquet_path.display()),
        );

        while let Some(event) = self.source.next_event() {
            // Monotonicity guard (Task 4.2): if the recorded timestamp is
            // earlier than the SimClock, log a warn and DO NOT advance the
            // clock backwards.
            let event_ts = event.timestamp;
            match self.last_event_ts {
                Some(last) if event_ts.as_nanos() < last.as_nanos() => {
                    warn!(
                        target: "replay",
                        prev = last.as_nanos(),
                        next = event_ts.as_nanos(),
                        "non-monotonic timestamp — skipping clock advancement"
                    );
                }
                _ => {
                    self.sim_clock.set_time(event_ts);
                    self.last_event_ts = Some(event_ts);
                    if self.first_event_ts.is_none() {
                        self.first_event_ts = Some(event_ts);
                    }
                }
            }

            // Step 3: market event SPSC.
            if !self.market_producer.try_push(event) {
                // Slow consumer relative to producer in this single-threaded
                // loop is impossible because we drain immediately below — a
                // failed push here would indicate the capacity is too small
                // for the bookkeeping we do per event. Drain and retry once.
                self.drain_market_consumer();
                if !self.market_producer.try_push(event) {
                    warn!(target: "replay", "SPSC full even after draining — skipping event");
                    continue;
                }
            }
            self.events_pushed += 1;

            // Step 4: drain consumer side immediately so the engine view of
            // the book stays in lockstep with the producer (this is what
            // gives the orchestrator a current book to fill against).
            self.drain_market_consumer();

            // Step 5 + 6: simulate fills for any pending orders and ingest
            // them into the trade aggregator.
            self.drain_orders_and_simulate_fills();
            self.drain_fills();
        }

        // Drain anything left in the queues (the source is exhausted but the
        // last few events may still be in-flight).
        self.drain_market_consumer();
        self.drain_orders_and_simulate_fills();
        self.drain_fills();

        let elapsed = started.elapsed();
        let recorded = match (self.first_event_ts, self.last_event_ts) {
            (Some(first), Some(last)) => last.as_nanos().saturating_sub(first.as_nanos()),
            _ => 0,
        };
        let elapsed_nanos = elapsed.as_nanos();
        let speed = if elapsed_nanos == 0 {
            0.0
        } else {
            recorded as f64 / elapsed_nanos as f64
        };

        let summary = ReplaySummary {
            total_events: self.events_consumed,
            total_trades: self.trade_stats.total_trades,
            net_pnl_qt: self.trade_stats.cumulative_pnl_qt,
            win_rate_pct: self.trade_stats.win_rate_pct(),
            max_drawdown_qt: self.trade_stats.max_drawdown_qt,
            elapsed_wall_clock_nanos: elapsed_nanos,
            recorded_duration_nanos: recorded,
            replay_speed_multiple: speed,
        };

        if self.config.summary_output {
            info!(
                target: "replay",
                summary = %summary.fmt_one_line(),
                "replay complete"
            );
        }
        self.journal_system_event("replay_complete", summary.fmt_one_line());
        // Task 4.4 — explicit speed-multiple log line, distinct from the
        // summary so operators reading just for "how fast did this run?"
        // don't have to parse the full summary.
        info!(
            target: "replay",
            wall_ms = elapsed_nanos / 1_000_000,
            recorded_s = recorded as f64 / 1e9,
            speed_x = speed,
            "replay speed"
        );

        summary
    }

    /// Story 7.2 — instrumented replay that captures all data needed for
    /// determinism comparison.
    ///
    /// Differs from [`Self::run`] in two ways:
    ///
    /// 1. Builds a fresh [`SignalPipeline`] (and optional
    ///    [`ThresholdRegimeDetector`]) on every call, then explicitly calls
    ///    [`SignalPipeline::reset`] before any events flow through. Per
    ///    Story 7.2 Task 3.1 this is mandatory, not optional — so the
    ///    method has no opt-out for the `reset` call. The pre-replay
    ///    invariant is asserted via [`SignalPipeline::snapshot`] against a
    ///    freshly-constructed reference pipeline.
    ///
    /// 2. Captures every signal output, every trade P&L, every regime
    ///    transition, and (optionally) periodic pipeline snapshots into a
    ///    [`ReplayResult`].
    ///
    /// The data source is rewound at the start of the call so back-to-back
    /// invocations on the same orchestrator produce comparable runs.
    pub fn run_with_capture(&mut self) -> ReplayResult {
        self.run_with_capture_internal(true)
    }

    /// Variant for verifying that `reset()` is necessary (Story 7.2 Task
    /// 3.3). This DOES NOT call [`SignalPipeline::reset`] before the run
    /// — leftover state from any previous `run_with_capture*` call
    /// persists. Production callers should always use
    /// [`Self::run_with_capture`].
    #[doc(hidden)]
    pub fn run_with_capture_no_reset(&mut self) -> ReplayResult {
        self.run_with_capture_internal(false)
    }

    fn run_with_capture_internal(&mut self, reset_signals: bool) -> ReplayResult {
        let started = Instant::now();
        info!(
            target: "replay",
            path = %self.config.parquet_path.display(),
            reset_signals,
            "replay (instrumented) started"
        );

        // Rewind the data source so successive runs see identical input.
        self.source.reset();

        // Reset orchestrator-side run-scoped state (we keep the journal /
        // broker / SimClock — those are owned across the orchestrator
        // lifetime; only per-run state is rewound).
        self.book = OrderBook::empty();
        self.last_event_ts = None;
        self.first_event_ts = None;
        self.events_pushed = 0;
        self.events_consumed = 0;
        self.trade_stats = TradeStats::default();
        self.sim_clock.set_time(UnixNanos::new(0));
        self.sim_clock.set_market_open(true);

        // Lazily build the signal pipeline + regime detector once (on the
        // first capture call) so subsequent calls observe their persisted
        // state — required by Story 7.2 Task 3.3 to demonstrate that
        // skipping `reset()` actually leaks state across runs.
        let sig_cfg = self.config.signal_instrumentation.clone();
        if self.pipeline.is_none() {
            self.pipeline = Some(SignalPipeline::new(
                sig_cfg.obi_warmup,
                FixedPrice::new(sig_cfg.max_spread_qt),
                sig_cfg.vpin_bucket_size,
                sig_cfg.vpin_num_buckets,
            ));
        }
        // Regime detector — persisted across capture runs so the no-reset
        // path actually leaks bar-buffer state. With reset, we rebuild it
        // (the trait has no reset method; rebuilding is the canonical
        // clean-slate operation).
        if reset_signals || self.regime_detector.is_none() {
            self.regime_detector = self
                .config
                .regime_instrumentation
                .as_ref()
                .map(|cfg| ThresholdRegimeDetector::new(cfg.detector.clone()));
        }
        let bar_interval_nanos = self
            .config
            .regime_instrumentation
            .as_ref()
            .map(|cfg| cfg.bar_interval_nanos)
            .unwrap_or(0);

        // Take the pipeline + detector out of `self` so the inner loop has
        // unfettered access to the rest of the orchestrator's mutable state.
        // We put them back in a `defer`-style block at the end. (`take()`
        // here is safe because we hold an exclusive `&mut self`.)
        let mut pipeline = self.pipeline.take().expect("pipeline lazily ensured");
        let mut regime_detector_opt = self.regime_detector.take();

        if reset_signals {
            // Story 7.2 Task 3.1 — mandatory pre-replay reset. Then assert
            // (Task 3.2) that the post-reset state matches a fresh
            // reference pipeline.
            pipeline.reset();
            assert_initial_pipeline_state(&pipeline, &sig_cfg);
        }

        let mut result = ReplayResult {
            total_events: 0,
            total_trades: 0,
            net_pnl_qt: 0,
            trade_pnls_qt: Vec::new(),
            obi_values: Vec::new(),
            vpin_values: Vec::new(),
            microprice_values: Vec::new(),
            regime_states: Vec::new(),
            snapshots: Vec::new(),
        };

        let mut bar_builder = BarBuilder::new(bar_interval_nanos);
        let snapshot_interval = self.config.snapshot_interval;
        let mut event_index: u64 = 0;
        let mut snapshots_captured: u64 = 0;

        while let Some(event) = self.source.next_event() {
            // Same monotonicity rules as run().
            let event_ts = event.timestamp;
            match self.last_event_ts {
                Some(last) if event_ts.as_nanos() < last.as_nanos() => {
                    warn!(
                        target: "replay",
                        prev = last.as_nanos(),
                        next = event_ts.as_nanos(),
                        "non-monotonic timestamp — skipping clock advancement (capture)"
                    );
                }
                _ => {
                    self.sim_clock.set_time(event_ts);
                    self.last_event_ts = Some(event_ts);
                    if self.first_event_ts.is_none() {
                        self.first_event_ts = Some(event_ts);
                    }
                }
            }

            // SPSC push + drain (same flow as run()).
            if !self.market_producer.try_push(event) {
                self.drain_market_consumer();
                if !self.market_producer.try_push(event) {
                    warn!(target: "replay", "SPSC full even after draining — skipping event (capture)");
                    continue;
                }
            }
            self.events_pushed += 1;
            self.drain_market_consumer();
            self.drain_orders_and_simulate_fills();
            self.drain_fills();

            // Update signals using the engine's view of the book + the just-
            // applied event (when it was a trade).
            let trade_view: Option<&MarketEvent> =
                if matches!(event.event_type, MarketEventType::Trade) {
                    Some(&event)
                } else {
                    None
                };
            let (obi, vpin, mp) = pipeline.update_all(&self.book, trade_view, &self.sim_clock);
            result.obi_values.push(obi);
            result.vpin_values.push(vpin);
            result.microprice_values.push(mp);

            // Regime: if a regime detector is configured AND a trade event
            // closed a bar, push the closed bar to the detector and capture
            // the resulting state (one entry per bar boundary, NOT per event).
            if let Some(detector) = regime_detector_opt.as_mut()
                && let Some(closed_bar) = bar_builder.consume(&event)
            {
                let state = detector.update(&closed_bar, &self.sim_clock);
                result.regime_states.push(state);
            }

            event_index += 1;
            if let Some(interval) = snapshot_interval
                && interval > 0
                && event_index.is_multiple_of(interval)
            {
                let snap = pipeline.snapshot();
                result.snapshots.push(PipelineSnapshotSerde {
                    obi: SignalSnapshotSerde::from(&snap.obi),
                    vpin: SignalSnapshotSerde::from(&snap.vpin),
                    microprice: SignalSnapshotSerde::from(&snap.microprice),
                });
                snapshots_captured += 1;
            }
        }

        // Drain any remaining queued items.
        self.drain_market_consumer();
        self.drain_orders_and_simulate_fills();
        self.drain_fills();

        // Flush the final partial bar to the regime detector so the result
        // covers every event without losing the tail.
        if let (Some(detector), Some(final_bar)) =
            (regime_detector_opt.as_mut(), bar_builder.flush())
        {
            let state = detector.update(&final_bar, &self.sim_clock);
            result.regime_states.push(state);
        }

        // Restore the pipeline + detector so a follow-up capture call sees
        // their post-run state (or the freshly reset state, depending on
        // which entry point was used).
        self.pipeline = Some(pipeline);
        self.regime_detector = regime_detector_opt;

        result.total_events = self.events_consumed;
        result.total_trades = self.trade_stats.total_trades;
        result.net_pnl_qt = self.trade_stats.cumulative_pnl_qt;
        result.trade_pnls_qt = std::mem::take(&mut self.trade_stats.per_trade_pnls_qt);

        let elapsed = started.elapsed();
        info!(
            target: "replay",
            events = result.total_events,
            trades = result.total_trades,
            net_pnl_qt = result.net_pnl_qt,
            snapshots = snapshots_captured,
            wall_ms = elapsed.as_millis() as u64,
            "replay (instrumented) complete (Task 4.4 — snapshot count logged)"
        );

        result
    }

    fn drain_market_consumer(&mut self) {
        while let Some(ev) = self.market_consumer.try_pop() {
            apply_market_event(&mut self.book, &ev);
            self.events_consumed += 1;
        }
    }

    fn drain_orders_and_simulate_fills(&mut self) {
        while let Some(order) = self.order_consumer.try_pop() {
            let order: OrderEvent = order;
            self.fill_sim
                .simulate(&order, &self.book, &mut self.fill_producer);
        }
    }

    fn drain_fills(&mut self) {
        while let Some(fill) = self.fill_consumer.try_pop() {
            self.trade_stats.apply_fill(&fill);
        }
    }

    fn journal_system_event(&self, category: &str, message: String) {
        let Some(journal) = self.journal.as_ref() else {
            return;
        };
        let rec = SystemEventRecord {
            timestamp: self.sim_clock.now(),
            category: category.to_string(),
            message,
        };
        let _ = journal.send(JournalEvent::SystemEvent(rec));
    }
}

/// Story 7.2 Task 3.2 — pre-replay state assertion.
///
/// After [`SignalPipeline::reset`] every signal must be at its
/// freshly-constructed state. We construct a reference pipeline with the
/// same params and compare snapshots field-by-field. Any divergence indicates
/// a `reset()` implementation that fails to clear all internal state — an
/// EPIC-7 determinism breaker that this assertion catches before any events
/// are processed.
fn assert_initial_pipeline_state(pipeline: &SignalPipeline, sig_cfg: &SignalInstrumentationConfig) {
    let reference = SignalPipeline::new(
        sig_cfg.obi_warmup,
        FixedPrice::new(sig_cfg.max_spread_qt),
        sig_cfg.vpin_bucket_size,
        sig_cfg.vpin_num_buckets,
    );
    let actual = pipeline.snapshot();
    let expected = reference.snapshot();
    assert_signal_initial(&actual.obi, &expected.obi, "obi");
    assert_signal_initial(&actual.vpin, &expected.vpin, "vpin");
    assert_signal_initial(&actual.microprice, &expected.microprice, "microprice");
}

fn assert_signal_initial(actual: &SignalSnapshot, expected: &SignalSnapshot, name: &str) {
    assert_eq!(
        actual.value, expected.value,
        "{name}: post-reset value should match a freshly-constructed signal"
    );
    assert_eq!(
        actual.valid, expected.valid,
        "{name}: post-reset valid flag should match a freshly-constructed signal"
    );
    assert_eq!(
        actual.timestamp.as_nanos(),
        expected.timestamp.as_nanos(),
        "{name}: post-reset timestamp should be at the initial UnixNanos value"
    );
}

/// Builds OHLCV bars from the [`MarketEvent`] stream — Story 7.2 Task 4.x.
///
/// Used only by [`ReplayOrchestrator::run_with_capture`] when regime
/// instrumentation is enabled. The orchestrator's V1 wiring does not
/// otherwise produce bars; this builder partitions trades into wall-clock
/// buckets using `bar_interval_nanos` and emits a completed [`Bar`] every
/// time a trade crosses into a new bucket.
struct BarBuilder {
    /// Interval in nanoseconds. `0` disables bar building (used when no
    /// regime instrumentation is configured — the call sites guard this
    /// already, but the type-level `0` short-circuit keeps the helper
    /// safely no-op in defensive paths).
    interval_nanos: u64,
    current: Option<BarInProgress>,
}

struct BarInProgress {
    bucket_start: u64,
    open: FixedPrice,
    high: FixedPrice,
    low: FixedPrice,
    close: FixedPrice,
    volume: u64,
    last_ts: UnixNanos,
}

impl BarBuilder {
    fn new(interval_nanos: u64) -> Self {
        Self {
            interval_nanos,
            current: None,
        }
    }

    /// Consume an event. Returns `Some(closed_bar)` if this event crossed
    /// into a new bar bucket and the previous bar is now finalised.
    fn consume(&mut self, event: &MarketEvent) -> Option<Bar> {
        if self.interval_nanos == 0 {
            return None;
        }
        // Only trade events feed the bar's open/high/low/close/volume; bid/ask
        // updates are ignored. This matches the behavior the regime detector
        // expects (OHLCV from trade prints, not from quote updates).
        if !matches!(event.event_type, MarketEventType::Trade) {
            return None;
        }

        let ts = event.timestamp.as_nanos();
        let bucket_start = ts - (ts % self.interval_nanos);
        match &mut self.current {
            None => {
                self.current = Some(BarInProgress {
                    bucket_start,
                    open: event.price,
                    high: event.price,
                    low: event.price,
                    close: event.price,
                    volume: event.size as u64,
                    last_ts: event.timestamp,
                });
                None
            }
            Some(in_progress) if in_progress.bucket_start == bucket_start => {
                if event.price.raw() > in_progress.high.raw() {
                    in_progress.high = event.price;
                }
                if event.price.raw() < in_progress.low.raw() {
                    in_progress.low = event.price;
                }
                in_progress.close = event.price;
                in_progress.volume = in_progress.volume.saturating_add(event.size as u64);
                in_progress.last_ts = event.timestamp;
                None
            }
            Some(in_progress) => {
                // New bucket — finalise the previous one and seed a fresh bar.
                let closed = Bar {
                    open: in_progress.open,
                    high: in_progress.high,
                    low: in_progress.low,
                    close: in_progress.close,
                    volume: in_progress.volume,
                    timestamp: in_progress.last_ts,
                };
                *in_progress = BarInProgress {
                    bucket_start,
                    open: event.price,
                    high: event.price,
                    low: event.price,
                    close: event.price,
                    volume: event.size as u64,
                    last_ts: event.timestamp,
                };
                Some(closed)
            }
        }
    }

    /// Flush the partial in-progress bar (if any) — called once at the end
    /// of [`ReplayOrchestrator::run_with_capture`] so the regime detector
    /// sees the final bar without waiting for an out-of-band trigger.
    fn flush(&mut self) -> Option<Bar> {
        let in_progress = self.current.take()?;
        Some(Bar {
            open: in_progress.open,
            high: in_progress.high,
            low: in_progress.low,
            close: in_progress.close,
            volume: in_progress.volume,
            timestamp: in_progress.last_ts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::parquet_writer::MarketDataWriter;
    use chrono::NaiveDate;
    use futures_bmad_core::{
        FillType, FixedPrice, MarketEvent, MarketEventType, OrderType, Side, UnixNanos,
    };

    fn write_test_file(
        dir: &std::path::Path,
        symbol: &str,
        date: NaiveDate,
        events: &[MarketEvent],
    ) -> PathBuf {
        let mut writer = MarketDataWriter::new(dir.to_path_buf(), symbol.to_string());
        for ev in events {
            writer.write_event(ev).unwrap();
        }
        writer.flush().unwrap();
        crate::persistence::parquet_writer::file_path_for(dir, symbol, date)
    }

    fn bid_event(ts: u64, price: i64) -> MarketEvent {
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

    fn ask_event(ts: u64, price: i64) -> MarketEvent {
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

    /// 3.1 — orchestrator wires SimClock + MockBrokerAdapter + ParquetReplaySource.
    #[test]
    fn new_wires_components_from_config() {
        let dir = tempfile::tempdir().unwrap();
        let ts0 = 1_776_470_400_000_000_000u64;
        let path = write_test_file(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
            &[bid_event(ts0, 18_000)],
        );

        let cfg = ReplayConfig::new(path);
        let orch = ReplayOrchestrator::new(cfg).unwrap();
        // SimClock starts at 0 — replay sets it on the first event.
        assert_eq!(orch.clock().now().as_nanos(), 0);
        // MockBrokerAdapter default behavior (Fill).
        assert_eq!(orch.broker().order_count(), 0);
    }

    /// 4.1 — running advances SimClock to event timestamps.
    /// 8.3 — clock advances monotonically based on event timestamps.
    #[test]
    fn run_advances_simclock_to_event_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let ts0 = 1_776_470_400_000_000_000u64;
        let path = write_test_file(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
            &[
                bid_event(ts0, 18_000),
                ask_event(ts0 + 1_000_000, 18_004),
                bid_event(ts0 + 2_000_000, 18_001),
            ],
        );

        let mut orch = ReplayOrchestrator::new(ReplayConfig::new(path)).unwrap();
        let summary = orch.run();
        // SimClock should now be at the LAST event's timestamp.
        assert_eq!(orch.clock().now().as_nanos(), ts0 + 2_000_000);
        assert_eq!(summary.total_events, 3);
        // The book reflects the last bid + ask.
        assert_eq!(orch.book().best_bid().unwrap().price.raw(), 18_001);
        assert_eq!(orch.book().best_ask().unwrap().price.raw(), 18_004);
    }

    /// 4.2 — non-monotonic timestamp logs a warning and does NOT advance
    /// SimClock backwards.
    #[test]
    fn non_monotonic_timestamp_does_not_advance_clock_backwards() {
        let dir = tempfile::tempdir().unwrap();
        let ts0 = 1_776_470_400_000_000_000u64;
        // First event at ts0, second event "earlier" at ts0 - 1ms (data
        // quality issue). The MarketDataWriter buckets by date; both are on
        // the same day so they end up in the same file. The writer logs a
        // non-monotonic warning but still persists both rows.
        let path = write_test_file(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
            &[
                bid_event(ts0 + 1_000_000, 18_000),
                bid_event(ts0, 18_001), // earlier — should NOT pull clock back
            ],
        );

        let mut orch = ReplayOrchestrator::new(ReplayConfig::new(path)).unwrap();
        let _ = orch.run();
        // Clock stayed at the higher (first) timestamp.
        assert_eq!(orch.clock().now().as_nanos(), ts0 + 1_000_000);
    }

    /// 5.1/5.2 — orders pushed onto the order queue produce immediate fills
    /// at the current best bid/ask, surfaced on the FillQueue.
    #[test]
    fn order_submitted_during_replay_yields_immediate_fill() {
        let dir = tempfile::tempdir().unwrap();
        let ts0 = 1_776_470_400_000_000_000u64;
        let path = write_test_file(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
            &[
                bid_event(ts0, 18_000),
                ask_event(ts0 + 1, 18_004),
                // A trade event so we have a third tick to advance through.
                MarketEvent {
                    timestamp: UnixNanos::new(ts0 + 2),
                    symbol_id: 0,
                    sequence: 0,
                    event_type: MarketEventType::Trade,
                    price: FixedPrice::new(18_002),
                    size: 1,
                    side: Some(Side::Buy),
                },
            ],
        );

        let mut orch = ReplayOrchestrator::new(ReplayConfig::new(path)).unwrap();
        // Pre-populate the book by hand so the order produces a fill before
        // we ever call `run` (the orchestrator's own run() loop also does
        // this, but we want to inspect the fill queue afterwards).
        let _ = orch.run();

        // Now push an order onto the engine→broker queue and re-pump the
        // simulator. We use `cfg(test)`-only accessors to drive each side
        // of the wiring.
        let order = OrderEvent {
            order_id: 1,
            symbol_id: 0,
            side: Side::Buy,
            quantity: 2,
            order_type: OrderType::Market,
            decision_id: 1,
            timestamp: UnixNanos::new(ts0 + 3),
        };
        assert!(orch.order_producer_mut().try_push(order));
        orch.drain_orders_and_simulate_fills();

        let fill = orch.fill_consumer_mut().try_pop().unwrap();
        // Buy → fills at the best ask price (18_004).
        assert_eq!(fill.fill_price.raw(), 18_004);
        assert_eq!(fill.fill_size, 2);
        assert!(matches!(fill.fill_type, FillType::Full));
    }

    /// 7.2 — completion summary is computed from fills observed during the
    /// run.
    #[test]
    fn summary_includes_speed_and_event_counts() {
        let dir = tempfile::tempdir().unwrap();
        let ts0 = 1_776_470_400_000_000_000u64;
        let path = write_test_file(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
            &[
                bid_event(ts0, 18_000),
                ask_event(ts0 + 1_000_000_000, 18_004),
                bid_event(ts0 + 2_000_000_000, 18_001),
            ],
        );

        let mut orch = ReplayOrchestrator::new(ReplayConfig::new(path)).unwrap();
        let summary = orch.run();
        assert_eq!(summary.total_events, 3);
        // No orders were submitted ⇒ no trades.
        assert_eq!(summary.total_trades, 0);
        assert_eq!(summary.net_pnl_qt, 0);
        // Recorded duration spans 2 seconds.
        assert_eq!(summary.recorded_duration_nanos, 2_000_000_000);
    }

    /// 2.4 — multi-file replay processes files in date order.
    #[test]
    fn multi_file_replay_chronological_order() {
        let dir = tempfile::tempdir().unwrap();
        let ts1 = 1_776_470_400_000_000_000u64;
        let ts2 = ts1 + 86_400_000_000_000;
        write_test_file(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
            &[bid_event(ts1, 18_000)],
        );
        write_test_file(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
            &[bid_event(ts2, 18_100)],
        );

        let symbol_dir = dir.path().join("market").join("MES");
        let mut orch = ReplayOrchestrator::new(ReplayConfig::new(symbol_dir.clone())).unwrap();
        let summary = orch.run();
        assert_eq!(summary.total_events, 2);
        // Last event is from day 2 ⇒ clock advanced through both.
        assert_eq!(orch.clock().now().as_nanos(), ts2);
    }

    /// 5.4 — MockBrokerAdapter implements the full BrokerAdapter trait for
    /// replay (subscribe = no-op, cancel = ack, query_positions = tracked
    /// state).
    #[tokio::test]
    async fn mock_broker_satisfies_full_trait_surface() {
        use futures_bmad_core::BrokerAdapter;

        let dir = tempfile::tempdir().unwrap();
        let ts0 = 1_776_470_400_000_000_000u64;
        let path = write_test_file(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
            &[bid_event(ts0, 18_000)],
        );

        let mut orch = ReplayOrchestrator::new(ReplayConfig::new(path)).unwrap();
        let broker = orch.broker_mut();

        // subscribe / cancel_order / query_positions all succeed without
        // contacting any network.
        broker.subscribe("MES").await.unwrap();
        broker.cancel_order(1).await.unwrap();
        let positions = broker.query_positions().await.unwrap();
        assert!(positions.is_empty());
        let open = broker.query_open_orders().await.unwrap();
        assert!(open.is_empty());
    }
}
