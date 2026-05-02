//! End-to-end integration tests for Story 7.4 — paper trade result recording.
//!
//! These tests drive the full pipeline:
//!
//!   feed → orchestrator → SPSC order/fill queues → MockFillSimulator
//!     → JournalSender (paper-tagged) → EventJournal worker → SQLite
//!     → JournalQuery (read-side analytics)
//!
//! and verify that:
//!
//! * paper-tagged fills land in `trade_events` with `source = "paper"`,
//! * the same write path round-trips every required field
//!   (order_id, fill_price, fill_size, timestamp, side, decision_id, P&L),
//! * `pnl_summary` and `paper_readiness_report` produce values consistent
//!   with the synthetic trade history we drove through,
//! * decision_id traceability is end-to-end (trace_decision returns rows
//!   for the queried id and only that id).
//!
//! The integration tests deliberately use the `EventJournal::run` worker
//! loop (not just `write_event`) so the channel + batching path is
//! exercised — that is the production hot-path Story 7.4 must not regress.

use std::sync::Arc;
use std::thread;

use futures_bmad_core::{
    FillEvent, FixedPrice, MarketEvent, MarketEventType, OrderEvent, OrderType, Side, TradeSource,
    UnixNanos,
};
use futures_bmad_engine::paper::{PaperTradingConfig, PaperTradingOrchestrator, VecMarketDataFeed};
use futures_bmad_engine::persistence::{
    EngineEvent as JournalEvent, EventJournal, JournalQuery, JournalSender, OrderStateChangeRecord,
    TradeEventRecord,
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

/// Helper — drive one synthetic FillEvent through the journal sender as a
/// `TradeEvent` record. Mimics what the order/bracket manager would do
/// after observing a fill on the engine→broker→engine path.
fn record_fill(sender: &JournalSender, fill: FillEvent, kind: &str, symbol_id: u32) {
    let rec = TradeEventRecord {
        timestamp: fill.timestamp,
        decision_id: Some(fill.decision_id),
        order_id: Some(fill.order_id),
        symbol_id,
        side: fill.side,
        price: fill.fill_price,
        size: fill.fill_size,
        kind: kind.to_string(),
        // Source override on the sender will rewrite this to Paper.
        source: TradeSource::Live,
    };
    assert!(sender.send(JournalEvent::TradeEvent(rec)));
}

/// Spawn the journal worker on a thread, returning a `JoinHandle` that
/// completes once the receiver is closed (all senders dropped).
fn spawn_journal_worker(
    journal: EventJournal,
    receiver: futures_bmad_engine::persistence::JournalReceiver,
) -> thread::JoinHandle<EventJournal> {
    thread::spawn(move || {
        let mut journal = journal;
        journal.run(receiver).expect("journal worker run");
        journal
    })
}

/// Task 8.1 + 8.2 — run paper trading, generate multiple trades, and
/// confirm the journal records them with `source = "paper"`.
#[test]
fn end_to_end_paper_trades_persist_with_paper_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");
    let journal = EventJournal::new(&path).unwrap();
    let (sender, receiver) = EventJournal::channel();

    let feed = VecMarketDataFeed::new(vec![
        bid(1, 18_000),
        ask(2, 18_004),
        bid(3, 18_001),
        ask(4, 18_005),
    ]);
    let mut orch = PaperTradingOrchestrator::new(PaperTradingConfig::default(), feed);
    orch.attach_journal(sender);
    let paper_sender = orch.journal_sender().expect("journal attached");

    // Drive a tiny round-trip: buy 1 @ ask, sell 1 @ bid. We synthesize the
    // fills via the orchestrator's pump (which calls MockFillSimulator) and
    // then mirror the resulting fill events into the journal as the order
    // manager would in the production wiring.
    let _ = orch.pump_until_idle();

    // Buy 1 — fills at the best ask (18_005).
    let buy = OrderEvent {
        order_id: 1,
        symbol_id: 0,
        side: Side::Buy,
        quantity: 1,
        order_type: OrderType::Market,
        decision_id: 11,
        timestamp: UnixNanos::new(5),
    };
    assert!(orch.order_producer_mut().try_push(buy));
    orch.drain_orders_and_simulate_fills();
    let buy_fill = orch.fill_consumer_mut().try_pop().unwrap();
    record_fill(&paper_sender, buy_fill, "fill", 0);

    // Sell 1 — fills at the best bid (18_001). Same decision_id pair so
    // P&L pairing closes the leg; net = 18_001 - 18_005 = -4.
    let sell = OrderEvent {
        order_id: 2,
        symbol_id: 0,
        side: Side::Sell,
        quantity: 1,
        order_type: OrderType::Market,
        decision_id: 11,
        timestamp: UnixNanos::new(6),
    };
    assert!(orch.order_producer_mut().try_push(sell));
    orch.drain_orders_and_simulate_fills();
    let sell_fill = orch.fill_consumer_mut().try_pop().unwrap();
    record_fill(&paper_sender, sell_fill, "fill", 0);

    // Drop the orchestrator's sender clones so the worker can exit cleanly.
    drop(paper_sender);
    drop(orch);

    let worker = spawn_journal_worker(journal, receiver);
    let _journal = worker.join().expect("worker join");

    // Open a fresh read connection and assert the journal records.
    let conn = rusqlite::Connection::open(&path).unwrap();
    let q = JournalQuery::new(&conn);
    let paper_trades = q.trades_by_source(TradeSource::Paper).unwrap();
    assert_eq!(
        paper_trades.len(),
        2,
        "two fills were recorded under the paper source"
    );
    assert!(paper_trades.iter().all(|t| t.source == TradeSource::Paper));
    // No live or replay rows exist in this run.
    assert_eq!(q.trades_by_source(TradeSource::Live).unwrap().len(), 0);
    assert_eq!(q.trades_by_source(TradeSource::Replay).unwrap().len(), 0);
}

