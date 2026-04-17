# Story 9.3: Tax Reporting Data

Status: ready-for-dev

## Story

As a trader-operator,
I want trade data logged in a format suitable for Section 1256 tax reporting,
So that I can accurately report 60/40 tax treatment for CME futures.

## Acceptance Criteria (BDD)

- Given event journal with trade data When tax report query run Then produces realized P&L per contract, per day: date, symbol, side, quantity, entry price, exit price, realized P&L (dollars), supports Section 1256 60/40 calculation
- Given daily event journal export When `data/events/{YYYY-MM-DD}.parquet` generated Then trade events in format compatible with tax prep tools, schema matches journal

## Tasks / Subtasks

### Task 1: Define tax reporting data structures (AC: realized P&L per contract per day)
- 1.1: In `engine/src/persistence/` (or a new `engine/src/tax/mod.rs`), define `TaxTradeRecord` struct with fields: `date` (NaiveDate), `symbol` (String), `side` (Side), `quantity` (u32), `entry_price` (FixedPrice), `exit_price` (FixedPrice), `realized_pnl_ticks` (i64), `realized_pnl_dollars` (f64), `decision_id` (String)
- 1.2: Define `tick_value` per instrument (e.g., MES = $1.25/tick, MNQ = $0.50/tick) for dollar conversion: `realized_pnl_dollars = realized_pnl_ticks as f64 * tick_value / 4.0` (quarter-tick to dollar)
- 1.3: Add `contract_type` field marking all records as `Section1256` (CME regulated futures)

### Task 2: Implement tax report query against SQLite journal (AC: produces realized P&L per contract per day)
- 2.1: In `engine/src/persistence/journal.rs`, add `query_tax_records(date: NaiveDate) -> Vec<TaxTradeRecord>` method
- 2.2: SQL query joins fill events with their corresponding entry orders to compute realized P&L: `exit_price - entry_price` (adjusted for side)
- 2.3: Group results by date + symbol for per-contract-per-day aggregation
- 2.4: Include only closed positions (matched entry/exit pairs) — open positions at year-end use mark-to-market (Section 1256 requirement)
- 2.5: Add `query_tax_records_range(start: NaiveDate, end: NaiveDate) -> Vec<TaxTradeRecord>` for annual reporting

### Task 3: Implement year-end mark-to-market for open positions (AC: supports Section 1256 calculation)
- 3.1: Add `query_open_positions_at_date(date: NaiveDate) -> Vec<OpenPositionRecord>` to journal
- 3.2: `OpenPositionRecord` includes: `symbol`, `side`, `quantity`, `entry_price`, `mark_price` (last settlement price), `unrealized_pnl_dollars`
- 3.3: Section 1256 requires marking open positions to market at year-end as if sold — include this in annual tax data

### Task 4: Add tax fields to daily Parquet export (AC: Parquet schema matches journal, compatible with tax prep)
- 4.1: In `engine/src/persistence/parquet.rs`, extend the daily event export to include `TaxTradeRecord` columns
- 4.2: Parquet schema for tax records: `date` (Date32), `symbol` (Utf8), `side` (Utf8), `quantity` (UInt32), `entry_price_ticks` (Int64), `exit_price_ticks` (Int64), `realized_pnl_ticks` (Int64), `realized_pnl_dollars` (Float64), `decision_id` (Utf8), `contract_type` (Utf8)
- 4.3: Daily export path: `data/events/{YYYY-MM-DD}.parquet` — ensure tax trade records are included alongside other event types
- 4.4: Verify Parquet schema consistency between journal query output and file export

### Task 5: Implement CSV export for tax prep tool compatibility (AC: compatible with tax preparation tools)
- 5.1: Add `export_tax_csv(year: i32, path: &Path) -> Result<()>` function
- 5.2: CSV columns: Date, Symbol, Side, Quantity, Entry Price, Exit Price, Realized P&L, Contract Type
- 5.3: Prices in CSV as dollar values (human-readable), not quarter-ticks
- 5.4: Include summary row with total realized P&L for the year
- 5.5: Add 60/40 split calculation: `long_term = total_pnl * 0.60`, `short_term = total_pnl * 0.40`

### Task 6: Unit and integration tests (AC: all)
- 6.1: Test `TaxTradeRecord` dollar P&L calculation: given entry=20000 ticks, exit=20004 ticks, MES tick_value=$1.25, verify P&L = $5.00
- 6.2: Test journal query returns correct records for a given date range
- 6.3: Test Parquet export includes tax fields with correct schema
- 6.4: Test CSV export produces valid format with correct dollar amounts
- 6.5: Test 60/40 split calculation accuracy
- 6.6: Test mark-to-market for open positions at year-end date

## Dev Notes

### Architecture Patterns & Constraints
- Section 1256: All CME regulated futures qualify for 60% long-term / 40% short-term capital gains treatment regardless of holding period
- Mark-to-market: open positions at year-end are treated as if sold at fair market value (last settlement price)
- P&L calculation: `realized_pnl_ticks = (exit_price.0 - entry_price.0) * quantity` for long, reversed for short. Dollar conversion uses instrument-specific tick value
- FixedPrice stores quarter-ticks: divide by 4 to get ticks, multiply by tick_value for dollars
- Data source is the SQLite event journal (WAL mode) — no separate tax database
- Daily Parquet export at `data/events/{YYYY-MM-DD}.parquet` is the existing export mechanism from Story 4.1
- Tax data is read-only queries — no hot path impact

### Project Structure Notes
```
crates/engine/
├── src/
│   ├── persistence/
│   │   ├── journal.rs          # Add tax report query methods
│   │   └── parquet.rs          # Extend daily export with tax columns
│   └── tax/                    # NEW (optional — could live in persistence/)
│       └── mod.rs              # TaxTradeRecord, export_tax_csv, 60/40 calculation
data/
├── events/
│   └── {YYYY-MM-DD}.parquet    # Daily export includes tax trade records
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Sections: Data Persistence & Logging
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 9, Story 9.3
- Story 4.1: SQLite Event Journal — provides the journal data source
- IRS Section 1256: 26 U.S.C. Section 1256 — regulated futures contracts, 60/40 treatment
- Dependencies: rusqlite 0.38.0, chrono 0.4.44, arrow/parquet crates (existing)

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
