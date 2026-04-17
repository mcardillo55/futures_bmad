# Story 9.4: Prometheus Metrics Endpoint

Status: ready-for-dev

## Story

As a trader-operator,
I want real-time system metrics available via HTTP,
So that I can monitor system health and performance.

## Acceptance Criteria (BDD)

- Given `engine/src/metrics.rs` When initialized Then lightweight HTTP endpoint via axum on configurable port, serves Prometheus-format at /metrics
- Given endpoint running When scraped Then reports: tick_to_decision_latency_ns (histogram), spsc_buffer_occupancy (gauge per queue), order_fill_latency_ns (histogram), daily_pnl_ticks (gauge), connection_state (gauge), circuit_breaker_active (gauge per type), trades_today (counter), regime_state (gauge), engine_version (info)
- Given metric collection When updated Then outside hot path (via crossbeam channel), HTTP on Tokio runtime

## Tasks / Subtasks

### Task 1: Add metrics dependencies to engine crate (AC: lightweight HTTP endpoint)
- 1.1: Add `axum = "0.8"` to workspace and engine Cargo.toml
- 1.2: Add `prometheus-client = "0.23"` (or `metrics` + `metrics-exporter-prometheus`) to workspace and engine Cargo.toml
- 1.3: Verify `tokio` is already a dependency (it is — used for broker async runtime)

### Task 2: Implement metrics registry and metric definitions (AC: all required metrics)
- 2.1: In `engine/src/metrics.rs`, create `EngineMetrics` struct holding all metric handles
- 2.2: Define `tick_to_decision_latency_ns` as a Histogram with appropriate buckets (e.g., 100ns, 500ns, 1us, 5us, 10us, 50us, 100us)
- 2.3: Define `spsc_buffer_occupancy` as a Family of Gauges labeled by `queue` ("market", "order", "fill")
- 2.4: Define `order_fill_latency_ns` as a Histogram with buckets (1ms, 5ms, 10ms, 50ms, 100ms, 500ms, 1s)
- 2.5: Define `daily_pnl_ticks` as a Gauge (i64)
- 2.6: Define `connection_state` as a Gauge (numeric FSM state: 0=Disconnected, 1=Connecting, 2=Connected, 3=Recovering, 4=Failed)
- 2.7: Define `circuit_breaker_active` as a Family of Gauges labeled by `breaker_type` (one per BreakerType variant, value 0 or 1)
- 2.8: Define `trades_today` as a Counter (u64)
- 2.9: Define `regime_state` as a Gauge (numeric: 0=Unknown, 1=Trending, 2=MeanReverting, 3=Volatile)
- 2.10: Define `engine_version` as an Info metric using `env!("CARGO_PKG_VERSION")`

### Task 3: Implement axum HTTP server for /metrics endpoint (AC: serves Prometheus-format at /metrics)
- 3.1: Create `async fn start_metrics_server(registry: Registry, port: u16)` that spawns an axum server
- 3.2: Define GET `/metrics` handler that encodes registry to Prometheus text format
- 3.3: Add optional GET `/health` endpoint returning 200 OK for basic liveness check
- 3.4: Make port configurable via `[metrics]` section in config TOML: `metrics_port = 9090`
- 3.5: Server runs on Tokio runtime — must NOT run on hot path thread

### Task 4: Implement metrics update channel (AC: updates outside hot path)
- 4.1: Define `MetricsUpdate` enum with variants for each metric update: `TickLatency(u64)`, `BufferOccupancy { queue: String, value: f64 }`, `FillLatency(u64)`, `PnlUpdate(i64)`, `ConnectionState(u8)`, `BreakerState { breaker: String, active: bool }`, `TradeExecuted`, `RegimeChange(u8)`
- 4.2: Create `crossbeam_channel::Sender<MetricsUpdate>` / `Receiver<MetricsUpdate>` pair
- 4.3: Pass `Sender` to hot path components; they call `try_send()` (non-blocking, drop if full)
- 4.4: Spawn a Tokio task that drains the `Receiver` and applies updates to the prometheus registry
- 4.5: Channel capacity: 4096 entries — large enough for burst updates, bounded to prevent memory growth

