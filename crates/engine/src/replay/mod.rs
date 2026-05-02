//! Replay subsystem — Story 7.1.
//!
//! The [`ReplayOrchestrator`] is a STARTUP-TIME concern. It assembles the
//! same engine pieces live trading uses (SPSC market-event queue, SPSC
//! order/fill queues, event loop, broker adapter, clock) but with replay
//! implementations injected:
//!
//! * `Clock` ⇒ [`futures_bmad_testkit::SimClock`] (deterministic; advanced
//!   to each recorded `MarketEvent::timestamp`).
//! * `BrokerAdapter` ⇒ [`futures_bmad_testkit::MockBrokerAdapter`] (no
//!   network; orders are answered with synthetic fills via
//!   [`MockFillSimulator`]).
//! * Market data ⇒ [`data_source::ParquetReplaySource`] (own-format Parquet
//!   files; single file or directory of date-partitioned daily files).
//!
//! Once `run()` returns, the engine event loop has consumed every replayed
//! market event, the journal contains the full audit trail, and a
//! [`ReplaySummary`] is logged + returned to the caller.
//!
//! NO replay-specific branching exists inside the event loop — see
//! `event_loop.rs`. The trait-based wiring is the entire mechanism.

pub mod data_source;
pub mod driver;
pub mod fill_sim;
pub mod orchestrator;

pub use data_source::{ParquetReplaySource, ReplaySourceError};
pub use driver::ReplayDriver;
pub use fill_sim::{FillModel, MockFillSimulator};
pub use orchestrator::{
    ReplayConfig, ReplayError, ReplayOrchestrator, ReplaySummary,
};
