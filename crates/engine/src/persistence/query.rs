//! Read-side queries against the SQLite event journal — Story 7.4.
//!
//! Where [`crate::persistence::journal`] owns the write path
//! (channel + worker + INSERT statements), this module owns the read path:
//! parameterized SELECTs that compute trade-level analytics on the rows the
//! journal has accumulated.
//!
//! The queries are deliberately small and direct (no ORM, no query builder).
//! Each function takes a [`rusqlite::Connection`] borrow and a
//! [`futures_bmad_core::TradeSource`] filter, returning a typed Rust value.
//! All functions are read-only; nothing in this module mutates the schema or
//! writes rows.
//!
//! ## Concurrency
//!
//! Same `!Send` constraint as [`EventJournal`]: the [`Connection`] must stay
//! pinned to a single thread. Open a fresh read-only connection on the query
//! thread (e.g. via `Connection::open_with_flags(path, OPEN_READ_ONLY)`) so
//! the journal worker keeps exclusive write access without contention.
//!
//! ## Invariants
//!
//! * All filter values are bound via `?N` placeholders (no string
//!   interpolation of user input — Story 7.4 Task 4.4).
//! * `decision_id` integrity is the audit-trail backbone (NFR17). The
//!   trace-decision query joins all four sources of decision-tagged rows
//!   (`trade_events`, `order_states`) so a single id surfaces every row that
//!   shares causal context.
//! * `ReadinessReport` is informational (Story 7.4 Task 6.4) — it does not
//!   make a deployment decision, only summarizes the journal contents so a
//!   human operator can.

use std::collections::BTreeMap;

use futures_bmad_core::{FixedPrice, Side, TradeSource, UnixNanos};
use rusqlite::{Connection, params};

use crate::persistence::journal::JournalError;

/// Single row of `trade_events` lifted into typed Rust values.
///
/// Mirrors [`crate::persistence::journal::TradeEventRecord`] but is returned
/// from the query path instead of accepted on the write path. `source` is
/// always populated (the column is `NOT NULL DEFAULT 'live'`), `decision_id`
/// uses the `0` sentinel-as-`None` convention from the writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeRecord {
    pub timestamp: UnixNanos,
    pub decision_id: Option<u64>,
    pub order_id: Option<u64>,
    pub symbol_id: u32,
    pub side: Side,
    pub price: FixedPrice,
    pub size: u32,
    pub kind: String,
    pub source: TradeSource,
}

/// Single row of `order_states` returned from the query path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderStateRecord {
    pub timestamp: UnixNanos,
    pub order_id: u64,
    pub decision_id: Option<u64>,
    pub from_state: String,
    pub to_state: String,
    pub source: TradeSource,
}

/// P&L summary computed by [`JournalQuery::pnl_summary`].
///
/// All P&L values are quarter-tick raw integers ([`FixedPrice::raw`]) — the
/// display layer converts. `win_rate` is in `[0.0, 1.0]` (NOT a percentage).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PnlSummary {
    pub net_pnl: FixedPrice,
    pub win_count: u32,
    pub loss_count: u32,
    pub win_rate: f64,
    pub max_drawdown: FixedPrice,
    pub trade_count: u32,
}

impl PnlSummary {
    /// Empty summary returned when no trades match the filter.
    pub fn empty() -> Self {
        Self {
            net_pnl: FixedPrice::new(0),
            win_count: 0,
            loss_count: 0,
            win_rate: 0.0,
            max_drawdown: FixedPrice::new(0),
            trade_count: 0,
        }
    }
}

/// Complete audit trail for a single decision_id — joins every journal row
/// that shares the id (Story 7.4 Task 5.3).
///
/// `trades` and `order_states` are returned in chronological order (ORDER BY
/// timestamp) so a reviewer reading the trace top-to-bottom sees the causal
/// sequence: signal → order submitted → confirmed → filled → P&L.
#[derive(Debug, Clone, Default)]
pub struct DecisionTrace {
    pub decision_id: u64,
    pub trades: Vec<TradeRecord>,
    pub order_states: Vec<OrderStateRecord>,
}

impl DecisionTrace {
    /// Total number of rows across both tables. `0` ⇒ no journal entry exists
    /// for the queried decision_id (typo, replay-only id, or not-yet-recorded).
    pub fn total_rows(&self) -> usize {
        self.trades.len() + self.order_states.len()
    }
}

