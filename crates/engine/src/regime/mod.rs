//! Regime detection module.
//!
//! Story 6.1 introduces the [`threshold::ThresholdRegimeDetector`], a V1
//! threshold-based regime classifier consuming OHLCV [`Bar`](futures_bmad_core::Bar)
//! data. Future stories may layer additional detectors (HMM, ML-based)
//! behind the same [`RegimeDetector`](futures_bmad_core::RegimeDetector)
//! trait, but Epic 6 ships the threshold variant only — HMM-based regime
//! detection is explicitly deferred until training data is available.
//!
//! Hard rules (from architecture spec):
//!   * `Unknown` regime blocks trading by default (configurable per detector
//!     via the `unknown_blocks_trading` flag on the detector's config struct).
//!   * Detectors operate on completed bars (1-min or 5-min interval), not
//!     every tick — the bar interval is configurable.
//!   * One ring-buffer allocation at construction time; no per-update heap
//!     allocation on the hot path.

#![deny(unsafe_code)]

pub mod threshold;

pub use threshold::{ThresholdRegimeConfig, ThresholdRegimeDetector};
