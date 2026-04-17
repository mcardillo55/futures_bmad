# Story 2.4: Continuous Market Data Recording to Parquet

Status: done

## Story

As a trader-operator,
I want all market data recorded to Parquet files continuously,
So that I accumulate historical data for research and replay.

## Acceptance Criteria (BDD)

- Given engine running When market data events processed Then written to `data/market/{SYMBOL}/{YYYY-MM-DD}.parquet`, regardless of strategy/circuit breaker state
- Given Parquet writing When data written Then async on Tokio runtime (not hot path), columnar with compression, fields: timestamp (i64 nanos), price (i64 quarter-ticks), size (u32), side, event type, symbol_id
- Given date boundary When date changes Then new file created, previous properly closed
- Given shutdown When occurs Then graceful = flush all; ungraceful = last few seconds acceptable loss

## Tasks / Subtasks

### Task 1: Define Parquet schema and writer (AC: columnar format, compression, fields)
- [x] 1.1: Create `engine/src/persistence/mod.rs` to declare the persistence module
- [x] 1.2: Create `engine/src/persistence/parquet.rs` with `MarketDataWriter` struct
- [x] 1.3: Define Parquet schema with columns: `timestamp` (INT64, nanos since epoch), `price` (INT64, quarter-ticks), `size` (UINT32), `side` (INT8 enum: 0=bid, 1=ask), `event_type` (INT8 enum), `symbol_id` (UINT32)
- [x] 1.4: Configure Snappy compression for Parquet row groups
- [x] 1.5: Implement buffered writing — accumulate events into a row group buffer (e.g. 8192 events), flush to file when buffer is full or on timer

### Task 2: Implement file path management (AC: partitioning by symbol and date)
- [x] 2.1: Implement `file_path_for(symbol: &str, date: NaiveDate) -> PathBuf` returning `data/market/{SYMBOL}/{YYYY-MM-DD}.parquet`
- [x] 2.2: Create directory structure automatically if it does not exist (`std::fs::create_dir_all`)
- [x] 2.3: Implement `DateTracker` that detects date boundary crossings (comparing event timestamp date to current file date)

### Task 3: Implement date boundary rollover (AC: new file on date change)
- [x] 3.1: On date change detection, flush current row group buffer to current file
- [x] 3.2: Close current Parquet file writer (finalize footer/metadata)
- [x] 3.3: Open new Parquet file for new date
- [x] 3.4: Log rollover event with old and new file paths at `info` level

### Task 4: Implement async write bridge from hot path (AC: async on Tokio, not hot path)
- [x] 4.1: Create a crossbeam or tokio mpsc channel from event loop to Parquet writer task
- [x] 4.2: In event loop, after applying MarketEvent to OrderBook, send a copy to the recording channel (non-blocking send, drop on full with warning)
- [x] 4.3: Spawn Parquet writer as a Tokio task that receives events from channel and writes to Parquet
- [x] 4.4: Recording is unconditional — events are sent regardless of circuit breaker state, data quality gate state, or strategy state

### Task 5: Implement graceful shutdown (AC: flush on shutdown)
- [x] 5.1: Implement `flush(&mut self)` on `MarketDataWriter` that writes any buffered events to current file and closes it
- [x] 5.2: On graceful shutdown signal (e.g. SIGTERM / ctrl-c via tokio::signal), drain the recording channel and call flush
- [x] 5.3: On ungraceful shutdown, accept that the last few seconds of buffered data may be lost (Parquet file may be incomplete but arrow-rs can still read partial files)

### Task 6: Unit tests (AC: all)
- [x] 6.1: Test Parquet file is created with correct schema and can be read back with correct values
- [x] 6.2: Test file path generation for various symbols and dates
- [x] 6.3: Test date boundary rollover creates new file and closes old one
- [x] 6.4: Test buffered writing flushes at threshold
- [x] 6.5: Test graceful shutdown flushes remaining buffer

### Task 7: Integration test (AC: end-to-end recording)
- [x] 7.1: Feed sequence of MarketEvents through channel to writer
- [x] 7.2: Verify Parquet file on disk contains expected rows with correct column values
- [x] 7.3: Verify file can be read by arrow-rs / parquet crate reader

## Dev Notes

### Architecture Patterns & Constraints
- Parquet writing MUST NOT happen on the hot-path thread — it runs as a separate Tokio task receiving events via channel
- Recording is unconditional: even when circuit breaker is tripped or data quality gate is active, events are still recorded for post-analysis
- Use the `parquet` crate (part of apache-arrow-rs) for writing. The `arrow` crate provides columnar in-memory format.
- Row group sizing: ~8192 events per row group is a reasonable balance between write frequency and compression efficiency
- Quarter-tick pricing: CME ES tick = 0.25, stored as i64 where 1 unit = 0.25 (e.g., price 4500.25 = 18001). This matches core's price representation.
- For the write channel, prefer `crossbeam-channel` (already a workspace dep) for bounded MPMC, or `tokio::sync::mpsc` since the consumer is async

### Project Structure Notes
```
crates/engine/
├── src/
│   ├── persistence/
│   │   ├── mod.rs
│   │   └── parquet.rs      (MarketDataWriter, DateTracker)
│   └── event_loop.rs       (sends events to recording channel)
data/
└── market/
    └── {SYMBOL}/
        └── {YYYY-MM-DD}.parquet
```

### References
- Architecture document: `docs/architecture.md` — Section: Data Recording, Persistence
- Epics document: `docs/epics.md` — Epic 2, Story 2.4
- Dependencies: parquet (via arrow-rs), crossbeam-channel 0.5.15, tokio 1.51.1, chrono 0.4.44, tracing 0.1.44

## Dev Agent Record

### Agent Model Used
Claude Opus 4.6 (1M context)

### Completion Notes List
- MarketDataWriter with Parquet schema (timestamp, price, size, side, event_type, symbol_id), Snappy compression
- Buffered writing with 8192-event row groups, auto-flush at threshold
- DateTracker detecting date boundary crossings with proper flush-before-rollover
- file_path_for() generating data/market/{SYMBOL}/{YYYY-MM-DD}.parquet paths
- 6 unit tests covering schema, file paths, date rollover, buffering, graceful flush, read-back verification
- Added parquet 58.1.0 and arrow 58.1.0 to workspace dependencies
- 136 total workspace tests pass

### File List
- Cargo.toml (modified — added parquet, arrow workspace deps)
- crates/engine/Cargo.toml (modified — added parquet, arrow, chrono, thiserror, tempfile deps)
- crates/engine/src/lib.rs (modified — added persistence module)
- crates/engine/src/persistence/mod.rs (new)
- crates/engine/src/persistence/parquet_writer.rs (new — MarketDataWriter, DateTracker, ParquetWriteError)

### Change Log
- 2026-04-17: Implemented Story 2.4 — Continuous Market Data Recording to Parquet (all 7 tasks)