/// Task 8.3 — pnl_summary returns the expected numeric values for the
/// trades the test drove through.
#[test]
fn end_to_end_pnl_summary_matches_synthetic_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");
    let journal = EventJournal::new(&path).unwrap();
    let (sender, receiver) = EventJournal::channel_with_source(TradeSource::Paper);

    // Record three round-trip pairs: two wins (+10, +5), one loss (-3).
    // Net: +12. wins=2 losses=1 trade_count=3. peak=10, then +15, then +12
    // ⇒ no drawdown after peak; max_dd = 0 across this monotonic curve…
    // …actually peak is +10 after pair 1, +15 after pair 2, +12 after pair
    // 3. Drawdown = 15 - 12 = 3.
    let pairs: &[(i64, i64)] = &[
        (100, 110), // long @100, sell @110 ⇒ +10
        (110, 115), // long @110, sell @115 ⇒ +5
        (115, 112), // long @115, sell @112 ⇒ -3
    ];
    let mut ts = 0u64;
    let mut order = 1u64;
    for (decision, (entry, exit)) in (1u64..).zip(pairs.iter()) {
        let buy = TradeEventRecord {
            timestamp: UnixNanos::new(ts),
            decision_id: Some(decision),
            order_id: Some(order),
            symbol_id: 0,
            side: Side::Buy,
            price: FixedPrice::new(*entry),
            size: 1,
            kind: "fill".to_string(),
            source: TradeSource::default(),
        };
        order += 1;
        ts += 1;
        let sell = TradeEventRecord {
            timestamp: UnixNanos::new(ts),
            decision_id: Some(decision),
            order_id: Some(order),
            symbol_id: 0,
            side: Side::Sell,
            price: FixedPrice::new(*exit),
            size: 1,
            kind: "fill".to_string(),
            source: TradeSource::default(),
        };
        ts += 1;
        order += 1;
        assert!(sender.send(JournalEvent::TradeEvent(buy)));
        assert!(sender.send(JournalEvent::TradeEvent(sell)));
    }
    drop(sender);
    let worker = spawn_journal_worker(journal, receiver);
    let _journal = worker.join().expect("worker");

    let conn = rusqlite::Connection::open(&path).unwrap();
    let q = JournalQuery::new(&conn);
    let s = q.pnl_summary(TradeSource::Paper).unwrap();
    assert_eq!(s.trade_count, 3);
    assert_eq!(s.win_count, 2);
    assert_eq!(s.loss_count, 1);
    assert_eq!(s.net_pnl.raw(), 12);
    assert_eq!(s.max_drawdown.raw(), 3);
    assert!((s.win_rate - 2.0 / 3.0).abs() < 1e-12);
}

