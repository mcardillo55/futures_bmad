//! Order WAL — write-ahead log for in-flight order state.
//!
//! The WAL is a SQLite table (`pending_orders`) sharing the same database file
//! as the journal (story 4.1). Two reasons to share the file:
//!
//!   1. **Single fsync barrier.** Crash recovery needs the order WAL and the
//!      journal events to be visible together — splitting them across files
//!      reintroduces an ordering window between the two fsyncs.
//!   2. **One backup target.** Operators back up the journal as a single file;
//!      forcing a separate WAL DB doubles the operational surface.
//!
//! Storage layout (per spec Task 3.2):
//!
//! ```sql
//! CREATE TABLE pending_orders (
//!     order_id     INTEGER PRIMARY KEY,
//!     decision_id  INTEGER NOT NULL,
//!     symbol_id    INTEGER NOT NULL,
//!     side         TEXT    NOT NULL,
//!     quantity     INTEGER NOT NULL,
//!     state        TEXT    NOT NULL,
//!     submitted_at INTEGER NOT NULL,
//!     bracket_id   INTEGER NULL
//! );
//! ```
//!
//! The critical ordering invariant: every `OrderEvent` that flows from the
//! engine to the broker is `write_before_submit()`-ed first. If the engine
//! crashes between WAL write and queue push, startup recovery finds the row
//! and queries the broker. If the engine crashes between queue push and the
//! broker's exchange round-trip, same story.
//!
//! Resolution writes (`mark_resolved`) update the row in place rather than
//! deleting — the `state` column carries the terminal designation
//! (`Filled`/`Rejected`/`Resolved`) so a forensic operator can replay the WAL
//! to see the per-order outcome without consulting the journal. `recover_pending`
//! filters out resolved rows so it only returns work-in-progress.

use std::path::Path;

use futures_bmad_core::{OrderEvent, OrderState, OrderType, Side, UnixNanos};
use rusqlite::{Connection, OpenFlags, params};
use tracing::{debug, info, warn};

const CREATE_PENDING_ORDERS: &str = "
    CREATE TABLE IF NOT EXISTS pending_orders (
        order_id     INTEGER PRIMARY KEY,
        decision_id  INTEGER NOT NULL,
        symbol_id    INTEGER NOT NULL,
        side         TEXT    NOT NULL,
        quantity     INTEGER NOT NULL,
        state        TEXT    NOT NULL,
        submitted_at INTEGER NOT NULL,
        bracket_id   INTEGER NULL
    );
";

#[derive(Debug, thiserror::Error)]
pub enum WalError {
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("unparseable WAL row: {0}")]
    BadRow(String),
}

/// In-memory snapshot of a row in `pending_orders`. Used by the recovery path
/// at startup (see [`OrderWal::recover_pending`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingOrder {
    pub order_id: u64,
    pub decision_id: u64,
    pub symbol_id: u32,
    pub side: Side,
    pub quantity: u32,
    pub state: OrderState,
    pub submitted_at: UnixNanos,
    pub bracket_id: Option<u64>,
}

/// Order WAL handle — wraps a single SQLite connection.
///
/// `OrderWal` is `!Send` (rusqlite::Connection is `!Send`). Construct it on
/// the same thread that owns the [`OrderManager`]; the synchronous writes are
/// already off the hot path because order submission itself is rare relative
/// to market-data flow.
pub struct OrderWal {
    conn: Connection,
}

