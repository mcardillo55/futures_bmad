# Story 1.1: Cargo Workspace & CI/CD Setup

Status: review

## Story

As a developer,
I want a properly structured Cargo virtual workspace with all four crates scaffolded and CI running,
So that all subsequent development has a consistent, verified foundation to build on.

## Acceptance Criteria (BDD)

- Given an empty project directory When the workspace is initialized Then a Cargo virtual workspace exists with `crates/core`, `crates/broker`, `crates/engine`, `crates/testkit`
- And `Cargo.toml` at root defines `workspace.members` and `workspace.dependencies` for all foundational crates (rithmic-rs =0.7.2, tokio 1.51.1, rtrb 0.3.3, rusqlite 0.38.0, tracing 0.1.44, thiserror 2.0.18, anyhow 1.0.100, proptest 1.9.0, insta 1.46.3, chrono 0.4.44, config 0.15.22, secrecy 0.10.3, tikv-jemallocator 0.6.1, core_affinity 0.8.3, crossbeam-channel 0.5.15, prost 0.14.3, tokio-tungstenite 0.29.0, databento =0.40.0, bumpalo 3.20.2)
- And Rust edition is 2024, toolchain is 1.95.0 stable
- And dependency direction is enforced: engine depends on broker and core; broker depends on core; testkit depends on core; core has zero internal crate dependencies
- And `rustfmt.toml` and `clippy.toml` exist with project conventions
- And `.gitignore` excludes `target/`, `.env`, `data/`, `*.db`, `*.db-wal`
- And `.env.example` documents required environment variables (RITHMIC_USER, RITHMIC_PASSWORD, etc.)
- And `.github/workflows/ci.yml` runs: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo nextest run`, `cargo audit`
- And `config/default.toml`, `config/paper.toml`, `config/live.toml` exist as placeholder config files
- And `cargo nextest run --workspace` passes with zero tests (clean build)
- And `cargo clippy --workspace -- -D warnings` passes

## Tasks / Subtasks

### Task 1: Create root workspace manifest (AC: workspace structure, editions, toolchain)
- [x] 1.1: Create root `Cargo.toml` as virtual workspace with `workspace.members = ["crates/*"]`
- [x] 1.2: Define `workspace.package` with edition = "2024"
- [x] 1.3: Define `workspace.dependencies` listing all foundational crates with exact versions specified in AC
- [x] 1.4: Create `rust-toolchain.toml` pinning to `1.95.0` stable channel

### Task 2: Scaffold all four crates (AC: workspace structure, dependency direction)
- [x] 2.1: Create `crates/core/Cargo.toml` with `package.edition.workspace = true`, zero internal dependencies. Add workspace deps: chrono, thiserror, tracing, serde (derive), proptest (dev)
- [x] 2.2: Create `crates/core/src/lib.rs` with placeholder module structure
- [x] 2.3: Create `crates/broker/Cargo.toml` depending on `futures_core` (path = "../core"). Add workspace deps: rithmic-rs, tokio, tracing, thiserror, anyhow
- [x] 2.4: Create `crates/broker/src/lib.rs` with placeholder
- [x] 2.5: Create `crates/engine/Cargo.toml` depending on `futures_core` and `futures_broker` (path refs). Add workspace deps: tokio, rtrb, tracing, crossbeam-channel, tikv-jemallocator, core_affinity, bumpalo
- [x] 2.6: Create `crates/engine/src/lib.rs` with placeholder
- [x] 2.7: Create `crates/testkit/Cargo.toml` depending on `futures_core` only (not broker or engine). Add workspace deps: proptest, chrono
- [x] 2.8: Create `crates/testkit/src/lib.rs` with placeholder

### Task 3: Create project config and lint files (AC: rustfmt, clippy, gitignore, env)
- [x] 3.1: Create `rustfmt.toml` with project conventions (max_width, edition, imports_granularity)
- [x] 3.2: Create `clippy.toml` with project conventions (cognitive complexity threshold, etc.)
- [x] 3.3: Create `.gitignore` excluding `target/`, `.env`, `data/`, `*.db`, `*.db-wal`
- [x] 3.4: Create `.env.example` documenting RITHMIC_USER, RITHMIC_PASSWORD, RITHMIC_SERVER, RITHMIC_GATEWAY, DATA_DIR, LOG_LEVEL

### Task 4: Create CI workflow (AC: GitHub Actions)
- [x] 4.1: Create `.github/workflows/ci.yml` with steps: checkout, install Rust 1.95.0, install cargo-nextest, install cargo-audit, run `cargo fmt --check`, run `cargo clippy --workspace -- -D warnings`, run `cargo nextest run --workspace`, run `cargo audit`
- [x] 4.2: Set trigger on push to main and pull requests

### Task 5: Create placeholder config files (AC: config files)
- [x] 5.1: Create `config/default.toml` with placeholder sections for trading, broker, fees
- [x] 5.2: Create `config/paper.toml` with paper trading overrides
- [x] 5.3: Create `config/live.toml` with live trading overrides (empty/commented)

### Task 6: Verify clean build (AC: nextest passes, clippy passes)
- [x] 6.1: Run `cargo build --workspace` and verify success
- [x] 6.2: Run `cargo clippy --workspace -- -D warnings` and verify zero warnings
- [x] 6.3: Run `cargo nextest run --workspace` and verify zero tests, zero failures

## Dev Notes

### Architecture Patterns & Constraints
- Virtual workspace manifest: root `Cargo.toml` has no `[package]`, only `[workspace]`
- All shared dependency versions defined in `workspace.dependencies` to ensure consistency
- Crate naming: use `futures_core`, `futures_broker`, `futures_engine`, `futures_testkit` as package names
- Dependency direction is compiler-enforced by Cargo.toml declarations — engine can import broker and core, broker can import core, testkit can import core, core imports nothing internal
- Target: `x86_64-unknown-linux-gnu` for production, native for development
- Edition 2024 requires Rust 1.85+ (1.95.0 satisfies this)

### Project Structure Notes
```
futures-trading/
├── Cargo.toml              (virtual workspace)
├── rust-toolchain.toml
├── rustfmt.toml
├── clippy.toml
├── .gitignore
├── .env.example
├── .github/workflows/ci.yml
├── config/
│   ├── default.toml
│   ├── paper.toml
│   └── live.toml
└── crates/
    ├── core/
    │   ├── Cargo.toml
    │   └── src/lib.rs
    ├── broker/
    │   ├── Cargo.toml
    │   └── src/lib.rs
    ├── engine/
    │   ├── Cargo.toml
    │   └── src/lib.rs
    └── testkit/
        ├── Cargo.toml
        └── src/lib.rs