/// Task 8.4 — decision_id traceability across the full chain.
///
/// We write both an `OrderStateChange` and matched fill `TradeEvent` rows
/// under the same decision_id, then verify `trace_decision` returns every
/// one of them — and only those (a different decision_id is recorded too,
/// to confirm the WHERE clause filters correctly).
#[test]
fn end_to_end_decision_id_traceability() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");
    let journal = EventJournal::new(&path).unwrap();
    let (sender, receiver) = EventJournal::channel_with_source(TradeSource::Paper);

    // Decision 42 — the trade we want to trace.
    let dec42 = 42u64;
    let other = 99u64;

    let mk_state =
        |order_id: u64, decision: u64, ts: u64, from: &str, to: &str| OrderStateChangeRecord {
            timestamp: UnixNanos::new(ts),
            order_id,
            decision_id: Some(decision),
            from_state: from.into(),
            to_state: to.into(),
            source: TradeSource::default(),
        };
    let mk_fill =
        |ts: u64, decision: u64, order_id: u64, side: Side, price_qt: i64| TradeEventRecord {
            timestamp: UnixNanos::new(ts),
            decision_id: Some(decision),
            order_id: Some(order_id),
            symbol_id: 0,
            side,
            price: FixedPrice::new(price_qt),
            size: 1,
            kind: "fill".to_string(),
            source: TradeSource::default(),
        };

    // Decision 42: Idle->Submitted, Submitted->Filled, two fills (entry+exit).
    sender.send(JournalEvent::OrderStateChange(mk_state(
        1,
        dec42,
        1,
        "Idle",
        "Submitted",
    )));
    sender.send(JournalEvent::OrderStateChange(mk_state(
        1,
        dec42,
        2,
        "Submitted",
        "Filled",
    )));
    sender.send(JournalEvent::TradeEvent(mk_fill(
        3,
        dec42,
        1,
        Side::Buy,
        100,
    )));
    sender.send(JournalEvent::TradeEvent(mk_fill(
        4,
        dec42,
        1,
        Side::Sell,
        110,
    )));
    // Decision 99 — a different decision.
    sender.send(JournalEvent::OrderStateChange(mk_state(
        2,
        other,
        5,
        "Idle",
        "Submitted",
    )));
    sender.send(JournalEvent::TradeEvent(mk_fill(
        6,
        other,
        2,
        Side::Buy,
        200,
    )));

    drop(sender);
    let worker = spawn_journal_worker(journal, receiver);
    let _journal = worker.join().expect("worker");

    let conn = rusqlite::Connection::open(&path).unwrap();
    let q = JournalQuery::new(&conn);
    let trace = q.trace_decision(dec42).unwrap();
    assert_eq!(trace.decision_id, dec42);
    assert_eq!(trace.trades.len(), 2);
    assert_eq!(trace.order_states.len(), 2);
    assert!(trace.trades.iter().all(|t| t.decision_id == Some(dec42)));
    assert!(
        trace
            .order_states
            .iter()
            .all(|s| s.decision_id == Some(dec42))
    );

    // Other decision is also reachable, separately.
    let other_trace = q.trace_decision(other).unwrap();
    assert_eq!(other_trace.trades.len(), 1);
    assert_eq!(other_trace.order_states.len(), 1);

    // A non-existent decision returns an empty trace.
    let none = q.trace_decision(123_456).unwrap();
    assert_eq!(none.total_rows(), 0);
}