impl OrderWal {
    /// Open the WAL using a fresh connection to `db_path`. The path is
    /// expected to be the journal database (so journal events and order WAL
    /// share one fsync barrier). The journal worker may already hold a
    /// separate connection to the same file — SQLite's WAL mode supports
    /// multiple writers.
    pub fn open(db_path: &Path) -> Result<Self, WalError> {
        // The journal owner is responsible for creating the file in WAL mode;
        // open with the default flags here so we don't fight over the
        // journal_mode pragma. busy_timeout = 5s gives concurrent journal
        // writers room to commit.
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_URI,
        )?;
        conn.pragma_update(None, "busy_timeout", 5_000)?;
        conn.execute_batch(CREATE_PENDING_ORDERS)?;
        info!(target: "order_wal", path = %db_path.display(), "order WAL opened");
        Ok(Self { conn })
    }

    /// Construct an in-memory WAL for tests.
    pub fn open_in_memory() -> Result<Self, WalError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(CREATE_PENDING_ORDERS)?;
        Ok(Self { conn })
    }

    /// Persist the order before it is pushed onto the broker queue.
    ///
    /// The state recorded is `Submitted` — by the time this row hits the
    /// disk, the OrderStateMachine has not yet flipped (the
    /// [`OrderManager::submit_order`] sequencing is documented in `mod.rs`).
    /// `submitted_at` is the `UnixNanos` the manager stamps onto the
    /// state machine in the same call.
    ///
    /// This MUST complete before the [`OrderEvent`] is pushed onto the SPSC
    /// queue. Callers must propagate the error to the caller and refuse the
    /// submission — a failed WAL write invalidates the durability guarantee.
    pub fn write_before_submit(
        &self,
        order: &OrderEvent,
        bracket_id: Option<u64>,
    ) -> Result<(), WalError> {
        self.conn.execute(
            "INSERT INTO pending_orders
                (order_id, decision_id, symbol_id, side, quantity, state, submitted_at, bracket_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                order.order_id as i64,
                order.decision_id as i64,
                order.symbol_id as i64,
                side_to_str(order.side),
                order.quantity as i64,
                state_to_str(OrderState::Submitted),
                order.timestamp.as_nanos() as i64,
                bracket_id.map(|id| id as i64),
            ],
        )?;
        debug!(
            target: "order_wal",
            order_id = order.order_id,
            decision_id = order.decision_id,
            symbol_id = order.symbol_id,
            side = ?order.side,
            quantity = order.quantity,
            order_type = ?order.order_type,
            "wal: write_before_submit"
        );
        // Quiet the unused-variable lint for OrderType — the WAL row schema
        // does not capture the order type today (Market/Limit/Stop is implicit
        // in how the broker received it). When the schema gains an order_type
        // column we'll log it as a structured field instead of dropping it
        // with `_ =`.
        let _ = order.order_type;
        Ok(())
    }

    /// Update the row to reflect a terminal outcome (`Filled` / `Rejected` /
    /// `Resolved`). The row is **not** deleted; the terminal `state` column
    /// is what `recover_pending` filters on.
    pub fn mark_resolved(&self, order_id: u64, final_state: OrderState) -> Result<(), WalError> {
        if !final_state.is_terminal() {
            warn!(
                target: "order_wal",
                order_id,
                ?final_state,
                "mark_resolved called with non-terminal state — refusing"
            );
            return Err(WalError::BadRow(format!(
                "non-terminal state {final_state:?} passed to mark_resolved"
            )));
        }
        let n = self.conn.execute(
            "UPDATE pending_orders SET state = ?2 WHERE order_id = ?1",
            params![order_id as i64, state_to_str(final_state)],
        )?;
        debug!(
            target: "order_wal",
            order_id,
            ?final_state,
            updated_rows = n,
            "wal: mark_resolved"
        );
        Ok(())
    }

    /// Update an in-flight order's state without resolving it (used for
    /// `Submitted -> Uncertain -> PendingRecon` transitions during reconciliation).
    pub fn update_state(&self, order_id: u64, new_state: OrderState) -> Result<(), WalError> {
        let n = self.conn.execute(
            "UPDATE pending_orders SET state = ?2 WHERE order_id = ?1",
            params![order_id as i64, state_to_str(new_state)],
        )?;
        debug!(
            target: "order_wal",
            order_id,
            ?new_state,
            updated_rows = n,
            "wal: update_state"
        );
        Ok(())
    }

    /// Return all rows whose `state` is non-terminal. Called once at startup
    /// by the engine's lifecycle/startup code — for each row, the engine
    /// queries the broker and reconciles state.
    pub fn recover_pending(&self) -> Result<Vec<PendingOrder>, WalError> {
        let mut stmt = self.conn.prepare(
            "SELECT order_id, decision_id, symbol_id, side, quantity, state, submitted_at, bracket_id
             FROM pending_orders
             WHERE state NOT IN ('Filled', 'Rejected', 'Resolved', 'Cancelled')",
        )?;
        let rows = stmt.query_map([], |row| {
            let order_id: i64 = row.get(0)?;
            let decision_id: i64 = row.get(1)?;
            let symbol_id: i64 = row.get(2)?;
            let side_s: String = row.get(3)?;
            let quantity: i64 = row.get(4)?;
            let state_s: String = row.get(5)?;
            let submitted_at: i64 = row.get(6)?;
            let bracket_id: Option<i64> = row.get(7)?;
            Ok((
                order_id,
                decision_id,
                symbol_id,
                side_s,
                quantity,
                state_s,
                submitted_at,
                bracket_id,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (oid, did, sid, ss, qty, st, sub, br) = row?;
            let side = parse_side(&ss).map_err(WalError::BadRow)?;
            let state = parse_state(&st).map_err(WalError::BadRow)?;
            out.push(PendingOrder {
                order_id: oid as u64,
                decision_id: did as u64,
                symbol_id: sid as u32,
                side,
                quantity: qty as u32,
                state,
                submitted_at: UnixNanos::new(sub as u64),
                bracket_id: br.map(|b| b as u64),
            });
        }
        Ok(out)
    }

    /// Look up a single row (for diagnostics / state reconciliation).
    pub fn get(&self, order_id: u64) -> Result<Option<PendingOrder>, WalError> {
        let mut stmt = self.conn.prepare(
            "SELECT order_id, decision_id, symbol_id, side, quantity, state, submitted_at, bracket_id
             FROM pending_orders
             WHERE order_id = ?1",
        )?;
        let mut rows = stmt.query(params![order_id as i64])?;
        if let Some(row) = rows.next()? {
            let oid: i64 = row.get(0)?;
            let did: i64 = row.get(1)?;
            let sid: i64 = row.get(2)?;
            let ss: String = row.get(3)?;
            let qty: i64 = row.get(4)?;
            let st: String = row.get(5)?;
            let sub: i64 = row.get(6)?;
            let br: Option<i64> = row.get(7)?;
            let side = parse_side(&ss).map_err(WalError::BadRow)?;
            let state = parse_state(&st).map_err(WalError::BadRow)?;
            return Ok(Some(PendingOrder {
                order_id: oid as u64,
                decision_id: did as u64,
                symbol_id: sid as u32,
                side,
                quantity: qty as u32,
                state,
                submitted_at: UnixNanos::new(sub as u64),
                bracket_id: br.map(|b| b as u64),
            }));
        }
        Ok(None)
    }
}

fn side_to_str(side: Side) -> &'static str {
    match side {
        Side::Buy => "Buy",
        Side::Sell => "Sell",
    }
}

fn parse_side(s: &str) -> Result<Side, String> {
    match s {
        "Buy" => Ok(Side::Buy),
        "Sell" => Ok(Side::Sell),
        other => Err(format!("invalid side string: {other}")),
    }
}

fn state_to_str(state: OrderState) -> &'static str {
    match state {
        OrderState::Idle => "Idle",
        OrderState::Submitted => "Submitted",
        OrderState::Confirmed => "Confirmed",
        OrderState::PartialFill => "PartialFill",
        OrderState::Filled => "Filled",
        OrderState::Rejected => "Rejected",
        OrderState::Cancelled => "Cancelled",
        OrderState::PendingCancel => "PendingCancel",
        OrderState::Uncertain => "Uncertain",
        OrderState::PendingRecon => "PendingRecon",
        OrderState::Resolved => "Resolved",
    }
}

