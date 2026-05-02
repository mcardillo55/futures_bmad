//! End-to-end integration tests for [`futures_bmad_engine::replay::ReplayOrchestrator`].
//!
//! Story 7.1 Task 9: drive a small known-data Parquet file end-to-end through
//! the orchestrator and assert on summary output, journal contents, and
//! deterministic time advancement.

use std::path::PathBuf;

use chrono::NaiveDate;
use futures_bmad_core::{Clock, FixedPrice, MarketEvent, MarketEventType, Side, UnixNanos};
use futures_bmad_engine::persistence::{
    EngineEvent as JournalEvent, EventJournal, MarketDataWriter,
};
use futures_bmad_engine::replay::{
    FillModel, ParquetReplaySource, ReplayConfig, ReplayOrchestrator,
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
    futures_bmad_engine::persistence::parquet_writer::file_path_for(dir, symbol, date)
}

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

fn trade(ts: u64, price: i64) -> MarketEvent {
    MarketEvent {
        timestamp: UnixNanos::new(ts),
        symbol_id: 0,
        sequence: 0,
        event_type: MarketEventType::Trade,
        price: FixedPrice::new(price),
        size: 1,
        side: Some(Side::Buy),
    }
}

/// 9.1 / 9.4 — end-to-end: build a known dataset, replay it, assert the
/// summary fields match expected values.
#[test]
fn end_to_end_replay_summary_matches_known_values() {
    let dir = tempfile::tempdir().unwrap();
    let ts0 = 1_776_470_400_000_000_000u64; // 2026-04-18 00:00:00 UTC
    let path = write_test_file(
        dir.path(),
        "MES",
        NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        &[
            bid(ts0, 18_000),
            ask(ts0 + 1_000_000, 18_004),
            trade(ts0 + 2_000_000, 18_002),
            trade(ts0 + 3_000_000, 18_002),
            bid(ts0 + 4_000_000, 18_001),
            ask(ts0 + 5_000_000, 18_005),
        ],
    );

    let mut orch = ReplayOrchestrator::new(ReplayConfig::new(path)).unwrap();
    let summary = orch.run();
    assert_eq!(summary.total_events, 6);
    assert_eq!(summary.total_trades, 0); // no orders submitted
    assert_eq!(summary.net_pnl_qt, 0);
    // Recorded duration = first → last event = 5ms.
    assert_eq!(summary.recorded_duration_nanos, 5_000_000);
    // Clock was advanced to the last timestamp.
    assert_eq!(orch.clock().now().as_nanos(), ts0 + 5_000_000);
    // Order book reflects the last bid + ask.
    assert_eq!(orch.book().best_bid().unwrap().price.raw(), 18_001);
    assert_eq!(orch.book().best_ask().unwrap().price.raw(), 18_005);
}

/// 9.3 — replay events flow through the journal when one is attached. A
/// system event records replay start and replay completion.
#[test]
fn end_to_end_replay_journals_start_and_complete() {
    let dir = tempfile::tempdir().unwrap();
    let ts0 = 1_776_470_400_000_000_000u64;
    let path = write_test_file(
        dir.path(),
        "MES",
        NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        &[bid(ts0, 18_000), ask(ts0 + 1_000_000, 18_004)],
    );

    let mut orch = ReplayOrchestrator::new(ReplayConfig::new(path)).unwrap();
    let (sender, receiver) = EventJournal::channel();
    orch.attach_journal(sender);
    let summary = orch.run();
    assert_eq!(summary.total_events, 2);

    let mut categories: Vec<String> = Vec::new();
    for _ in 0..16 {
        match receiver.try_recv_for_test() {
            Some(JournalEvent::SystemEvent(rec)) => categories.push(rec.category),
            Some(_) => {}
            None => break,
        }
    }
    assert!(
        categories.iter().any(|c| c == "replay_start"),
        "expected replay_start in {categories:?}"
    );
    assert!(
        categories.iter().any(|c| c == "replay_complete"),
        "expected replay_complete in {categories:?}"
    );
}

/// 9.x — multi-file directory replay yields the same summary as a single
/// file containing the union of events.
#[test]
fn multi_file_directory_replay_aggregates_summary() {
    let dir = tempfile::tempdir().unwrap();
    let ts1 = 1_776_470_400_000_000_000u64;
    let ts2 = ts1 + 86_400_000_000_000;
    write_test_file(
        dir.path(),
        "MES",
        NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        &[bid(ts1, 18_000), ask(ts1 + 1_000_000, 18_004)],
    );
    write_test_file(
        dir.path(),
        "MES",
        NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
        &[bid(ts2, 18_100), ask(ts2 + 1_000_000, 18_105)],
    );

    let symbol_dir = dir.path().join("market").join("MES");
    let mut orch = ReplayOrchestrator::new(ReplayConfig::new(symbol_dir)).unwrap();
    let summary = orch.run();
    assert_eq!(summary.total_events, 4);
    assert_eq!(orch.clock().now().as_nanos(), ts2 + 1_000_000);
}

/// Construct + immediately drop the orchestrator without calling run() —
/// no panic, no resource leak.
#[test]
fn construct_only_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let ts0 = 1_776_470_400_000_000_000u64;
    let path = write_test_file(
        dir.path(),
        "MES",
        NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        &[bid(ts0, 18_000)],
    );
    let _orch = ReplayOrchestrator::new(ReplayConfig::new(path)).unwrap();
}

/// FillModel default is ImmediateAtMarket; explicit construction works
/// the same.
#[test]
fn fill_model_immediate_at_market_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let ts0 = 1_776_470_400_000_000_000u64;
    let path = write_test_file(
        dir.path(),
        "MES",
        NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        &[bid(ts0, 18_000), ask(ts0 + 1, 18_004)],
    );

    let cfg = ReplayConfig {
        parquet_path: path,
        fill_model: FillModel::ImmediateAtMarket,
        summary_output: false,
        market_event_capacity: Some(64),
        snapshot_interval: None,
        signal_instrumentation: futures_bmad_engine::replay::SignalInstrumentationConfig::default(),
        regime_instrumentation: None,
    };
    let mut orch = ReplayOrchestrator::new(cfg).unwrap();
    let summary = orch.run();
    assert_eq!(summary.total_events, 2);
}

/// `ParquetReplaySource` re-exports cleanly from the public API and the
/// schema validation surfaces a public error type.
#[test]
fn parquet_replay_source_public_api() {
    let dir = tempfile::tempdir().unwrap();
    let ts0 = 1_776_470_400_000_000_000u64;
    let path = write_test_file(
        dir.path(),
        "MES",
        NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        &[bid(ts0, 18_000)],
    );

    let mut src = ParquetReplaySource::open(&path).unwrap();
    let ev = src.next().unwrap();
    assert_eq!(ev.price.raw(), 18_000);
    assert_eq!(src.events_emitted(), 1);
    assert_eq!(src.file_count(), 1);
}
