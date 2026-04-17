# Story 8.4: Reconnection FSM

Status: ready-for-dev

## Story

As a trader-operator,
I want the system to automatically recover from broker disconnections,
So that brief network blips don't require manual intervention.

## Acceptance Criteria (BDD)

- Given `engine/src/connection/fsm.rs` When FSM implemented Then 5 states: Connected, Disconnected, Reconnecting, Reconciling, Ready + terminal CircuitBreak
- Given connection drops When WebSocket detected (within 5s, NFR7) Then Connected→Disconnected, strategies paused, existing stops remain
- Given Disconnected When reconnection attempted Then →Reconnecting, exponential backoff (initial=1s, max=60s, jitter), each attempt logged
- Given connection re-established When succeeds Then →Reconciling, positions queried from PnL plant, orders queried from OrderPlant, compared against local state, wait for stable (5s)
- Given reconciliation succeeds When state consistent Then →Ready, strategies re-armed
- Given reconciliation fails (mismatch or 60s timeout) When detected Then →CircuitBreak, connection failure breaker, full diagnostic logged

## Tasks / Subtasks

### Task 1: Define FSM states and transitions (AC: 5 states + CircuitBreak)
- 1.1: Create `crates/engine/src/connection/mod.rs` with module declaration for `fsm`
- 1.2: In `crates/engine/src/connection/fsm.rs`, define `ConnectionState` enum: `Connected`, `Disconnected { since: Instant }`, `Reconnecting { attempt: u32, next_retry: Instant }`, `Reconciling { started: Instant }`, `Ready`, `CircuitBreak { reason: String }`
- 1.3: Define `ConnectionEvent` enum for valid transitions: `ConnectionLost`, `ReconnectAttempt`, `ConnectionRestored`, `ReconciliationComplete`, `ReconciliationFailed(String)`, `ReconciliationTimeout`
- 1.4: Implement `ConnectionState::apply(self, event: ConnectionEvent) -> ConnectionState` as a pure state transition function
- 1.5: Ensure invalid transitions are logged and rejected (e.g., cannot go from Ready to Reconciling without disconnection)

### Task 2: Implement FSM controller (AC: detection within 5s, strategies paused)
- 2.1: Create `ConnectionFsm` struct holding: `state: ConnectionState`, `config: ReconnectionConfig`, sender for state change notifications
- 2.2: Define `ReconnectionConfig`: `initial_backoff_ms: u64` (default 1000), `max_backoff_ms: u64` (default 60000), `jitter_range_ms: u64` (default 500), `reconciliation_timeout_s: u64` (default 60), `stability_window_s: u64` (default 5)
- 2.3: Implement `on_connection_lost(&mut self)`: transition to Disconnected, emit strategy pause signal, log at `warn` level
- 2.4: CRITICAL: on disconnection, existing exchange-resting stop orders remain — do NOT attempt to cancel or modify them
- 2.5: Implement `current_state(&self) -> &ConnectionState` for external queries
- 2.6: Implement `can_submit_orders(&self) -> bool` returning true only in `Ready` state

### Task 3: Implement exponential backoff with jitter (AC: backoff initial=1s, max=60s, jitter)
- 3.1: Implement `calculate_backoff(attempt: u32, config: &ReconnectionConfig) -> Duration`
- 3.2: Backoff formula: `min(initial * 2^attempt, max)` with random jitter added (0..jitter_range_ms)
- 3.3: Use `rand::thread_rng()` for jitter to prevent thundering herd on multi-instance setups
- 3.4: Log each reconnection attempt at `info` level: attempt number, backoff duration, next retry time

### Task 4: Implement reconnection loop (AC: reconnection attempts, logging)
- 4.1: Implement `async fn reconnection_loop(&mut self, broker: &BrokerAdapter)` that runs while in Reconnecting state
- 4.2: On each iteration: sleep for backoff duration, attempt connection via `broker.connect()`
- 4.3: On success: transition to Reconciling, break loop
- 4.4: On failure: increment attempt counter, recalculate backoff, log failure reason
- 4.5: No maximum attempt limit — keep retrying until connection restored or operator intervenes

