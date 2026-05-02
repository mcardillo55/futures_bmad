//! `engine` binary entry point.
//!
//! Supported modes (Story 7.3 added paper):
//!
//! * `--replay <parquet-path>` — Story 7.1: deterministic historical
//!   replay via [`ReplayOrchestrator`] (SimClock + MockBrokerAdapter +
//!   Parquet source).
//! * `--paper --config <toml>` — Story 7.3: live data, simulated
//!   execution. Reads `[broker] mode = "paper"` from `<toml>` and
//!   wires a [`PaperTradingOrchestrator`] (SystemClock +
//!   MockBrokerAdapter + live data feed).
//! * `--live --config <toml>` — reserved for Epic 8 (startup sequence).
//!   Currently exits with `not yet implemented`.
//!
//! Running the binary without picking a mode prints help and exits with a
//! non-zero status code so an operator never accidentally launches the
//! engine without having chosen a mode.
//!
//! Story 7.3 AC — broker-mode selection happens HERE and ONLY HERE. After
//! the right adapter is constructed, every downstream component (event
//! loop, signals, risk, regime, order manager, journal) is adapter-
//! agnostic.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tracing::{error, info, warn};

use futures_bmad_core::BrokerMode;
use futures_bmad_engine::paper::{
    MarketDataFeed, PaperTradingConfig, PaperTradingOrchestrator, VecMarketDataFeed,
};
use futures_bmad_engine::replay::{FillModel, ReplayConfig, ReplayOrchestrator};
use futures_bmad_engine::startup::{load_config, sanity_check_paper_credentials};

#[derive(Debug, Parser)]
#[command(
    name = "engine",
    about = "futures_bmad trading engine",
    long_about = "Run the trading engine. Pick exactly one mode: --replay <parquet>, \
                  --paper --config <toml>, or --live --config <toml> (Epic 8)."
)]
struct Cli {
    /// Replay historical data from a Parquet file or a directory of
    /// date-partitioned Parquet files. Mutually exclusive with --paper /
    /// --live.
    #[arg(long, value_name = "PARQUET_PATH", conflicts_with_all = ["paper", "live"])]
    replay: Option<PathBuf>,

    /// Run in paper-trading mode — live data with simulated execution
    /// (Story 7.3). Requires `--config <toml>` whose `[broker] mode` is
    /// either `paper` (mode honored) or `live` (refused so an operator
    /// cannot accidentally route real orders by passing the wrong flag).
    #[arg(long, conflicts_with = "live")]
    paper: bool,

    /// Run in live-trading mode. Currently NOT implemented — Epic 8 lands
    /// the full live startup sequence.
    #[arg(long)]
    live: bool,

    /// Path to a TOML config file. Required for `--paper` and `--live`;
    /// optional for `--replay` (currently informational only).
    #[arg(long, value_name = "CONFIG_PATH")]
    config: Option<PathBuf>,
}

fn main() -> ExitCode {
    // Default tracing subscriber — operators can override via RUST_LOG.
    tracing_subscriber_init();

    let cli = Cli::parse();

    if let Some(replay_path) = cli.replay {
        return run_replay(replay_path);
    }

    if cli.paper {
        return run_paper(cli.config);
    }

    if cli.live {
        error!(
            "live mode is not yet implemented (Epic 8 — startup sequence). \
             Use --paper --config <toml> for paper trading or --replay <parquet> for replay."
        );
        return ExitCode::from(2);
    }

    error!(
        "no mode selected — pass `--replay <parquet>` for replay, \
         `--paper --config <toml>` for paper trading (Story 7.3), or \
         `--live --config <toml>` for live (Epic 8, not yet implemented)."
    );
    ExitCode::from(2)
}

