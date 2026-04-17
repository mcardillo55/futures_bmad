# Story 8.2: Startup Sequence

Status: ready-for-dev

## Story

As a trader-operator,
I want a safe, deterministic startup sequence,
So that the system never begins trading in an inconsistent state.

## Acceptance Criteria (BDD)

- Given `engine/src/lifecycle/startup.rs` When system starts Then executes in strict order: 1. Load/validate config 2. Init SQLite journal (WAL) 3. Connect to Rithmic 4. Verify connectivity 5. Replay order WAL 6. Sync positions 7. Arm circuit breakers 8. Start heartbeat 9. Begin market data 10. Enable strategies (if regime permits)
- Given any step fails When failure occurs Then logs with full context, exits cleanly, no partial state, exit code indicates failed step
- Given WAL replay discovers in-flight orders When reconciliation determines state Then filled orders update position, uncertain orders trigger reconciliation, orphaned orders cancelled

## Tasks / Subtasks

### Task 1: Define startup step abstraction (AC: strict order, clean exit on failure)
- 1.1: Create `crates/engine/src/lifecycle/mod.rs` with module declarations for `startup` and `shutdown`
- 1.2: In `crates/engine/src/lifecycle/startup.rs`, define `StartupStep` enum with 10 variants matching the sequence: `LoadConfig`, `InitJournal`, `ConnectBroker`, `VerifyConnectivity`, `ReplayWal`, `SyncPositions`, `ArmCircuitBreakers`, `StartHeartbeat`, `BeginMarketData`, `EnableStrategies`
- 1.3: Implement `StartupStep::exit_code(&self) -> i32` mapping each step to a unique exit code (10-19) for operator diagnostics
- 1.4: Implement `Display` for `StartupStep` with human-readable descriptions

### Task 2: Implement startup orchestrator (AC: strict sequential order, full context logging)
- 2.1: Implement `pub async fn run_startup(cli_args: CliArgs) -> Result<RunningSystem>` as the top-level entry point
- 2.2: Execute each step sequentially using a macro or explicit calls — each step wrapped in `execute_step(step, || { ... })` that logs start/completion/failure
- 2.3: On any step failure: log error with `anyhow` context chain including step name and details, call cleanup for any already-initialized resources, exit with step-specific exit code
- 2.4: Define `RunningSystem` struct containing all initialized components: config, journal, broker connection, circuit breakers, market data handle, strategy manager

### Task 3: Implement Step 1 — Load/validate config (AC: config loading)
- 3.1: Call `load_config(&cli_args)` from Story 8.1
- 3.2: On validation failure, format all validation errors and exit with code 10
- 3.3: Log loaded config summary at `info` level (redacting credentials)

### Task 4: Implement Step 2 — Init SQLite journal (AC: WAL mode)
- 4.1: Open SQLite database at configured path with WAL journal mode
- 4.2: Run migrations if needed (create tables if not exist)
- 4.3: Verify write access by inserting and deleting a test record
- 4.4: Log journal path and WAL status at `info` level

### Task 5: Implement Step 3-4 — Connect to Rithmic and verify (AC: connectivity)
- 5.1: Create broker adapter using config credentials (SecretString)
- 5.2: Establish WebSocket connections to Rithmic plants (ticker, order, PnL)
- 5.3: Verify connectivity: send heartbeat, wait for response with timeout (10s)
- 5.4: On connection failure, log endpoint and error details (never log credentials)

### Task 6: Implement Step 5 — Replay order WAL (AC: in-flight order reconciliation)
- 6.1: In `crates/engine/src/order_manager/wal.rs`, implement `replay_wal(journal: &Journal, broker: &BrokerAdapter) -> Result<WalReplayResult>`
- 6.2: Query journal for orders in non-terminal states (Submitted, Confirmed, PartialFill)
- 6.3: For each in-flight order, query broker for current status via OrderPlant
- 6.4: Filled orders: update local position state, log fill details
- 6.5: Uncertain orders (no response from broker within 5s): add to reconciliation queue, log warning
- 6.6: Orphaned orders (broker has no record): attempt cancel, log as orphaned
- 6.7: Define `WalReplayResult` struct: `filled: Vec<OrderId>`, `cancelled: Vec<OrderId>`, `reconciled: Vec<OrderId>`, `uncertain: Vec<OrderId>`

### Task 7: Implement Step 6 — Sync positions (AC: position sync)
- 7.1: Call `BrokerAdapter::query_positions()` via `crates/broker/src/position_sync.rs`
- 7.2: Compare broker-reported positions against local state (from WAL replay)
- 7.3: If mismatch: log discrepancy with both sides, use broker as source of truth, update local state
- 7.4: If local shows flat but broker shows position: critical warning, update local to match broker
- 7.5: Log final position state at `info` level

### Task 8: Implement Steps 7-10 — Arm breakers, heartbeat, market data, strategies (AC: full sequence)
- 8.1: Arm circuit breakers with config thresholds from `TradingConfig` (Story 5.1 `CircuitBreakers::new`)
- 8.2: Start heartbeat task (periodic broker ping, detection of disconnection)
- 8.3: Subscribe to market data feeds for configured instruments
- 8.4: Wait for initial market data snapshot (order book populated)
- 8.5: Enable strategies only if current regime permits trading (check regime state)
- 8.6: Log "System ready" at `info` level with summary: instruments, position state, regime

### Task 9: Unit tests (AC: all)
- 9.1: Test successful startup: all steps execute in order, RunningSystem returned
- 9.2: Test step failure at each position: verify cleanup called, correct exit code returned
- 9.3: Test WAL replay: in-flight order found filled at broker, position updated
- 9.4: Test WAL replay: orphaned order (broker has no record) is cancelled locally
- 9.5: Test position sync: mismatch detected, broker position used as source of truth
- 9.6: Test position sync: matching positions pass without warning
- 9.7: Test config failure produces exit code 10 with all validation errors

## Dev Notes

### Architecture Patterns & Constraints
- Startup is strictly sequential — no parallelism. Each step depends on the previous step's success. This is intentional for safety: we never want to subscribe to market data before positions are synced
- `anyhow` is used for startup errors because startup is not on the hot path and rich error context is valuable for operator diagnostics
- Exit codes 10-19 are reserved for startup failures. The operator/systemd can distinguish failure types from the exit code
- WAL replay is critical for crash recovery: if the process died between submitting an order and receiving a fill, the WAL holds the last known state. The broker is always the source of truth
- Position sync uses `BrokerAdapter::query_positions()` from `broker/src/position_sync.rs` — this queries the PnL plant for current positions
- Strategies are only enabled if the current regime permits. If regime is "no-trade" (e.g., pre-market), strategies stay disabled until regime changes

### Project Structure Notes
```
crates/engine/src/lifecycle/
├── mod.rs              (module declarations)
├── startup.rs          (StartupStep, run_startup, RunningSystem)
└── shutdown.rs         (Story 8.3)

crates/engine/src/order_manager/
└── wal.rs              (replay_wal, WalReplayResult)

crates/broker/src/
└── position_sync.rs    (query_positions, PositionReport)
```

### References
- Story 8.1 (Configuration Loading) — Step 1 dependency
- Story 4.1 (SQLite Event Journal) — Step 2 journal init
- Story 5.1 (Circuit Breaker Framework) — Step 7 arming
- Story 8.4 (Reconnection FSM) — heartbeat and connection monitoring

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