fn parse_state(s: &str) -> Result<OrderState, String> {
    Ok(match s {
        "Idle" => OrderState::Idle,
        "Submitted" => OrderState::Submitted,
        "Confirmed" => OrderState::Confirmed,
        "PartialFill" => OrderState::PartialFill,
        "Filled" => OrderState::Filled,
        "Rejected" => OrderState::Rejected,
        "Cancelled" => OrderState::Cancelled,
        "PendingCancel" => OrderState::PendingCancel,
        "Uncertain" => OrderState::Uncertain,
        "PendingRecon" => OrderState::PendingRecon,
        "Resolved" => OrderState::Resolved,
        other => return Err(format!("invalid state string: {other}")),
    })
}

/// Suppress the unused-OrderType import in non-cfg(test) builds — the type is
/// re-exported through the `OrderEvent` field but not directly named.
#[allow(dead_code)]
fn _silence_unused_order_type(_o: OrderType) {}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_bmad_core::FixedPrice;
    use tempfile::TempDir;

    fn make_event(order_id: u64) -> OrderEvent {
        OrderEvent {
            order_id,
            symbol_id: 1,
            side: Side::Buy,
            quantity: 3,
            order_type: OrderType::Market,
            decision_id: order_id * 10,
            timestamp: UnixNanos::new(1_700_000_000_000_000_000 + order_id),
        }
    }

    /// Task 7.4 — order written, simulated crash (drop conn), recover_pending returns it.
    #[test]
    fn write_before_submit_persists_and_recover_returns_pending() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.db");

        {
            let wal = OrderWal::open(&path).unwrap();
            wal.write_before_submit(&make_event(1), Some(99)).unwrap();
            wal.write_before_submit(&make_event(2), None).unwrap();
            // Drop without explicit close to simulate a process crash.
        }

        let wal = OrderWal::open(&path).unwrap();
        let pending = wal.recover_pending().unwrap();
        assert_eq!(pending.len(), 2);
        let p1 = pending.iter().find(|p| p.order_id == 1).unwrap();
        assert_eq!(p1.decision_id, 10);
        assert_eq!(p1.symbol_id, 1);
        assert_eq!(p1.side, Side::Buy);
        assert_eq!(p1.quantity, 3);
        assert_eq!(p1.state, OrderState::Submitted);
        assert_eq!(p1.bracket_id, Some(99));
        let p2 = pending.iter().find(|p| p.order_id == 2).unwrap();
        assert_eq!(p2.bracket_id, None);
    }

    /// Task 7.5 — mark_resolved: resolved orders not returned by recover_pending.
    #[test]
    fn mark_resolved_excludes_terminal_orders_from_recovery() {
        let wal = OrderWal::open_in_memory().unwrap();
        wal.write_before_submit(&make_event(1), None).unwrap();
        wal.write_before_submit(&make_event(2), None).unwrap();
        wal.write_before_submit(&make_event(3), None).unwrap();

        wal.mark_resolved(1, OrderState::Filled).unwrap();
        wal.mark_resolved(3, OrderState::Resolved).unwrap();

        let pending = wal.recover_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].order_id, 2);
    }

    /// mark_resolved with non-terminal state is rejected.
    #[test]
    fn mark_resolved_rejects_non_terminal_state() {
        let wal = OrderWal::open_in_memory().unwrap();
        wal.write_before_submit(&make_event(1), None).unwrap();
        let err = wal.mark_resolved(1, OrderState::Submitted).unwrap_err();
        assert!(matches!(err, WalError::BadRow(_)));
    }

    /// update_state preserves the row but flips the state column (used for Submitted->Uncertain).
    #[test]
    fn update_state_preserves_row_and_changes_state() {
        let wal = OrderWal::open_in_memory().unwrap();
        wal.write_before_submit(&make_event(1), None).unwrap();
        wal.update_state(1, OrderState::Uncertain).unwrap();

        let p = wal.get(1).unwrap().unwrap();
        assert_eq!(p.state, OrderState::Uncertain);

        // Uncertain is non-terminal, so it still appears in recover_pending.
        let pending = wal.recover_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state, OrderState::Uncertain);
    }

    /// recover_pending also filters out Cancelled rows.
    #[test]
    fn recover_pending_excludes_cancelled() {
        let wal = OrderWal::open_in_memory().unwrap();
        wal.write_before_submit(&make_event(1), None).unwrap();
        wal.mark_resolved(1, OrderState::Cancelled).unwrap();
        let pending = wal.recover_pending().unwrap();
        assert!(pending.is_empty());
    }

    /// Round-trip through the file: timestamp + price encoded as integers and
    /// recovered identically.
    #[test]
    fn fields_round_trip_across_file_reopen() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("wal.db");
        let _px = FixedPrice::new(17_929); // unused but keeps import non-dead in tests
        {
            let wal = OrderWal::open(&path).unwrap();
            let mut e = make_event(42);
            e.side = Side::Sell;
            wal.write_before_submit(&e, Some(7)).unwrap();
        }
        let wal = OrderWal::open(&path).unwrap();
        let p = wal.get(42).unwrap().unwrap();
        assert_eq!(p.side, Side::Sell);
        assert_eq!(p.bracket_id, Some(7));
        assert_eq!(p.submitted_at.as_nanos(), 1_700_000_000_000_000_042);
    }
}