/// Task 3.4 — comparison test: a paper-tagged trade and a live-tagged
/// trade with the same input data produce journal records that differ only
/// in their `source` column.
#[test]
fn paper_and_live_trades_differ_only_in_source_tag() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");
    let journal = EventJournal::new(&path).unwrap();
    let (paper_sender, paper_rx) = EventJournal::channel_with_source(TradeSource::Paper);
    let (live_sender, live_rx) = EventJournal::channel_with_source(TradeSource::Live);

    // Same input on both senders: the only difference at write time is the
    // sender's source override.
    let template = TradeEventRecord {
        timestamp: UnixNanos::new(1_700_000_000_000_000_000),
        decision_id: Some(7),
        order_id: Some(42),
        symbol_id: 1,
        side: Side::Buy,
        price: FixedPrice::new(17929),
        size: 1,
        kind: "fill".to_string(),
        source: TradeSource::Live, // overridden on send
    };
    paper_sender.send(JournalEvent::TradeEvent(template.clone()));
    live_sender.send(JournalEvent::TradeEvent(template.clone()));
    drop(paper_sender);
    drop(live_sender);

    // Drain both receivers via a single journal worker — we move the
    // already-built journal into the worker thread and re-attach a SECOND
    // receiver via a wrapper. Simpler: run two workers sequentially.
    let mut journal = journal;
    while let Some(ev) = paper_rx.try_recv_for_test() {
        journal.write_event(&ev).unwrap();
    }
    while let Some(ev) = live_rx.try_recv_for_test() {
        journal.write_event(&ev).unwrap();
    }
    let path = journal.db_path().to_path_buf();
    drop(journal);

    let conn = rusqlite::Connection::open(&path).unwrap();
    let q = JournalQuery::new(&conn);
    let paper = q.trades_by_source(TradeSource::Paper).unwrap();
    let live = q.trades_by_source(TradeSource::Live).unwrap();
    assert_eq!(paper.len(), 1);
    assert_eq!(live.len(), 1);

    // Field-by-field equality except source — confirms the schema records
    // identical content for both modes.
    let p = &paper[0];
    let l = &live[0];
    assert_eq!(p.timestamp, l.timestamp);
    assert_eq!(p.decision_id, l.decision_id);
    assert_eq!(p.order_id, l.order_id);
    assert_eq!(p.symbol_id, l.symbol_id);
    assert_eq!(p.side, l.side);
    assert_eq!(p.price.raw(), l.price.raw());
    assert_eq!(p.size, l.size);
    assert_eq!(p.kind, l.kind);
    assert_ne!(p.source, l.source);
}

/// Task 6.3 — `emit_readiness_summary` writes a readiness-summary system
/// event to the journal and returns the same report the inline log line
/// described.
#[test]
fn emit_readiness_summary_journals_session_summary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");
    let journal = EventJournal::new(&path).unwrap();
    let (sender, receiver) = EventJournal::channel();

    let feed = VecMarketDataFeed::new(Vec::new());
    let mut orch = PaperTradingOrchestrator::new(PaperTradingConfig::default(), feed);
    orch.attach_journal(sender);

    // Pump some paper-tagged fills into the journal so the readiness report
    // returns non-empty values.
    let paper_sender = orch.journal_sender().expect("journal attached");
    let mk_fill = |ts: u64, decision: u64, side: Side, price_qt: i64| TradeEventRecord {
        timestamp: UnixNanos::new(ts),
        decision_id: Some(decision),
        order_id: Some(decision),
        symbol_id: 0,
        side,
        price: FixedPrice::new(price_qt),
        size: 1,
        kind: "fill".to_string(),
        source: TradeSource::default(),
    };
    paper_sender.send(JournalEvent::TradeEvent(mk_fill(1, 1, Side::Buy, 100)));
    paper_sender.send(JournalEvent::TradeEvent(mk_fill(2, 1, Side::Sell, 110)));
    drop(paper_sender);

    // Spawn the worker so it persists everything sent so far. We need it to
    // exit before we open a read-only connection to the DB.
    let worker = spawn_journal_worker(journal, receiver);
    drop(orch); // last sender clone — worker can now exit
    let _journal = worker.join().expect("worker");

    // Now construct a fresh orchestrator (we already dropped the previous
    // one) so we can call emit_readiness_summary with a connection open
    // against the on-disk DB.
    let feed = VecMarketDataFeed::new(Vec::new());
    let orch2 = PaperTradingOrchestrator::new(PaperTradingConfig::default(), feed);
    let read_conn = rusqlite::Connection::open(&path).unwrap();
    let report = orch2.emit_readiness_summary(&read_conn).unwrap();
    assert_eq!(report.total_trades, 1);
    assert!(report.positive_expectancy);
    assert_eq!(report.net_pnl.raw(), 10);
}

/// Sanity check — the orchestrator's `journal_sender()` returns `None`
/// before `attach_journal()` is called, and a paper-tagged sender after.
#[test]
fn journal_sender_accessor_reports_attachment_state() {
    let feed = VecMarketDataFeed::new(Vec::new());
    let mut orch = PaperTradingOrchestrator::new(PaperTradingConfig::default(), feed);
    assert!(orch.journal_sender().is_none());

    let (sender, _rx) = EventJournal::channel();
    orch.attach_journal(sender);
    let s = orch.journal_sender().expect("attached");
    // Confirm the sender stamps Paper.
    assert_eq!(s.source_override(), Some(TradeSource::Paper));
}

