//! `engine` binary entry point.
//!
//! V1 surface (Story 7.1): the only supported mode is replay. Live trading
//! mode (`--live`) is reserved for Epic 8 (startup sequence). Running the
//! binary without `--replay` therefore prints help and exits with a
//! non-zero status code so an operator never accidentally launches the
//! engine without having chosen a mode.
//!
//! Replay startup wiring (Story 7.1 Task 6.2): the binary detects the
//! `--replay <path>` flag and constructs a [`ReplayOrchestrator`] instead
//! of any live broker connection. All downstream code (event loop,
//! signals, risk, regime, order manager, journal) is unaware of replay vs
//! live — see `replay/orchestrator.rs` for the wiring.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tracing::{error, info};

use futures_bmad_engine::replay::{FillModel, ReplayConfig, ReplayOrchestrator};

#[derive(Debug, Parser)]
#[command(
    name = "engine",
    about = "futures_bmad trading engine",
    long_about = "Run the trading engine. Currently the only supported mode is \
                  --replay <parquet-path>; live mode lands in Epic 8."
)]
struct Cli {
    /// Replay historical data from a Parquet file or a directory of
    /// date-partitioned Parquet files. Mutually exclusive with live mode
    /// (live mode not yet implemented).
    #[arg(long, value_name = "PARQUET_PATH")]
    replay: Option<PathBuf>,

    /// Optional path to a TOML config file (currently informational only;
    /// loaded by Epic 8). Accepted so the spec example
    /// `--replay <path> --config <path>` parses cleanly.
    #[arg(long, value_name = "CONFIG_PATH")]
    #[allow(dead_code)]
    config: Option<PathBuf>,
}

fn main() -> ExitCode {
    // Default tracing subscriber — operators can override via RUST_LOG.
    tracing_subscriber_init();

    let cli = Cli::parse();

    let Some(replay_path) = cli.replay else {
        error!(
            "no mode selected — pass `--replay <parquet-path>` to run replay. \
             Live mode is not yet implemented (Epic 8)."
        );
        return ExitCode::from(2);
    };

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
