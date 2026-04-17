# Story 9.5: Watchdog Heartbeat Interface

Status: ready-for-dev

## Story

As a trader-operator,
I want the engine to emit heartbeats for an external watchdog,
So that when I deploy the watchdog for live trading, the interface is ready.

## Acceptance Criteria (BDD)

- Given heartbeat config When engine running Then emits HeartbeatEvent at configurable interval (default 10s), monotonically increasing sequence + timestamp
- Given emission mechanism When sent Then via configurable transport (V1: TCP socket or health endpoint), simple enough for future watchdog to consume
- Given engine unable to emit When heartbeat missed Then miss logged, next heartbeat includes gap info
- Given watchdog binary not yet built When heartbeats emitted Then also logged to event journal for testing/verification

## Tasks / Subtasks

### Task 1: Define HeartbeatEvent type in core (AC: monotonically increasing sequence + timestamp)
- 1.1: Verify `core/src/events/lifecycle.rs` has `HeartbeatEvent` struct; if not, define it with fields: `timestamp` (UnixNanos), `sequence` (u64), `gap_count` (u32, number of missed heartbeats since last successful emit), `gap_duration_ms` (u64, total gap duration if any)
- 1.2: Implement `HeartbeatEvent::new(timestamp: UnixNanos, sequence: u64) -> Self` with gap fields defaulting to 0
- 1.3: Implement `HeartbeatEvent::with_gap(timestamp: UnixNanos, sequence: u64, gap_count: u32, gap_duration_ms: u64) -> Self`
- 1.4: Derive `Debug`, `Clone`, `serde::Serialize`, `serde::Deserialize` for wire format and journal storage

### Task 2: Add heartbeat configuration (AC: configurable interval, configurable transport)
- 2.1: In `core/src/config/` or `engine/src/config/`, add `HeartbeatConfig` struct: `interval_secs` (u64, default 10), `transport` (HeartbeatTransport enum), `enabled` (bool, default true)
- 2.2: Define `HeartbeatTransport` enum: `TcpSocket { host: String, port: u16 }`, `HealthEndpoint` (reuses metrics HTTP server), `JournalOnly` (log to journal, no external transport)
- 2.3: Add `[heartbeat]` section to config TOML with serde deserialization
- 2.4: Default transport for V1: `JournalOnly` (safest, no external dependencies)

### Task 3: Implement heartbeat emitter (AC: emits at configurable interval)
- 3.1: Create `engine/src/heartbeat.rs` with `HeartbeatEmitter` struct
- 3.2: Fields: `config` (HeartbeatConfig), `sequence` (AtomicU64), `last_emit` (Instant), `missed_count` (u32), `journal_sender` (crossbeam Sender)
- 3.3: Implement `tick(&mut self, clock: &dyn Clock)` — called from a timer or dedicated thread, checks if interval has elapsed
- 3.4: On interval elapsed: increment sequence, create `HeartbeatEvent`, send via configured transport and to journal
- 3.5: Sequence number must be monotonically increasing across the engine's lifetime (no reset on reconnect)

### Task 4: Implement gap detection and reporting (AC: missed heartbeat logged, next includes gap info)
- 4.1: Track `expected_next_emit` timestamp; if current time exceeds `expected_next_emit + interval`, a gap occurred
- 4.2: Calculate `gap_count`: how many heartbeat intervals were missed
- 4.3: On gap detection, log at `warn` level: `tracing::warn!(missed_count = gap_count, gap_duration_ms, "heartbeat gap detected")`
- 4.4: Next `HeartbeatEvent` includes `gap_count` and `gap_duration_ms` fields so the future watchdog knows about the gap
- 4.5: Reset gap tracking after successfully emitting the catch-up heartbeat

### Task 5: Implement V1 transport: TCP socket (AC: simple enough for future watchdog)
- 5.1: For `TcpSocket` transport, open a non-blocking TCP connection to configured host:port
- 5.2: Serialize `HeartbeatEvent` as newline-delimited JSON and write to socket
- 5.3: If connection fails, log at `warn` level and fall back to journal-only (do not crash)
- 5.4: Reconnect on next heartbeat interval if previously disconnected
- 5.5: Protocol is deliberately simple: one JSON line per heartbeat, no handshake, no acknowledgment

