# Story 2.5: Historical Parquet Ingestion

Status: ready-for-dev

## Story

As a trader-operator,
I want to load historical market data from Parquet files,
So that I can feed it through the replay engine for strategy validation.

## Acceptance Criteria (BDD)

- Given Parquet file (Databento or own recording) When data source initialized Then produces sequential MarketEvent stream, ordered by timestamp, Databento format correctly mapped
- Given historical data loaded When fed into event loop Then uses same SPSC buffer path as live data
- Given multiple Parquet files for date range When multi-day replay requested Then files loaded chronologically, correct timestamp ordering across boundaries
- Given corrupt Parquet file When bad records encountered Then skipped with warning, ingestion continues, summary reported

## Tasks / Subtasks

### Task 1: Define DataSource trait (AC: sequential MarketEvent stream)
- 1.1: Create `engine/src/data/mod.rs` to declare the data module
- 1.2: Create `engine/src/data/data_source.rs` with `DataSource` trait: `fn next_event(&mut self) -> Option<MarketEvent>`, `fn reset(&mut self)`, `fn event_count(&self) -> usize`
- 1.3: Trait must be generic enough for both own-format Parquet and Databento-format Parquet

### Task 2: Implement own-format Parquet reader (AC: sequential stream, timestamp ordered)
- 2.1: Create `engine/src/data/parquet_source.rs` with `ParquetDataSource` implementing `DataSource`
- 2.2: Read Parquet file using `parquet` crate, mapping columns to MarketEvent fields (timestamp i64 nanos, price i64 quarter-ticks, size u32, side, event_type, symbol_id)
- 2.3: Validate timestamp ordering — assert events are monotonically non-decreasing, warn if not
- 2.4: Load file lazily via row-group iteration to avoid loading entire file into memory at once

### Task 3: Implement Databento format mapper (AC: Databento format correctly mapped)
- 3.1: Create `engine/src/data/databento_source.rs` with `DatabentDataSource` implementing `DataSource`
- 3.2: Use `databento` crate (=0.40.0) to read Databento Parquet/DBN files
- 3.3: Map Databento MBO/MBP fields to MarketEvent: timestamp, price (convert from Databento fixed-point to quarter-ticks), size, side, event_type
- 3.4: Handle Databento-specific fields (action, flags, channel_id) — map relevant ones, discard others

### Task 4: Implement multi-file chronological loading (AC: date range, cross-boundary ordering)
- 4.1: Create `engine/src/data/multi_day_source.rs` with `MultiDayDataSource` implementing `DataSource`
- 4.2: Accept a date range and symbol, discover files matching `data/market/{SYMBOL}/{YYYY-MM-DD}.parquet` pattern
- 4.3: Sort discovered files chronologically by date extracted from filename
- 4.4: Implement sequential iteration: exhaust file N before moving to file N+1
- 4.5: Validate timestamp ordering across file boundaries — last event of file N should be <= first event of file N+1, warn if not

### Task 5: Implement corrupt data handling (AC: skip bad records, continue, report)
- 5.1: Wrap row-group and record reading in error handling — on corrupt/unreadable row group, log warning with file path and row group index, skip to next
- 5.2: On individual record parse failure, log at `warn` level, increment skip counter, continue
- 5.3: At end of file processing, log summary: total records read, records skipped, files processed
- 5.4: If entire file is unreadable, log error, skip file, continue with next file in sequence

### Task 6: Wire to SPSC buffer for replay (AC: same path as live data)
- 6.1: Create `engine/src/replay/mod.rs` and `engine/src/replay/data_source.rs` as replay coordination module
- 6.2: Implement replay driver that reads from `DataSource` and pushes to `MarketEventProducer` (same SPSC as live)
- 6.3: Use `SimClock` to advance time based on event timestamps rather than wall clock
- 6.4: Consumer side (event loop) processes events identically whether from live or replay — no code path differences

### Task 7: Unit tests (AC: all)
- 7.1: Test own-format Parquet reader produces correct MarketEvent sequence
- 7.2: Test Databento format mapping produces correct price conversion and field mapping
- 7.3: Test multi-day source iterates files in chronological order
- 7.4: Test cross-boundary timestamp ordering validation
- 7.5: Test corrupt row group is skipped, processing continues, summary logged
- 7.6: Test corrupt file is skipped, next file processed
- 7.7: Test replay driver pushes events to SPSC and SimClock advances correctly

## Dev Notes

### Architecture Patterns & Constraints
- The replay system uses the exact same SPSC buffer and event loop consumer as live data — this is critical for ensuring strategy behavior is identical in backtest and production
- SimClock (from testkit) is used during replay to control time progression. The event loop and all time-dependent components (stale detector, etc.) see SimClock time, not wall clock
- Databento price format uses fixed-point i64 with 1e-9 resolution. Conversion to quarter-ticks: `(databento_price as f64 / 1e9 / tick_size).round() as i64` where tick_size = 0.25 for ES
- Memory management: do not load entire Parquet files into memory. Use row-group-at-a-time iteration. Typical daily ES file is ~2-5 million events.
- The `DataSource` trait is synchronous (not async) since replay runs on the producer side which can be a dedicated thread, not necessarily Tokio

### Project Structure Notes
```
crates/engine/
├── src/
│   ├── data/
│   │   ├── mod.rs
│   │   ├── data_source.rs       (DataSource trait)
│   │   ├── parquet_source.rs    (own-format reader)
│   │   ├── databento_source.rs  (Databento format reader)
│   │   └── multi_day_source.rs  (chronological multi-file)
│   └── replay/
│       ├── mod.rs
│       └── data_source.rs       (replay driver, SPSC bridge)
crates/testkit/
├── src/
│   └── clock.rs                 (SimClock for replay)
```

### References
- Architecture document: `docs/architecture.md` — Section: Replay Engine, Data Sources
- Epics document: `docs/epics.md` — Epic 2, Story 2.5
- Dependencies: parquet (via arrow-rs), databento =0.40.0, chrono 0.4.44, tracing 0.1.44, rtrb 0.3.3

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
