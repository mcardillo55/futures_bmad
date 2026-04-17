# Story 8.5: Deployment Workflow

Status: ready-for-dev

## Story

As a trader-operator,
I want a safe deployment process with rollback capability,
So that I can update the system without risking data loss or unexpected behavior.

## Acceptance Criteria (BDD)

- Given new binary built When deployed Then previous kept as engine.prev, new deployed to /opt/trading/engine, systemd restart
- Given systemd unit When configured Then runs with --config flag, Restart=on-failure, resource limits, env vars from systemd env file
- Given deploy during market hours When attempted Then only permitted if all positions flat, operator confirms
- Given binary When starts Then logs version (CARGO_PKG_VERSION), version in Prometheus metrics

## Tasks / Subtasks

### Task 1: Add version reporting to engine binary (AC: version logging, Prometheus metrics)
- 1.1: In `crates/engine/src/main.rs`, log version at startup using `env!("CARGO_PKG_VERSION")` at `info` level: "Trading engine v{version} starting"
- 1.2: Add `--version` flag to CLI args (clap handles this with `#[command(version)]`)
- 1.3: Define Prometheus gauge `engine_build_info` with labels: `version`, `rust_version`, `target` — set to 1 on startup
- 1.4: Use `built` or compile-time env vars for `rust_version` and `target` metadata

### Task 2: Create deployment script (AC: binary swap, rollback, engine.prev)
- 2.1: Create `scripts/deploy.sh` with deploy workflow:
  - Accept new binary path as argument
  - Validate binary exists and is executable
  - Check if deployment during market hours (warn and require `--force` flag)
  - Copy current `/opt/trading/engine` to `/opt/trading/engine.prev`
  - Copy new binary to `/opt/trading/engine`
  - Set ownership and permissions (trading-engine user, 0755)
  - Restart via `systemctl restart trading-engine`
  - Wait for startup (poll systemctl status, timeout 30s)
  - Verify new version via log output or health endpoint
- 2.2: Create `scripts/rollback.sh`:
  - Copy `/opt/trading/engine.prev` back to `/opt/trading/engine`
  - Restart via `systemctl restart trading-engine`
  - Log rollback event

### Task 3: Implement market hours safety check (AC: flat position required during market hours)
- 3.1: In `scripts/deploy.sh`, define market hours check: CME Globex ES hours (Sunday 5pm CT - Friday 4pm CT, with daily halt 4:15-4:30pm CT)
- 3.2: If within market hours and `--force` not specified: exit with error "Deploy during market hours requires --force flag"
- 3.3: If `--force` specified during market hours: query position state (if engine is running) via journal or health endpoint
- 3.4: If positions are not flat: exit with error "Cannot deploy with open positions. Flatten first."
- 3.5: Require explicit operator confirmation: prompt "Deploy during market hours with flat positions. Continue? [y/N]"

### Task 4: Create systemd unit file (AC: --config flag, Restart=on-failure, resource limits, env vars)
- 4.1: Create `deploy/trading-engine.service` systemd unit file:
  ```
  [Unit]
  Description=Futures Trading Engine
  After=network-online.target
  Wants=network-online.target

  [Service]
  Type=simple
  User=trading-engine
  Group=trading-engine
  ExecStart=/opt/trading/engine --mode live --config /opt/trading/config
  Restart=on-failure
  RestartSec=10
  EnvironmentFile=/opt/trading/env
  LimitNOFILE=65536
  LimitMEMLOCK=infinity
  MemoryMax=2G
  CPUQuota=200%
  WorkingDirectory=/opt/trading

  [Install]
  WantedBy=multi-user.target
  ```
- 4.2: Create `deploy/env.example` template for the systemd environment file with broker credential placeholders
- 4.3: Document that `/opt/trading/env` must be mode 0600 owned by root (credentials file)

### Task 5: Create build script (AC: release build)
- 5.1: Create `scripts/build-release.sh`:
  - Run `cargo build --release --target x86_64-unknown-linux-gnu`
  - Strip binary: `strip target/x86_64-unknown-linux-gnu/release/engine`
  - Print binary size and version
  - Copy to `dist/engine-{version}` with version from Cargo.toml
- 5.2: Add `cross` support for building Linux binary from macOS dev machine
- 5.3: Log build metadata: git commit hash, build timestamp, rust version

### Task 6: Create deployment directory structure documentation (AC: deploy path)
- 6.1: Document expected directory structure in code comments or deploy README:
  ```
  /opt/trading/
  ├── engine              (current binary)
  ├── engine.prev         (previous binary for rollback)
  ├── config/
  │   ├── default.toml
  │   ├── live.toml
  │   └── paper.toml
  ├── env                 (systemd environment file, mode 0600)
  ├── data/
  │   ├── journal.db      (SQLite journal)
  │   └── parquet/        (market data recordings)
  └── logs/               (log files if not using journald)
  ```
- 6.2: Create `scripts/setup-host.sh` that creates directory structure, user, and sets permissions

### Task 7: Tests and validation (AC: all)
- 7.1: Test version string is embedded: `engine --version` prints correct version
- 7.2: Test deploy script: mock deployment to temp directory, verify engine.prev created
- 7.3: Test rollback script: verify engine.prev restored to engine
- 7.4: Test market hours check: mock time during market hours, verify --force required
- 7.5: Test systemd unit file syntax: `systemd-analyze verify deploy/trading-engine.service`
- 7.6: Verify Prometheus build info metric emitted on startup

## Dev Notes

### Architecture Patterns & Constraints
- Deployment is intentionally simple: binary swap + systemd restart. No containers, no orchestrators. Single binary on a single host
- Rollback is one command: copy engine.prev back and restart. This is faster and more reliable than any complex rollback mechanism
- Market hours check is a safety gate, not a hard block. The `--force` flag exists because sometimes you must deploy during market hours (critical bug fix). But you must be flat first
- systemd `Restart=on-failure` with `RestartSec=10` provides basic crash recovery. Combined with WAL replay (Story 8.2), the system recovers automatically from most crashes
- The env file (`/opt/trading/env`) contains broker credentials. It must be mode 0600 owned by root — systemd reads it as root before dropping privileges to the trading-engine user
- `LimitMEMLOCK=infinity` is needed for potential future use of io_uring or locked memory for the hot path

### Project Structure Notes
```
scripts/
├── build-release.sh    (cargo build --release, strip, version)
├── deploy.sh           (binary swap, market hours check)
├── rollback.sh         (restore engine.prev)
└── setup-host.sh       (create directories, user, permissions)

deploy/
├── trading-engine.service  (systemd unit file)
└── env.example             (environment file template)
```

### References
- Story 8.1 (Configuration Loading) — config directory structure, env vars
- Story 8.2 (Startup Sequence) — startup after systemd restart
- Story 8.3 (Graceful Shutdown) — SIGTERM from systemd stop

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