/// Go/no-go readiness report computed by [`JournalQuery::paper_readiness_report`]
/// — Story 7.4 Task 6.1.
///
/// Informational. The deployment decision is human-made; this struct only
/// surfaces what the journal contains so the operator can decide.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReadinessReport {
    /// Total paper trades observed (paired entries that closed a leg).
    pub total_trades: u32,
    /// `true` when net P&L > 0 over the observed sample.
    pub positive_expectancy: bool,
    /// Win rate in `[0.0, 1.0]` over decided trades (excluding scratches).
    pub win_rate: f64,
    /// Sum of winning P&L divided by absolute sum of losing P&L. `0.0` if no
    /// losses, `f64::INFINITY` if losses exist but were exactly zero.
    pub profit_factor: f64,
    /// Largest peak-to-trough drawdown in raw quarter-ticks.
    pub max_drawdown: FixedPrice,
    /// Lightweight Sharpe estimate: mean per-trade P&L / std-dev of per-trade
    /// P&L (not annualised). `0.0` when std-dev is zero or fewer than 2
    /// trades.
    pub sharpe_estimate: f64,
    /// Longest run of consecutive losing trades observed in the sample.
    pub consecutive_loss_max: u32,
    /// Net P&L in raw quarter-ticks.
    pub net_pnl: FixedPrice,
}

impl ReadinessReport {
    /// Story 7.4 Task 6.2 — minimum-trades gate. The default minimum is 500
    /// trades (per the story spec) but callers can pass any threshold.
    pub fn meets_minimum_threshold(&self, min_trades: u32) -> bool {
        self.total_trades >= min_trades
    }

    /// Empty report for an empty-journal case (no paper trades observed).
    pub fn empty() -> Self {
        Self {
            total_trades: 0,
            positive_expectancy: false,
            win_rate: 0.0,
            profit_factor: 0.0,
            max_drawdown: FixedPrice::new(0),
            sharpe_estimate: 0.0,
            consecutive_loss_max: 0,
            net_pnl: FixedPrice::new(0),
        }
    }

    /// One-line summary suitable for `info!`-level logging at session end
    /// (Task 6.3). Format is `key=value` pairs, easy to grep.
    pub fn fmt_one_line(&self) -> String {
        format!(
            "trades={} net_pnl_qt={} positive_expectancy={} win_rate={:.3} \
             profit_factor={:.3} max_dd_qt={} sharpe_estimate={:.3} \
             consecutive_loss_max={}",
            self.total_trades,
            self.net_pnl.raw(),
            self.positive_expectancy,
            self.win_rate,
            self.profit_factor,
            self.max_drawdown.raw(),
            self.sharpe_estimate,
            self.consecutive_loss_max,
        )
    }
}

/// Read-side query API over the journal (Story 7.4 Task 4).
///
/// Stateless borrow over a [`Connection`]; intended to be constructed
/// per-query by callers that already own the connection. Lives in
/// `persistence/query.rs` so the writer (`journal.rs`) stays focused on the
/// channel + flush loop.
pub struct JournalQuery<'a> {
    conn: &'a Connection,
}

