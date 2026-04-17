# Story 2.4: Continuous Market Data Recording to Parquet

Status: ready-for-dev

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
- 1.1: Create `engine/src/persistence/mod.rs` to declare the persistence module
- 1.2: Create `engine/src/persistence/parquet.rs` with `MarketDataWriter` struct
- 1.3: Define Parquet schema with columns: `timestamp` (INT64, nanos since epoch), `price` (INT64, quarter-ticks), `size` (UINT32), `side` (INT8 enum: 0=bid, 1=ask), `event_type` (INT8 enum), `symbol_id` (UINT32)
- 1.4: Configure Snappy compression for Parquet row groups
- 1.5: Implement buffered writing — accumulate events into a row group buffer (e.g. 8192 events), flush to file when buffer is full or on timer

### Task 2: Implement file path management (AC: partitioning by symbol and date)
- 2.1: Implement `file_path_for(symbol: &str, date: NaiveDate) -> PathBuf` returning `data/market/{SYMBOL}/{YYYY-MM-DD}.parquet`
- 2.2: Create directory structure automatically if it does not exist (`std::fs::create_dir_all`)
- 2.3: Implement `DateTracker` that detects date boundary crossings (comparing event timestamp date to current file date)

### Task 3: Implement date boundary rollover (AC: new file on date change)
- 3.1: On date change detection, flush current row group buffer to current file
- 3.2: Close current Parquet file writer (finalize footer/metadata)
- 3.3: Open new Parquet file for new date
- 3.4: Log rollover event with old and new file paths at `info` level

### Task 4: Implement async write bridge from hot path (AC: async on Tokio, not hot path)
- 4.1: Create a crossbeam or tokio mpsc channel from event loop to Parquet writer task
- 4.2: In event loop, after applying MarketEvent to OrderBook, send a copy to the recording channel (non-blocking send, drop on full with warning)
- 4.3: Spawn Parquet writer as a Tokio task that receives events from channel and writes to Parquet
- 4.4: Recording is unconditional — events are sent regardless of circuit breaker state, data quality gate state, or strategy state

### Task 5: Implement graceful shutdown (AC: flush on shutdown)
- 5.1: Implement `flush(&mut self)` on `MarketDataWriter` that writes any buffered events to current file and closes it
- 5.2: On graceful shutdown signal (e.g. SIGTERM / ctrl-c via tokio::signal), drain the recording channel and call flush
- 5.3: On ungraceful shutdown, accept that the last few seconds of buffered data may be lost (Parquet file may be incomplete but arrow-rs can still read partial files)

### Task 6: Unit tests (AC: all)
- 6.1: Test Parquet file is created with correct schema and can be read back with correct values
- 6.2: Test file path generation for various symbols and dates
- 6.3: Test date boundary rollover creates new file and closes old one
- 6.4: Test buffered writing flushes at threshold
- 6.5: Test graceful shutdown flushes remaining buffer

### Task 7: Integration test (AC: end-to-end recording)
- 7.1: Feed sequence of MarketEvents through channel to writer
- 7.2: Verify Parquet file on disk contains expected rows with correct column values
- 7.3: Verify file can be read by arrow-rs / parquet crate reader

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
### Debug Log References
### Completion Notes List
### File List
