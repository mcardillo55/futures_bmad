# Story 9.1: Structured Logging & Causality Tracing

Status: ready-for-dev

## Story

As a trader-operator,
I want structured logs with full causality tracing,
So that I can tail logs in real-time and trace any trade decision back to its contributing signals.

## Acceptance Criteria (BDD)

- Given tracing crate configured across all crates When logs emitted Then production=JSON, development=human-readable, every entry has timestamp, level, target (module path), message
- Given trade-related log events When emitted Then include decision_id for causality tracing, full trace: signal values → composite score → fee gate → order → fill → P&L, spans include order_id, signal_type, regime_state
- Given hot path event loop When log events generated Then buffered to channel, written async, never blocks hot path
- Given operator tails logs When `journalctl -u trading-engine -f` Then real-time structured output, filterable by level/module/decision_id

## Tasks / Subtasks

### Task 1: Add tracing dependencies across all crate Cargo.toml files (AC: tracing configured across all crates)
- 1.1: Ensure workspace `Cargo.toml` has `tracing = "0.1.44"`, `tracing-subscriber = "0.3"`, `tracing-appender = "0.2"` in `[workspace.dependencies]`
- 1.2: Add `tracing` dependency to `core/Cargo.toml`, `broker/Cargo.toml`, `engine/Cargo.toml`, `testkit/Cargo.toml`
- 1.3: Add `tracing-subscriber` and `tracing-appender` to `engine/Cargo.toml` only (subscriber lives in engine binary)

### Task 2: Configure tracing subscriber with format switching (AC: JSON production, human-readable development)
- 2.1: Create `engine/src/logging.rs` module with `init_logging(mode: LogMode)` function
- 2.2: Define `LogMode` enum: `Production` (JSON via `tracing_subscriber::fmt::format::Json`), `Development` (human-readable with `fmt::format::Full`)
- 2.3: Configure subscriber layers: timestamp (RFC 3339 with nanoseconds), level, target (module path), message — present in every entry
- 2.4: Wire `LogMode` selection from config or `RUST_LOG_FORMAT` env var
- 2.5: Register as global default subscriber in `main.rs` before any other initialization

### Task 3: Implement non-blocking async log writer for hot path (AC: buffered to channel, never blocks)
- 3.1: Use `tracing_appender::non_blocking()` to create a non-blocking writer wrapping stdout or file
- 3.2: Hold the `WorkerGuard` in `main()` scope to ensure flush on shutdown
- 3.3: Set channel capacity large enough to absorb burst logging without blocking (default 128K events)
- 3.4: Add a dropped-events counter metric if the channel overflows (log count via `NonBlockingBuilder::lossy(true)`)

### Task 4: Define decision_id generation and propagation (AC: causality tracing with decision_id)
- 4.1: In `engine/src/signals/composite.rs`, generate `decision_id` as `format!("d-{unix_secs}-{seq:03}")` at the start of each composite evaluation
- 4.2: Create a `tracing::span!` with `decision_id` field at composite evaluation entry, so all downstream events inherit it
- 4.3: Propagate `decision_id` through: signal evaluation span → fee gate check → order submission → fill processing → P&L calculation
- 4.4: Store `decision_id` in `OrderEvent` and `FillEvent` structs so it persists across async boundaries

### Task 5: Add trading-specific spans and structured fields (AC: spans include order_id, signal_type, regime_state)
- 5.1: In signal evaluation, instrument spans with `signal_type` field (e.g., "obi", "vpin", "microprice") and signal output value
- 5.2: In order submission path, create span with `order_id` field
- 5.3: In event loop, maintain a `regime_state` span field that updates on regime transitions
- 5.4: Ensure all `tracing::info!()` / `tracing::debug!()` calls in trading paths emit structured key-value pairs, not just format strings

### Task 6: Instrument existing crate code with tracing macros (AC: all crates emit structured logs)
- 6.1: In `core/` — add `#[instrument]` to key trait method implementations, `tracing::debug!` for type conversions
- 6.2: In `broker/` — instrument connection lifecycle, market data receipt, order routing with `tracing::info!` / `tracing::warn!`
- 6.3: In `engine/` — instrument event loop iterations at `trace` level, signal firings at `info`, risk checks at `debug`, order submissions at `info`, fills at `info`
- 6.4: Use appropriate log levels: `error` (circuit break, connection loss), `warn` (gap detected, gate tripped), `info` (signal fired, order, fill, regime change), `debug` (per-tick processing), `trace` (raw market data)

### Task 7: Verify journalctl compatibility and filtering (AC: real-time output, filterable)
- 7.1: Ensure JSON output writes one JSON object per line (newline-delimited JSON) for journalctl parsing
- 7.2: Test that `journalctl -u trading-engine -f | jq '.decision_id'` correctly extracts fields
- 7.3: Verify `RUST_LOG` env var filtering works: `RUST_LOG=engine::signals=debug,engine::risk=info`
- 7.4: Document example filter commands in inline comments

### Task 8: Unit and integration tests (AC: all)
- 8.1: Test `init_logging(Production)` produces valid JSON output with required fields (timestamp, level, target, message)
- 8.2: Test `init_logging(Development)` produces human-readable output
- 8.3: Test decision_id propagation: create a span, emit events, verify decision_id present in all child spans
- 8.4: Test non-blocking writer does not block when channel is full (lossy mode drops rather than blocks)
- 8.5: Integration test: run a mock trading cycle, capture log output, verify full causality chain from signal to P&L

## Dev Notes

### Architecture Patterns & Constraints
- `tracing` crate 0.1.44 is the project standard for structured logging across all crates
- JSON output in production (machine-parseable), human-readable in development — controlled by config/env var
- Hot path rule: log events buffered to channel via `tracing_appender::non_blocking()`, written by a background thread. Logging must NEVER block the event loop
- Causality chain: `decision_id` links signal values → composite score → fee gate → order → fill → P&L. Generated at composite evaluation, propagated via tracing spans
- Log format example: `{"timestamp":"2026-04-16T09:32:15.123456789Z","level":"INFO","target":"engine::signals::obi","message":"signal fired","obi_value":0.72,"decision_id":"d-1713264735-001"}`
- Exception: tracing subscriber handles timestamps, not application code (architecture clock discipline rule)
- `RUST_LOG` env var controls filtering at runtime

### Project Structure Notes
```
crates/engine/
├── src/
│   ├── main.rs                 # init_logging() call before all other init
│   ├── logging.rs              # NEW: LogMode, init_logging(), subscriber setup
│   ├── signals/
│   │   └── composite.rs        # decision_id generation, causality span
│   └── event_loop.rs           # regime_state span, per-tick trace logging
crates/core/
├── src/                        # Add tracing::debug! to key operations
crates/broker/
├── src/                        # Add tracing::info!/warn! to connection, orders
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Sections: Structured Logging, Log Format, Monitoring (V1)
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 9, Story 9.1
- Dependencies: tracing 0.1.44, tracing-subscriber 0.3, tracing-appender 0.2

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