impl<'a> JournalQuery<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Load every `trade_events` row matching `source`, chronologically.
    pub fn trades_by_source(&self, source: TradeSource) -> Result<Vec<TradeRecord>, JournalError> {
        let mut stmt = self.conn.prepare(
            "SELECT timestamp, decision_id, order_id, symbol_id, side, price, size, kind, source
             FROM trade_events
             WHERE source = ?1
             ORDER BY timestamp ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![source.as_str()], row_to_trade_record)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Load every `order_states` row matching `source`, chronologically.
    pub fn order_states_by_source(
        &self,
        source: TradeSource,
    ) -> Result<Vec<OrderStateRecord>, JournalError> {
        let mut stmt = self.conn.prepare(
            "SELECT timestamp, order_id, decision_id, from_state, to_state, source
             FROM order_states
             WHERE source = ?1
             ORDER BY timestamp ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![source.as_str()], row_to_order_state_record)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Efficient `COUNT(*)` of trade rows for a given source (Task 4.3).
    pub fn trade_count(&self, source: TradeSource) -> Result<u32, JournalError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM trade_events WHERE source = ?1",
            params![source.as_str()],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u32)
    }

    /// Compute net P&L, win/loss counts, win rate, max drawdown, and trade
    /// count over all trade rows tagged `source` (Task 4.2).
    ///
    /// Trades are paired in FIFO order: opposite-side fills close existing
    /// legs and contribute one round-trip P&L per pairing. This mirrors the
    /// per-trade accounting used by [`crate::replay::ReplayOrchestrator`] so
    /// paper- and replay-mode summaries are directly comparable.
    pub fn pnl_summary(&self, source: TradeSource) -> Result<PnlSummary, JournalError> {
        let trades = self.trades_by_source(source)?;
        Ok(compute_pnl_summary(&trades))
    }

    /// Story 7.4 Task 5.3 — return every row across `trade_events` and
    /// `order_states` that carries this `decision_id`, regardless of source.
    ///
    /// The result is the complete audit trail for one trade decision: signal
    /// evaluation context (recorded as separate rows during signal capture),
    /// order submission, fills, and the closing P&L row. The chronological
    /// ordering inside each `Vec` lets a reviewer read the trace as a
    /// timeline.
    pub fn trace_decision(&self, decision_id: u64) -> Result<DecisionTrace, JournalError> {
        let mut trade_stmt = self.conn.prepare(
            "SELECT timestamp, decision_id, order_id, symbol_id, side, price, size, kind, source
             FROM trade_events
             WHERE decision_id = ?1
             ORDER BY timestamp ASC, id ASC",
        )?;
        let trade_rows = trade_stmt.query_map(params![decision_id as i64], row_to_trade_record)?;
        let mut trades = Vec::new();
        for row in trade_rows {
            trades.push(row?);
        }

        let mut order_stmt = self.conn.prepare(
            "SELECT timestamp, order_id, decision_id, from_state, to_state, source
             FROM order_states
             WHERE decision_id = ?1
             ORDER BY timestamp ASC, id ASC",
        )?;
        let order_rows =
            order_stmt.query_map(params![decision_id as i64], row_to_order_state_record)?;
        let mut order_states = Vec::new();
        for row in order_rows {
            order_states.push(row?);
        }

        Ok(DecisionTrace {
            decision_id,
            trades,
            order_states,
        })
    }

    /// Story 7.4 Task 6.1 — full readiness report over all paper trades.
    ///
    /// Computes:
    ///
    /// * `total_trades` — round-trip pair count
    /// * `positive_expectancy` — net P&L > 0
    /// * `win_rate` — wins / (wins + losses)
    /// * `profit_factor` — sum of winning P&L / abs sum of losing P&L
    /// * `max_drawdown` — largest peak-to-trough decline
    /// * `sharpe_estimate` — mean / std-dev of per-trade P&L (not annualised)
    /// * `consecutive_loss_max` — longest losing run
    /// * `net_pnl` — cumulative P&L in raw quarter-ticks
    pub fn paper_readiness_report(&self) -> Result<ReadinessReport, JournalError> {
        let trades = self.trades_by_source(TradeSource::Paper)?;
        Ok(compute_readiness_report(&trades))
    }
}

fn row_to_trade_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TradeRecord> {
    let timestamp: i64 = row.get(0)?;
    let decision_id: i64 = row.get(1)?;
    let order_id: Option<i64> = row.get(2)?;
    let symbol_id: i64 = row.get(3)?;
    let side: i64 = row.get(4)?;
    let price: i64 = row.get(5)?;
    let size: i64 = row.get(6)?;
    let kind: String = row.get(7)?;
    let source_str: String = row.get(8)?;
    Ok(TradeRecord {
        timestamp: UnixNanos::new(timestamp.max(0) as u64),
        decision_id: if decision_id == 0 {
            None
        } else {
            Some(decision_id as u64)
        },
        order_id: order_id.map(|id| id as u64),
        symbol_id: symbol_id.max(0) as u32,
        side: side_from_i64(side),
        price: FixedPrice::new(price),
        size: size.max(0) as u32,
        kind,
        source: TradeSource::parse(&source_str).unwrap_or_default(),
    })
}

fn row_to_order_state_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<OrderStateRecord> {
    let timestamp: i64 = row.get(0)?;
    let order_id: i64 = row.get(1)?;
    let decision_id: Option<i64> = row.get(2)?;
    let from_state: String = row.get(3)?;
    let to_state: String = row.get(4)?;
    let source_str: String = row.get(5)?;
    Ok(OrderStateRecord {
        timestamp: UnixNanos::new(timestamp.max(0) as u64),
        order_id: order_id.max(0) as u64,
        decision_id: decision_id.map(|id| id as u64),
        from_state,
        to_state,
        source: TradeSource::parse(&source_str).unwrap_or_default(),
    })
}

fn side_from_i64(s: i64) -> Side {
    // Mirrors `side_to_i64` in journal.rs (Buy=0, Sell=1).
    match s {
        0 => Side::Buy,
        _ => Side::Sell,
    }
}

/// FIFO leg used by [`compute_pnl_summary`] / [`compute_readiness_report`].
#[derive(Clone, Copy)]
struct OpenLeg {
    side: Side,
    qty: u32,
    price_qt: i64,
}