```

### References
- Architecture document: `docs/architecture.md` — Section: Project Structure, Dependency Graph, Build & CI
- Epics document: `docs/epics.md` — Epic 1, Story 1.1

## Dev Agent Record

### Agent Model Used
Claude Opus 4.6 (1M context)

### Debug Log References
N/A - clean implementation with no issues

### Completion Notes List
- Created virtual workspace with 4 crates: core, broker, engine, testkit
- All workspace dependencies pinned to exact versions from AC
- Dependency direction enforced: engine→{broker,core}, broker→core, testkit→core, core→none
- Edition 2024, toolchain 1.95.0 stable
- CI workflow: fmt check, clippy, nextest, audit on push/PR to main
- All verification passed: build, clippy (zero warnings), tests (zero tests, zero failures)

### Change Log
- 2026-04-16: Story implemented - all tasks completed

### File List
- Cargo.toml (new)
- rust-toolchain.toml (new)
- rustfmt.toml (new)
- clippy.toml (new)
- .gitignore (new)
- .env.example (new)
- .github/workflows/ci.yml (new)
- config/default.toml (new)
- config/paper.toml (new)
- config/live.toml (new)
- crates/core/Cargo.toml (new)
- crates/core/src/lib.rs (new)
- crates/broker/Cargo.toml (new)
- crates/broker/src/lib.rs (new)
- crates/engine/Cargo.toml (new)
- crates/engine/src/lib.rs (new)
- crates/testkit/Cargo.toml (new)
- crates/testkit/src/lib.rs (new)