### Task 5: Wire metric emission points in hot path (AC: all metrics populated)
- 5.1: In `engine/src/event_loop.rs`, measure tick-to-decision latency using `Instant::elapsed()` and send `TickLatency` update
- 5.2: In event loop, periodically (every N ticks) check SPSC buffer occupancy and send `BufferOccupancy` updates
- 5.3: In order manager, on fill receipt, compute fill latency (submission timestamp to fill timestamp) and send `FillLatency`
- 5.4: In P&L tracking, on each P&L change, send `PnlUpdate`
- 5.5: In connection FSM, on state change, send `ConnectionState`
- 5.6: In circuit breakers, on trip/clear, send `BreakerState`
- 5.7: In order manager, on trade execution, send `TradeExecuted`
- 5.8: In regime detector, on state change, send `RegimeChange`

### Task 6: Integrate metrics server startup into engine lifecycle (AC: initialized at startup)
- 6.1: In `engine/src/lifecycle/startup.rs`, initialize `EngineMetrics` and start HTTP server before entering event loop
- 6.2: Pass metrics `Sender` to all components during construction
- 6.3: In `engine/src/lifecycle/shutdown.rs`, gracefully shutdown axum server
- 6.4: Log metrics endpoint URL at startup: `tracing::info!("Metrics available at http://0.0.0.0:{port}/metrics")`

### Task 7: Unit and integration tests (AC: all)
- 7.1: Test each metric type registers correctly and can be updated
- 7.2: Test `/metrics` endpoint returns valid Prometheus text format (parse with a Prometheus text parser or regex)
- 7.3: Test `MetricsUpdate` channel: send updates, verify metrics reflect new values
- 7.4: Test non-blocking behavior: fill channel to capacity, verify `try_send` returns without blocking
- 7.5: Test `engine_version` info metric matches `CARGO_PKG_VERSION`
- 7.6: Integration test: start metrics server, HTTP GET `/metrics`, verify all metric names present in output

## Dev Notes

### Architecture Patterns & Constraints
- Metrics collection MUST NOT impact hot path performance. All metric updates go through a crossbeam channel with `try_send()` — if the channel is full, the update is silently dropped (metrics are best-effort)
- The HTTP server runs on the Tokio runtime, completely separate from the hot path thread. The hot path thread only sends `MetricsUpdate` messages
- axum chosen for minimal overhead — no need for full web framework features
- Prometheus text format is the standard; no need for protobuf/OpenMetrics unless specifically requested
- Histogram buckets should be tuned to the expected latency ranges: tick-to-decision is microsecond-scale, fill latency is millisecond-scale
- `env!("CARGO_PKG_VERSION")` is resolved at compile time — no runtime cost

### Project Structure Notes
```
crates/engine/
├── Cargo.toml                  # Add axum, prometheus-client
├── src/
│   ├── metrics.rs              # EngineMetrics struct, MetricsUpdate enum, HTTP server, /metrics handler
│   ├── main.rs                 # Wire metrics server startup
│   ├── lifecycle/
│   │   ├── startup.rs          # Initialize metrics, start HTTP server
│   │   └── shutdown.rs         # Graceful metrics server shutdown
│   ├── event_loop.rs           # Emit TickLatency, BufferOccupancy updates
│   ├── risk/
│   │   └── circuit_breakers.rs # Emit BreakerState updates
│   ├── regime/
│   │   └── threshold.rs        # Emit RegimeChange updates
│   └── connection/
│       └── fsm.rs              # Emit ConnectionState updates
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Section: Monitoring (V1), "Lightweight Prometheus metrics via HTTP endpoint (axum)"
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 9, Story 9.4
- Dependencies: axum 0.8, prometheus-client 0.23 (or metrics + metrics-exporter-prometheus), crossbeam-channel 0.5.15, tokio 1.51.1

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