/// Pair fills FIFO into round-trip trades and accumulate P&L statistics.
///
/// Only entries with `kind` matching one of `"fill"`, `"partial_fill"`,
/// `"tp_fill"`, `"sl_fill"`, `"entry_fill"` are considered. Other kinds
/// (`submit`, `cancel`, etc.) are pure audit rows and contribute no P&L.
fn compute_pnl_summary(trades: &[TradeRecord]) -> PnlSummary {
    let (cum_pnl, wins, losses, max_dd, _per_trade) = pair_trades_pnl(trades);
    let trade_count = (wins + losses) as u32;
    let win_rate = if trade_count == 0 {
        0.0
    } else {
        wins as f64 / trade_count as f64
    };
    PnlSummary {
        net_pnl: FixedPrice::new(cum_pnl),
        win_count: wins as u32,
        loss_count: losses as u32,
        win_rate,
        max_drawdown: FixedPrice::new(max_dd),
        trade_count,
    }
}

fn compute_readiness_report(trades: &[TradeRecord]) -> ReadinessReport {
    let (cum_pnl, wins, losses, max_dd, per_trade) = pair_trades_pnl(trades);
    let total_trades = (wins + losses) as u32;
    if total_trades == 0 {
        let mut empty = ReadinessReport::empty();
        empty.net_pnl = FixedPrice::new(cum_pnl);
        return empty;
    }

    let win_rate = wins as f64 / total_trades as f64;
    let positive_expectancy = cum_pnl > 0;

    // Profit factor: sum of winning P&L / |sum of losing P&L|.
    let mut sum_wins_qt: i64 = 0;
    let mut sum_losses_qt: i64 = 0;
    for &p in &per_trade {
        if p > 0 {
            sum_wins_qt = sum_wins_qt.saturating_add(p);
        } else if p < 0 {
            sum_losses_qt = sum_losses_qt.saturating_add(p);
        }
    }
    let profit_factor = if sum_losses_qt == 0 {
        if sum_wins_qt == 0 { 0.0 } else { f64::INFINITY }
    } else {
        sum_wins_qt as f64 / (sum_losses_qt.unsigned_abs() as f64)
    };

    // Sharpe-style estimate: mean / sample std-dev. Not annualised.
    let n = per_trade.len() as f64;
    let mean = per_trade.iter().map(|&p| p as f64).sum::<f64>() / n;
    let sharpe_estimate = if per_trade.len() >= 2 {
        let variance = per_trade
            .iter()
            .map(|&p| {
                let d = p as f64 - mean;
                d * d
            })
            .sum::<f64>()
            / (n - 1.0);
        let std_dev = variance.sqrt();
        if std_dev == 0.0 { 0.0 } else { mean / std_dev }
    } else {
        0.0
    };

    // Longest consecutive-loss run.
    let mut max_run = 0u32;
    let mut cur_run = 0u32;
    for &p in &per_trade {
        if p < 0 {
            cur_run += 1;
            if cur_run > max_run {
                max_run = cur_run;
            }
        } else {
            cur_run = 0;
        }
    }

    ReadinessReport {
        total_trades,
        positive_expectancy,
        win_rate,
        profit_factor,
        max_drawdown: FixedPrice::new(max_dd),
        sharpe_estimate,
        consecutive_loss_max: max_run,
        net_pnl: FixedPrice::new(cum_pnl),
    }
}