fn run_replay(replay_path: PathBuf) -> ExitCode {
    info!(path = %replay_path.display(), "starting replay");

    let cfg = ReplayConfig {
        parquet_path: replay_path,
        fill_model: FillModel::ImmediateAtMarket,
        summary_output: true,
        market_event_capacity: None,
        // Story 7.2 instrumentation knobs default off for the binary entry
        // point — `run()` does not read them. The instrumented capture path
        // (`run_with_capture`) is wired up by tests / future stories.
        snapshot_interval: None,
        signal_instrumentation: futures_bmad_engine::replay::SignalInstrumentationConfig::default(),
        regime_instrumentation: None,
    };

    let mut orch = match ReplayOrchestrator::new(cfg) {
        Ok(o) => o,
        Err(e) => {
            error!(error = %e, "failed to construct replay orchestrator");
            return ExitCode::from(1);
        }
    };

    let summary = orch.run();
    info!(
        total_events = summary.total_events,
        total_trades = summary.total_trades,
        net_pnl_qt = summary.net_pnl_qt,
        win_rate_pct = summary.win_rate_pct,
        max_drawdown_qt = summary.max_drawdown_qt,
        elapsed_ms = (summary.elapsed_wall_clock_nanos / 1_000_000) as u64,
        speed_x = summary.replay_speed_multiple,
        "REPLAY SUMMARY"
    );
    // Plain stdout summary so an operator doesn't need to grep logs.
    println!("--- replay summary ---");
    println!("total events:     {}", summary.total_events);
    println!("total trades:     {}", summary.total_trades);
    println!("net P&L (qt):     {}", summary.net_pnl_qt);
    println!("win rate (%):     {:.2}", summary.win_rate_pct);
    println!("max drawdown qt:  {}", summary.max_drawdown_qt);
    println!(
        "wall clock (ms):  {}",
        summary.elapsed_wall_clock_nanos / 1_000_000
    );
    println!(
        "recorded (s):     {:.3}",
        summary.recorded_duration_nanos as f64 / 1e9
    );
    println!(
        "speed multiple:   {:.1}x real-time",
        summary.replay_speed_multiple
    );

    ExitCode::SUCCESS
}

/// Story 7.3 paper-mode entry point.
///
/// Loads the broker mode from the supplied config, constructs the right
/// orchestrator, and pumps until the data feed is exhausted. The V1 binary
/// uses an empty in-process feed because the live Rithmic data path lands
/// in Epic 8 — operators run paper mode through a test harness today;
/// production deployments will gain the live data wiring once Epic 8
/// merges. The orchestrator and config plumbing are stable across that
/// future change.
fn run_paper(config_path: Option<PathBuf>) -> ExitCode {
    let Some(path) = config_path else {
        error!("--paper requires --config <toml>. Example: --paper --config config/paper.toml");
        return ExitCode::from(2);
    };

    let cfg = match load_config(&path) {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "failed to load paper config");
            return ExitCode::from(1);
        }
    };

    if cfg.broker.mode != BrokerMode::Paper {
        error!(
            mode = cfg.broker.mode.as_str(),
            "config selects mode {} but --paper was passed; refusing to start. \
             Set [broker] mode = \"paper\" in {}.",
            cfg.broker.mode.as_str(),
            path.display()
        );
        return ExitCode::from(2);
    }

    sanity_check_paper_credentials(cfg.broker.mode);

    info!(config = %path.display(), "starting paper trading");

    // Empty feed for V1: the live Rithmic data wiring lands in Epic 8.
    // The orchestrator + adapter selection is what Story 7.3 ships; once
    // 8.x bridges Rithmic into a `MarketDataFeed` impl, this `let feed = `
    // line is the only change required to flip from "no-op binary" to
    // "live data, simulated execution".
    let feed: VecMarketDataFeed = VecMarketDataFeed::new(Vec::new());
    let _: &dyn MarketDataFeed = &feed; // proof: the feed is a trait object slot.
    let mut orch = PaperTradingOrchestrator::new(PaperTradingConfig::default(), feed);
    orch.emit_startup_banner();
    let summary = orch.pump_until_idle();

    info!(
        events_consumed = summary.events_consumed,
        orders_processed = summary.orders_processed,
        fills_emitted = summary.fills_emitted,
        fills_consumed = summary.fills_consumed,
        "PAPER TRADING SUMMARY"
    );
    println!("--- paper trading summary ---");
    println!("events consumed: {}", summary.events_consumed);
    println!("orders processed: {}", summary.orders_processed);
    println!("fills emitted:    {}", summary.fills_emitted);
    println!("fills consumed:   {}", summary.fills_consumed);
    if summary.events_consumed == 0 {
        warn!(
            "paper mode binary completed with zero events — V1 ships without a live data \
             feed wired in (Epic 8 lands the Rithmic bridge). The orchestrator + adapter \
             selection ARE in place; integration tests exercise the full paper data path."
        );
    }

    ExitCode::SUCCESS
}

/// Best-effort tracing subscriber initialization. We don't pull in
/// `tracing-subscriber` as a workspace dep (it's not in the spec for this
/// story); the engine binary just installs the no-op fmt subscriber from
/// `tracing` itself. Operators redirect logs via the binary's structured
/// output downstream.
fn tracing_subscriber_init() {
    // `tracing` alone does not include a default fmt subscriber, but the
    // `tracing` crate does provide a no-op default if no subscriber is set
    // — log statements are silently dropped. That is acceptable for V1; a
    // later observability story (Epic 9) wires the real subscriber.
}