/// Task 5.x — decision_id flows from order submission to fill and into the
/// journal record. We assert the chain by comparing the OrderEvent's
/// decision_id to the FillEvent's decision_id (set by MockFillSimulator)
/// and to the journal row's decision_id.
#[test]
fn decision_id_is_unbroken_from_order_to_journal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");
    let journal = EventJournal::new(&path).unwrap();
    let (sender, receiver) = EventJournal::channel();

    let feed = VecMarketDataFeed::new(vec![bid(1, 18_000), ask(2, 18_004)]);
    let mut orch = PaperTradingOrchestrator::new(PaperTradingConfig::default(), feed);
    orch.attach_journal(sender);
    let _ = orch.pump_until_idle();
    let paper_sender = orch.journal_sender().expect("attached");

    let original_decision = 4242u64;
    let order = OrderEvent {
        order_id: 1,
        symbol_id: 0,
        side: Side::Buy,
        quantity: 1,
        order_type: OrderType::Market,
        decision_id: original_decision,
        timestamp: UnixNanos::new(3),
    };
    assert!(orch.order_producer_mut().try_push(order));
    orch.drain_orders_and_simulate_fills();
    let fill = orch.fill_consumer_mut().try_pop().unwrap();
    assert_eq!(
        fill.decision_id, original_decision,
        "decision_id flows through MockFillSimulator unchanged"
    );

    // Mirror the fill into the journal as the order manager would.
    let rec = TradeEventRecord {
        timestamp: fill.timestamp,
        decision_id: Some(fill.decision_id),
        order_id: Some(fill.order_id),
        symbol_id: 0,
        side: fill.side,
        price: fill.fill_price,
        size: fill.fill_size,
        kind: "fill".into(),
        source: TradeSource::default(),
    };
    paper_sender.send(JournalEvent::TradeEvent(rec));
    drop(paper_sender);
    drop(orch);

    let worker = spawn_journal_worker(journal, receiver);
    let _journal = worker.join().expect("worker");

    let conn = rusqlite::Connection::open(&path).unwrap();
    let q = JournalQuery::new(&conn);
    let trace = q.trace_decision(original_decision).unwrap();
    assert_eq!(trace.trades.len(), 1);
    assert_eq!(trace.trades[0].decision_id, Some(original_decision));
}

// --- Multi-source isolation guard --------------------------------------

/// Sanity guard against cross-source contamination: writes paper, live,
/// and replay rows in interleaved order and confirms each query returns
/// only its source's rows.
#[test]
fn multi_source_isolation_holds_under_interleaved_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");
    let mut journal = EventJournal::new(&path).unwrap();

    let mk_rec = |ts: u64, src: TradeSource| TradeEventRecord {
        timestamp: UnixNanos::new(ts),
        decision_id: Some(ts),
        order_id: Some(ts),
        symbol_id: 0,
        side: Side::Buy,
        price: FixedPrice::new(100),
        size: 1,
        kind: "fill".into(),
        source: src,
    };
    // 6 records, alternating sources.
    for (i, src) in (1u64..=6).zip([
        TradeSource::Paper,
        TradeSource::Live,
        TradeSource::Replay,
        TradeSource::Paper,
        TradeSource::Live,
        TradeSource::Replay,
    ]) {
        journal
            .write_event(&JournalEvent::TradeEvent(mk_rec(i, src)))
            .unwrap();
    }

    let conn = rusqlite::Connection::open(&path).unwrap();
    let q = JournalQuery::new(&conn);
    assert_eq!(q.trade_count(TradeSource::Paper).unwrap(), 2);
    assert_eq!(q.trade_count(TradeSource::Live).unwrap(), 2);
    assert_eq!(q.trade_count(TradeSource::Replay).unwrap(), 2);
}

/// Compile-time guarantee: the Arc-wrapped sender pattern paper mode uses
/// continues to clone-and-stamp correctly. Future refactors that turn
/// JournalSender into a generic / behind a trait must keep this invariant.
#[test]
fn cloned_sender_inherits_source_override() {
    let (s, _r) = EventJournal::channel_with_source(TradeSource::Paper);
    let cloned = Arc::new(s).as_ref().clone();
    assert_eq!(cloned.source_override(), Some(TradeSource::Paper));
}