/// Shared FIFO pairing — returns `(cum_pnl_qt, wins, losses, max_drawdown,
/// per_trade_pnls)`.
///
/// Per-trade P&L is recorded as the close P&L (entry vs exit, multiplied by
/// matched qty) — the same convention the replay [`TradeStats`] uses.
fn pair_trades_pnl(trades: &[TradeRecord]) -> (i64, u64, u64, i64, Vec<i64>) {
    // Per-symbol FIFO queues so multi-symbol journals don't cross-pair.
    let mut legs: BTreeMap<u32, Vec<OpenLeg>> = BTreeMap::new();
    let mut cum_pnl: i64 = 0;
    let mut peak: i64 = 0;
    let mut max_dd: i64 = 0;
    let mut wins: u64 = 0;
    let mut losses: u64 = 0;
    let mut per_trade: Vec<i64> = Vec::new();

    for tr in trades {
        if !is_fill_kind(&tr.kind) {
            continue;
        }
        if tr.size == 0 {
            continue;
        }

        let mut remaining = tr.size;
        let queue = legs.entry(tr.symbol_id).or_default();

        // Drain opposing-side legs first (FIFO close).
        while remaining > 0
            && queue
                .first()
                .map(|leg| leg.side != tr.side)
                .unwrap_or(false)
        {
            let leg = queue.first_mut().unwrap();
            let close_qty = remaining.min(leg.qty);
            // Long leg: P&L = (exit - entry) * qty
            // Short leg: P&L = (entry - exit) * qty
            let leg_pnl = match leg.side {
                Side::Buy => tr.price.raw().saturating_sub(leg.price_qt),
                Side::Sell => leg.price_qt.saturating_sub(tr.price.raw()),
            }
            .saturating_mul(close_qty as i64);
            cum_pnl = cum_pnl.saturating_add(leg_pnl);
            per_trade.push(leg_pnl);
            if leg_pnl > 0 {
                wins += 1;
            } else if leg_pnl < 0 {
                losses += 1;
            }
            // Drawdown bookkeeping after each closed pair.
            if cum_pnl > peak {
                peak = cum_pnl;
            }
            let dd = peak.saturating_sub(cum_pnl);
            if dd > max_dd {
                max_dd = dd;
            }
            leg.qty -= close_qty;
            remaining -= close_qty;
            if leg.qty == 0 {
                queue.remove(0);
            }
        }
        // Anything left opens a new same-side leg.
        if remaining > 0 {
            queue.push(OpenLeg {
                side: tr.side,
                qty: remaining,
                price_qt: tr.price.raw(),
            });
        }
    }

    (cum_pnl, wins, losses, max_dd, per_trade)
}