### Task 5: Implement reconciliation phase (AC: query positions/orders, compare, 5s stability)
- 5.1: Implement `async fn reconcile(&mut self, broker: &BrokerAdapter, local_state: &PositionManager) -> Result<(), ReconciliationError>`
- 5.2: Query positions from PnL plant via `BrokerAdapter::query_positions()` (broker/src/position_sync.rs)
- 5.3: Query open orders from OrderPlant via `BrokerAdapter::query_orders()`
- 5.4: Compare broker-reported positions against `local_state` — log any discrepancies
- 5.5: Compare broker-reported orders against local order state — identify missing/extra orders
- 5.6: If discrepancies found: adopt broker state as source of truth, update local state, log each change at `warn` level
- 5.7: After initial comparison, poll for stability: re-query at 1s intervals for 5s, verify no further changes
- 5.8: If state stable for 5s: return Ok, transition to Ready

### Task 6: Implement reconciliation failure handling (AC: 60s timeout = CircuitBreak)
- 6.1: Start reconciliation timeout timer (60s from `ReconnectionConfig`)
- 6.2: If stability not achieved within 60s: return `ReconciliationError::Timeout`
- 6.3: If position mismatch cannot be resolved: return `ReconciliationError::PositionMismatch { local, broker }`
- 6.4: On any reconciliation error: transition to CircuitBreak, trip connection failure breaker (Story 5.1)
- 6.5: Log full diagnostic at `error` level: local state snapshot, broker state snapshot, all discrepancies, timeline of reconciliation attempts

### Task 7: Implement state change notifications (AC: strategies paused/re-armed)
- 7.1: Use `tokio::sync::watch` channel to broadcast state changes to subscribers
- 7.2: Strategy manager subscribes: pauses on Disconnected/Reconnecting/Reconciling, re-arms on Ready
- 7.3: Order manager subscribes: rejects new orders unless state is Ready
- 7.4: Heartbeat monitor subscribes: detects Connected→Disconnected transition
- 7.5: Log each state transition at `info` level with previous and new state

### Task 8: Implement heartbeat-based disconnection detection (AC: detection within 5s)
- 8.1: Implement heartbeat sender: send ping to broker every 2s while Connected/Ready
- 8.2: Implement response monitor: if no pong received within 5s, trigger ConnectionLost event
- 8.3: Also detect WebSocket close/error frames as immediate disconnection
- 8.4: Log heartbeat failures at `warn` level before triggering disconnection

### Task 9: Unit tests (AC: all)
- 9.1: Test state transition: Connected → Disconnected on ConnectionLost
- 9.2: Test state transition: Disconnected → Reconnecting → Reconciling → Ready (happy path)
- 9.3: Test state transition: Reconciling → CircuitBreak on timeout
- 9.4: Test invalid transition rejected: Ready → Reconciling (must go through Disconnected first)
- 9.5: Test exponential backoff: attempt 0=1s, attempt 1=2s, attempt 2=4s, capped at 60s
- 9.6: Test jitter: backoff values have random variation within configured range
- 9.7: Test `can_submit_orders()`: true only in Ready state
- 9.8: Test reconciliation: matching positions → Ready
- 9.9: Test reconciliation: mismatched positions → update local, log warning, still Ready
- 9.10: Test reconciliation: 60s timeout → CircuitBreak with diagnostic log
- 9.11: Test strategies paused on Disconnected, re-armed on Ready

## Dev Notes

### Architecture Patterns & Constraints
- The FSM is a pure state machine: `apply(state, event) -> state`. Side effects (broker calls, logging) happen in the controller, not in the transition function. This makes the FSM trivially testable
- CRITICAL: no new orders may be submitted except in Ready state. The `can_submit_orders()` check is called by the order manager before every submission
- Exchange-resting stops are NEVER touched during reconnection. They protect positions independently of our connection state
- Reconciliation is mandatory — we never skip it. Even if the disconnection was 100ms, we must verify state consistency before resuming trading
- The 5s stability window in reconciliation prevents race conditions: if positions are still changing (fills arriving), we wait until they settle
- CircuitBreak is terminal for the FSM — requires operator intervention or process restart to recover. This prevents automated recovery from genuinely broken states

### Project Structure Notes
```
crates/engine/src/connection/
├── mod.rs              (module declarations, ReconnectionConfig)
└── fsm.rs              (ConnectionState, ConnectionEvent, ConnectionFsm)

crates/broker/src/
└── position_sync.rs    (query_positions — used during reconciliation)
```

### References
- Story 5.1 (Circuit Breaker Framework) — connection failure breaker tripped on CircuitBreak
- Story 8.2 (Startup Sequence) — heartbeat started during startup
- Story 8.3 (Graceful Shutdown) — FSM state affects shutdown behavior
- NFR7: disconnection detection within 5 seconds

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
