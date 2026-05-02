//! Paper trading subsystem — Story 7.3.
//!
//! Paper trading runs the engine against LIVE market data while routing
//! orders to an in-process [`futures_bmad_testkit::MockBrokerAdapter`]
//! instead of the real Rithmic OrderPlant. This makes it possible to
//! validate the system end-to-end against real market conditions without
//! risking capital.
//!
//! Mode matrix (Story 7.3 Task 4.3):
//!
//! | Mode    | Clock                                    | BrokerAdapter                                  | Data source         |
//! |---------|------------------------------------------|------------------------------------------------|---------------------|
//! | Live    | [`futures_bmad_core::SystemClock`]       | `RithmicAdapter` (OrderPlant)                  | Rithmic live        |
//! | Paper   | [`futures_bmad_core::SystemClock`]       | [`futures_bmad_testkit::MockBrokerAdapter`]    | Rithmic live        |
//! | Replay  | [`futures_bmad_testkit::SimClock`]       | [`futures_bmad_testkit::MockBrokerAdapter`]    | Parquet files       |
//!
//! The clock and the broker adapter are independent axes — paper mode picks
//! `SystemClock` (data is live, timestamps are real) plus `MockBrokerAdapter`
//! (no orders ever leave the host). Only the replay path uses [`SimClock`].
//!
//! ## Architecture
//!
//! Like [`crate::replay::ReplayOrchestrator`], the paper-trading wiring is a
//! STARTUP-TIME concern. Once the orchestrator is built, every downstream
//! consumer (event loop, signals, regime detection, risk, journal) sees the
//! same trait objects it would see in live trading. There is NO branching
//! anywhere on `BrokerMode` outside the binary's startup function — the
//! single switch is the [`futures_bmad_core::BrokerMode`] enum read by
//! `main.rs` to pick which adapter to instantiate.
//!
//! Story 7.4 will extend this by tagging journaled trades with their
//! source mode (`paper` vs `live`); the orchestrator already exposes a
//! [`PaperTradingOrchestrator::attach_journal`] hook so 7.4 can layer on
//! tagging without re-engineering the wiring.

pub mod data_feed;
pub mod orchestrator;

pub use data_feed::{MarketDataFeed, VecMarketDataFeed};
pub use orchestrator::{
    PaperTradingConfig, PaperTradingError, PaperTradingOrchestrator, PaperTradingSummary,
};

// Re-export the journal query API so paper-mode callers reading session
// outcomes don't have to reach across module boundaries (Story 7.4).
pub use crate::persistence::query::{
    DecisionTrace, JournalQuery, OrderStateRecord, PnlSummary, ReadinessReport, TradeRecord,
};