### Task 6: Implement health endpoint transport option (AC: configurable transport)
- 6.1: If `HealthEndpoint` transport selected and metrics HTTP server is running (Story 9.4), add GET `/heartbeat` endpoint
- 6.2: Endpoint returns JSON with latest `HeartbeatEvent` and 200 OK
- 6.3: If no heartbeat has been emitted yet, return 503 Service Unavailable
- 6.4: This allows a simple `curl` or HTTP check as the watchdog mechanism

### Task 7: Wire heartbeat emission to event journal (AC: logged to event journal for testing)
- 7.1: Send `HeartbeatEvent` to journal via the existing `crossbeam_channel::Sender<EngineEvent>` (as `EngineEvent::Heartbeat(HeartbeatEvent)`)
- 7.2: In journal writer, handle `EngineEvent::Heartbeat` variant — insert into SQLite with timestamp and sequence
- 7.3: Add structured `tracing::debug!` on each heartbeat emission (debug level to avoid log noise)
- 7.4: Verify heartbeats appear in daily Parquet export for offline analysis

### Task 8: Integrate heartbeat emitter into engine lifecycle (AC: emits when engine running)
- 8.1: In `engine/src/lifecycle/startup.rs`, create `HeartbeatEmitter` from config and start emission
- 8.2: Option A: dedicated thread with `sleep(interval)` loop — simplest, isolated from hot path
- 8.3: Option B: timer on Tokio runtime — leverages existing async infrastructure
- 8.4: In `engine/src/lifecycle/shutdown.rs`, stop heartbeat emission gracefully, emit final heartbeat with `message: "shutdown"`
- 8.5: First heartbeat emitted immediately on startup (sequence=1), then at configured interval

### Task 9: Unit and integration tests (AC: all)
- 9.1: Test `HeartbeatEvent` sequence is monotonically increasing across multiple emissions
- 9.2: Test heartbeat emitted at configured interval (use SimClock to advance time)
- 9.3: Test gap detection: advance clock past 3 intervals, verify next heartbeat has `gap_count=2`
- 9.4: Test journal receives `HeartbeatEvent` via channel
- 9.5: Test TCP transport handles connection failure gracefully (no panic, falls back to journal)
- 9.6: Test health endpoint returns latest heartbeat JSON
- 9.7: Test default config: 10s interval, enabled, JournalOnly transport

## Dev Notes

### Architecture Patterns & Constraints
- `HeartbeatEvent` is defined in `core/src/events/lifecycle.rs` as part of the `EngineEvent` enum (architecture specifies `EngineEvent::Heartbeat(HeartbeatEvent)`)
- Heartbeat emitter runs on its own thread or Tokio task — never on the hot path thread
- V1 scope: the watchdog binary itself is deferred to live-trading preparation. This story builds only the heartbeat emission interface
- TCP socket protocol is intentionally minimal: newline-delimited JSON, no handshake. Future watchdog just reads lines and checks for gaps
- Sequence numbers never reset during engine lifetime. On engine restart, sequence starts at 1 again (watchdog detects restart by sequence reset)
- Gap info is included in the HeartbeatEvent itself, so the watchdog doesn't need to track timing — it just reads the gap fields

### Project Structure Notes
```
crates/core/
├── src/
│   ├── events/
│   │   └── lifecycle.rs        # HeartbeatEvent struct (may already exist)
│   └── config/
│       └── (heartbeat config if in core, or engine config)
crates/engine/
├── src/
│   ├── heartbeat.rs            # NEW: HeartbeatEmitter, transport logic
│   ├── config/
│   │   └── loader.rs           # Load [heartbeat] config section
│   ├── lifecycle/
│   │   ├── startup.rs          # Start heartbeat emitter
│   │   └── shutdown.rs         # Stop heartbeat, emit final event
│   └── persistence/
│       └── journal.rs          # Handle EngineEvent::Heartbeat variant
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Sections: Watchdog, EngineEvent variants
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 9, Story 9.5
- Architecture file tree: `core/src/events/lifecycle.rs` — `HeartbeatEvent` defined here
- Dependencies: crossbeam-channel 0.5.15, serde 1.x, chrono 0.4.44

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