fn is_fill_kind(kind: &str) -> bool {
    matches!(
        kind,
        "fill" | "partial_fill" | "tp_fill" | "sl_fill" | "entry_fill"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::EventJournal;
    use crate::persistence::journal::{
        EngineEvent as JournalEvent, OrderStateChangeRecord, TradeEventRecord,
    };
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn open_journal() -> (EventJournal, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.db");
        let journal = EventJournal::new(&path).unwrap();
        (journal, dir)
    }

    fn read_only_conn(journal: &EventJournal) -> Connection {
        // Tests open a second connection to the same file so the writer's
        // owned `Connection` stays untouched while we exercise queries.
        Connection::open(journal.db_path()).unwrap()
    }

    fn fill(
        ts: u64,
        decision_id: Option<u64>,
        order_id: Option<u64>,
        side: Side,
        price_qt: i64,
        size: u32,
        source: TradeSource,
        kind: &str,
    ) -> TradeEventRecord {
        TradeEventRecord {
            timestamp: UnixNanos::new(ts),
            decision_id,
            order_id,
            symbol_id: 1,
            side,
            price: FixedPrice::new(price_qt),
            size,
            kind: kind.to_string(),
            source,
        }
    }

    fn write(journal: &mut EventJournal, ev: JournalEvent) {
        journal.write_event(&ev).unwrap();
    }

    /// Task 4.1 — trades_by_source filters by source tag.
    #[test]
    fn trades_by_source_returns_only_matching_rows() {
        let (mut journal, _dir) = open_journal();
        write(
            &mut journal,
            JournalEvent::TradeEvent(fill(
                1,
                Some(1),
                Some(1),
                Side::Buy,
                100,
                1,
                TradeSource::Paper,
                "fill",
            )),
        );
        write(
            &mut journal,
            JournalEvent::TradeEvent(fill(
                2,
                Some(2),
                Some(2),
                Side::Sell,
                110,
                1,
                TradeSource::Paper,
                "fill",
            )),
        );
        write(
            &mut journal,
            JournalEvent::TradeEvent(fill(
                3,
                Some(3),
                Some(3),
                Side::Buy,
                100,
                1,
                TradeSource::Live,
                "fill",
            )),
        );
        write(
            &mut journal,
            JournalEvent::TradeEvent(fill(
                4,
                Some(4),
                Some(4),
                Side::Buy,
                100,
                1,
                TradeSource::Replay,
                "fill",
            )),
        );

        let conn = read_only_conn(&journal);
        let q = JournalQuery::new(&conn);
        let paper = q.trades_by_source(TradeSource::Paper).unwrap();
        let live = q.trades_by_source(TradeSource::Live).unwrap();
        let replay = q.trades_by_source(TradeSource::Replay).unwrap();
        assert_eq!(paper.len(), 2);
        assert_eq!(live.len(), 1);
        assert_eq!(replay.len(), 1);
        assert!(paper.iter().all(|t| t.source == TradeSource::Paper));
        assert!(live.iter().all(|t| t.source == TradeSource::Live));
        assert!(replay.iter().all(|t| t.source == TradeSource::Replay));
    }

    /// Task 4.3 — trade_count returns an efficient COUNT and matches Vec len.
    #[test]
    fn trade_count_matches_vec_length() {
        let (mut journal, _dir) = open_journal();
        for i in 0..5 {
            write(
                &mut journal,
                JournalEvent::TradeEvent(fill(
                    i,
                    Some(i),
                    Some(i),
                    Side::Buy,
                    100,
                    1,
                    TradeSource::Paper,
                    "fill",
                )),
            );
        }
        write(
            &mut journal,
            JournalEvent::TradeEvent(fill(
                100,
                Some(100),
                Some(100),
                Side::Buy,
                100,
                1,
                TradeSource::Live,
                "fill",
            )),
        );

        let conn = read_only_conn(&journal);
        let q = JournalQuery::new(&conn);
        assert_eq!(q.trade_count(TradeSource::Paper).unwrap(), 5);
        assert_eq!(q.trade_count(TradeSource::Live).unwrap(), 1);
        assert_eq!(q.trade_count(TradeSource::Replay).unwrap(), 0);
    }

    /// Task 4.2 — pnl_summary computes net P&L, wins, losses, win rate, max
    /// drawdown over a known dataset.
    ///
    /// Scenario: paper trades — Buy 1@100, Sell 1@110 (win +10), Sell 1@95
    /// (open short), Buy 1@100 (loss -5), Buy 1@100, Sell 1@99 (loss -1).
    /// Net = 10 - 5 - 1 = +4.
    #[test]
    fn pnl_summary_known_dataset() {
        let (mut journal, _dir) = open_journal();
        let trades = vec![
            // Win pair +10
            fill(
                1,
                Some(1),
                Some(1),
                Side::Buy,
                100,
                1,
                TradeSource::Paper,
                "fill",
            ),
            fill(
                2,
                Some(1),
                Some(1),
                Side::Sell,
                110,
                1,
                TradeSource::Paper,
                "fill",
            ),
            // Loss pair -5: short @95, cover @100
            fill(
                3,
                Some(2),
                Some(2),
                Side::Sell,
                95,
                1,
                TradeSource::Paper,
                "fill",
            ),
            fill(
                4,
                Some(2),
                Some(2),
                Side::Buy,
                100,
                1,
                TradeSource::Paper,
                "fill",
            ),
            // Loss pair -1: long @100, sell @99
            fill(
                5,
                Some(3),
                Some(3),
                Side::Buy,
                100,
                1,
                TradeSource::Paper,
                "fill",
            ),
            fill(
                6,
                Some(3),
                Some(3),
                Side::Sell,
                99,
                1,
                TradeSource::Paper,
                "fill",
            ),
        ];
        for t in trades {
            write(&mut journal, JournalEvent::TradeEvent(t));
        }
        let conn = read_only_conn(&journal);
        let q = JournalQuery::new(&conn);
        let s = q.pnl_summary(TradeSource::Paper).unwrap();
        assert_eq!(s.trade_count, 3);
        assert_eq!(s.win_count, 1);
        assert_eq!(s.loss_count, 2);
        assert_eq!(s.net_pnl.raw(), 4);
        // Max drawdown = peak (10) minus running min after losses (-1+5 = +4)
        // = 10 - 4 = 6.
        assert_eq!(s.max_drawdown.raw(), 6);
        assert!((s.win_rate - 1.0 / 3.0).abs() < 1e-12);
    }

    /// Empty journal returns the empty summary cleanly.
    #[test]
    fn pnl_summary_empty_when_no_trades() {
        let (journal, _dir) = open_journal();
        let conn = read_only_conn(&journal);
        let q = JournalQuery::new(&conn);
        let s = q.pnl_summary(TradeSource::Paper).unwrap();
        assert_eq!(s, PnlSummary::empty());
    }

    /// Task 5.3 — trace_decision returns every row sharing a decision_id
    /// across both `trade_events` and `order_states`.
    #[test]
    fn trace_decision_returns_all_rows_for_id() {
        let (mut journal, _dir) = open_journal();
        // Two rows under decision 7, one under decision 8.
        write(
            &mut journal,
            JournalEvent::TradeEvent(fill(
                1,
                Some(7),
                Some(1),
                Side::Buy,
                100,
                1,
                TradeSource::Paper,
                "fill",
            )),
        );
        write(
            &mut journal,
            JournalEvent::TradeEvent(fill(
                2,
                Some(7),
                Some(1),
                Side::Sell,
                110,
                1,
                TradeSource::Paper,
                "fill",
            )),
        );
        write(
            &mut journal,
            JournalEvent::TradeEvent(fill(
                3,
                Some(8),
                Some(2),
                Side::Buy,
                200,
                1,
                TradeSource::Paper,
                "fill",
            )),
        );
        write(
            &mut journal,
            JournalEvent::OrderStateChange(OrderStateChangeRecord {
                timestamp: UnixNanos::new(0),
                order_id: 1,
                decision_id: Some(7),
                from_state: "Idle".into(),
                to_state: "Submitted".into(),
                source: TradeSource::Paper,
            }),
        );
        write(
            &mut journal,
            JournalEvent::OrderStateChange(OrderStateChangeRecord {
                timestamp: UnixNanos::new(1),
                order_id: 1,
                decision_id: Some(7),
                from_state: "Submitted".into(),
                to_state: "Filled".into(),
                source: TradeSource::Paper,
            }),
        );

        let conn = read_only_conn(&journal);
        let q = JournalQuery::new(&conn);
        let trace = q.trace_decision(7).unwrap();
        assert_eq!(trace.decision_id, 7);
        assert_eq!(trace.trades.len(), 2);
        assert_eq!(trace.order_states.len(), 2);
        assert_eq!(trace.total_rows(), 4);

        // decision_id=999 returns an empty trace, no error.
        let empty = q.trace_decision(999).unwrap();
        assert_eq!(empty.total_rows(), 0);
    }

    /// Task 6.1 — paper_readiness_report computes profit factor, drawdown,
    /// Sharpe estimate, consecutive-loss max over a small known dataset.
    #[test]
    fn paper_readiness_report_known_dataset() {
        let (mut journal, _dir) = open_journal();
        let trades = vec![
            // Win +10
            fill(
                1,
                Some(1),
                Some(1),
                Side::Buy,
                100,
                1,
                TradeSource::Paper,
                "fill",
            ),
            fill(
                2,
                Some(1),
                Some(1),
                Side::Sell,
                110,
                1,
                TradeSource::Paper,
                "fill",
            ),
            // Loss -5
            fill(
                3,
                Some(2),
                Some(2),
                Side::Buy,
                100,
                1,
                TradeSource::Paper,
                "fill",
            ),
            fill(
                4,
                Some(2),
                Some(2),
                Side::Sell,
                95,
                1,
                TradeSource::Paper,
                "fill",
            ),
            // Loss -3 (consecutive)
            fill(
                5,
                Some(3),
                Some(3),
                Side::Buy,
                100,
                1,
                TradeSource::Paper,
                "fill",
            ),
            fill(
                6,
                Some(3),
                Some(3),
                Side::Sell,
                97,
                1,
                TradeSource::Paper,
                "fill",
            ),
            // Win +2
            fill(
                7,
                Some(4),
                Some(4),
                Side::Buy,
                100,
                1,
                TradeSource::Paper,
                "fill",
            ),
            fill(
                8,
                Some(4),
                Some(4),
                Side::Sell,
                102,
                1,
                TradeSource::Paper,
                "fill",
            ),
        ];
        for t in trades {
            write(&mut journal, JournalEvent::TradeEvent(t));
        }

        let conn = read_only_conn(&journal);
        let q = JournalQuery::new(&conn);
        let r = q.paper_readiness_report().unwrap();
        assert_eq!(r.total_trades, 4);
        // 10 - 5 - 3 + 2 = +4
        assert_eq!(r.net_pnl.raw(), 4);
        assert!(r.positive_expectancy);
        // Wins: 2/4
        assert!((r.win_rate - 0.5).abs() < 1e-12);
        // Profit factor: (10 + 2) / (5 + 3) = 12/8 = 1.5
        assert!((r.profit_factor - 1.5).abs() < 1e-12);
        // Max drawdown: peak 10 (after first win), trough +2 (after both
        // losses), drawdown = 8.
        assert_eq!(r.max_drawdown.raw(), 8);
        // Two consecutive losses.
        assert_eq!(r.consecutive_loss_max, 2);
    }

    /// Task 6.2 — meets_minimum_threshold gating works as documented.
    #[test]
    fn readiness_minimum_threshold_predicate() {
        let r = ReadinessReport {
            total_trades: 500,
            ..ReadinessReport::empty()
        };
        assert!(r.meets_minimum_threshold(500));
        assert!(r.meets_minimum_threshold(100));
        assert!(!r.meets_minimum_threshold(501));
        let zero = ReadinessReport::empty();
        assert!(!zero.meets_minimum_threshold(1));
    }

    /// Task 7.8 — the readiness report scales to a 500+ trade dataset
    /// without panicking and computes finite values.
    #[test]
    fn readiness_500_trades_completes_with_finite_values() {
        let (mut journal, _dir) = open_journal();
        // Alternating tiny win/loss pattern. 500 round trips = 1000 fill rows.
        for i in 0..500u64 {
            let entry_ts = i * 10;
            let exit_ts = entry_ts + 1;
            let entry_price: i64 = 100;
            let exit_price: i64 = if i.is_multiple_of(2) { 101 } else { 99 };
            write(
                &mut journal,
                JournalEvent::TradeEvent(fill(
                    entry_ts,
                    Some(i + 1),
                    Some(i + 1),
                    Side::Buy,
                    entry_price,
                    1,
                    TradeSource::Paper,
                    "fill",
                )),
            );
            write(
                &mut journal,
                JournalEvent::TradeEvent(fill(
                    exit_ts,
                    Some(i + 1),
                    Some(i + 1),
                    Side::Sell,
                    exit_price,
                    1,
                    TradeSource::Paper,
                    "fill",
                )),
            );
        }

        let conn = read_only_conn(&journal);
        let q = JournalQuery::new(&conn);
        let r = q.paper_readiness_report().unwrap();
        assert_eq!(r.total_trades, 500);
        assert!(r.meets_minimum_threshold(500));
        assert!(r.win_rate.is_finite());
        assert!(r.profit_factor.is_finite() || r.profit_factor.is_infinite());
        assert!(r.sharpe_estimate.is_finite());
        // Net = 250 wins of +1 and 250 losses of -1 = 0
        assert_eq!(r.net_pnl.raw(), 0);
    }

    /// Task 7.7 — every TradeRecord field round-trips through SQLite without
    /// data loss (timestamp, side, price_qt, decision_id, order_id, kind, source).
    #[test]
    fn trade_record_round_trips_all_fields() {
        let (mut journal, _dir) = open_journal();
        let original = fill(
            1_700_000_000_123_456_789,
            Some(42),
            Some(7),
            Side::Sell,
            i64::from(17929),
            3,
            TradeSource::Paper,
            "tp_fill",
        );
        write(&mut journal, JournalEvent::TradeEvent(original.clone()));

        let conn = read_only_conn(&journal);
        let q = JournalQuery::new(&conn);
        let trades = q.trades_by_source(TradeSource::Paper).unwrap();
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.timestamp.as_nanos(), 1_700_000_000_123_456_789);
        assert_eq!(t.decision_id, Some(42));
        assert_eq!(t.order_id, Some(7));
        assert_eq!(t.symbol_id, 1);
        assert_eq!(t.side, Side::Sell);
        assert_eq!(t.price.raw(), 17929);
        assert_eq!(t.size, 3);
        assert_eq!(t.kind, "tp_fill");
        assert_eq!(t.source, TradeSource::Paper);
    }

    /// Task 7.1 / 7.2 / 7.3 — JournalSender::with_source actually rewrites
    /// the source tag end-to-end through the writer.
    #[test]
    fn sender_source_override_rewrites_records() {
        let (mut journal, _dir) = open_journal();
        let (paper_sender, paper_receiver) = EventJournal::channel_with_source(TradeSource::Paper);
        let (live_sender, live_receiver) = EventJournal::channel();
        let (replay_sender, replay_receiver) =
            EventJournal::channel_with_source(TradeSource::Replay);

        // Build records with default (Live) source and let the sender stamp.
        let r = || {
            JournalEvent::TradeEvent(TradeEventRecord {
                timestamp: UnixNanos::new(1),
                decision_id: Some(1),
                order_id: Some(1),
                symbol_id: 1,
                side: Side::Buy,
                price: FixedPrice::new(100),
                size: 1,
                kind: "fill".into(),
                source: TradeSource::Live,
            })
        };

        assert!(paper_sender.send(r()));
        assert!(live_sender.send(r()));
        assert!(replay_sender.send(r()));

        // Pull each event back out and write it through the journal so the
        // SELECT can confirm what source landed in the column.
        for rx in [&paper_receiver, &live_receiver, &replay_receiver] {
            let ev = rx.try_recv_for_test().unwrap();
            journal.write_event(&ev).unwrap();
        }

        let conn = read_only_conn(&journal);
        let q = JournalQuery::new(&conn);
        assert_eq!(q.trade_count(TradeSource::Paper).unwrap(), 1);
        assert_eq!(q.trade_count(TradeSource::Live).unwrap(), 1);
        assert_eq!(q.trade_count(TradeSource::Replay).unwrap(), 1);
    }
}
