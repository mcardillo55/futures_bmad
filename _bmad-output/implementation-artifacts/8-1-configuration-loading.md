# Story 8.1: Configuration Loading

Status: ready-for-dev

## Story

As a trader-operator,
I want layered configuration with semantic validation,
So that I can safely manage different configurations for paper and live trading.

## Acceptance Criteria (BDD)

- Given `engine/src/config/loader.rs` When config loaded at startup Then loads in layers: default.toml → paper.toml or live.toml (CLI flag) → env vars, later overrides earlier, config crate 0.15.22 handles layering/TOML, env vars override (e.g. TRADING__MAX_DAILY_LOSS_TICKS=800)
- Given config loaded When semantic validation runs (core/src/config/validation.rs) Then all values validated for semantic correctness, all errors collected (not fail-on-first), invalid config prevents startup with clear error
- Given broker credentials When loaded Then from env vars only (secrecy crate), never logged even at debug, .env supported for local dev (gitignored)

## Tasks / Subtasks

### Task 1: Add configuration dependencies to workspace (AC: config crate, secrecy)
- 1.1: Add `config = "0.15.22"` to workspace Cargo.toml dependencies
- 1.2: Add `secrecy = { version = "0.10", features = ["serde"] }` to workspace Cargo.toml dependencies
- 1.3: Add `dotenvy = "0.15"` for .env file loading in local dev
- 1.4: Add `config` and `dotenvy` as dependencies to `crates/engine/Cargo.toml`
- 1.5: Add `secrecy` as dependency to `crates/core/Cargo.toml` (credential types live in core)

### Task 2: Define configuration structs in core (AC: semantic validation, credential handling)
- 2.1: In `crates/core/src/config/mod.rs`, define top-level `AppConfig` struct with sections: `trading: TradingConfig`, `fees: HashMap<String, FeeConfig>`, `broker: BrokerConfig`, `signals: SignalsConfig`, `regime: RegimeConfig` — derive `Debug, Deserialize`
- 2.2: Ensure `BrokerConfig` uses `secrecy::SecretString` for credential fields (`api_key`, `api_secret`, `fcm_id`, `ib_id`), implement custom `Debug` that redacts secrets
- 2.3: Define `TradingConfig` fields: `max_position_size: u32`, `max_daily_loss_ticks: i64`, `max_consecutive_losses: u32`, `max_trades_per_day: u32`, `instruments: Vec<String>`
- 2.4: Define `FeeConfig` fields: `exchange_fee: f64`, `clearing_fee: f64`, `nfa_fee: f64`, `effective_date: chrono::NaiveDate`
- 2.5: Define `SignalsConfig` with signal-specific parameters (OBI depth, VPIN bucket size, etc.)
- 2.6: Define `RegimeConfig` with regime detection thresholds and enabled regimes

### Task 3: Implement semantic validation (AC: all errors collected, invalid prevents startup)
- 3.1: Create `crates/core/src/config/validation.rs` with `validate_config(config: &AppConfig) -> Result<(), Vec<ValidationError>>`
- 3.2: Define `ValidationError` enum with variants: `InvalidRange { field, value, min, max }`, `MissingRequired { field }`, `InvalidCombination { fields, reason }`, `StaleData { field, date, max_age }`
- 3.3: Validate trading limits: `max_position_size > 0`, `max_daily_loss_ticks > 0 && <= 2000`, `max_consecutive_losses > 0`, `max_trades_per_day > 0`
- 3.4: Validate fee schedule: `effective_date` not older than 90 days, all fee values non-negative
- 3.5: Validate broker config: required fields present (non-empty SecretString)
- 3.6: Collect ALL validation errors into a Vec before returning — never fail on first error
- 3.7: Implement `Display` for `ValidationError` with clear, actionable messages including field path and expected range

### Task 4: Implement layered config loading (AC: layers, env var override)
- 4.1: Create `crates/engine/src/config/loader.rs` with `load_config(cli_args: &CliArgs) -> Result<AppConfig>`
- 4.2: Build config using `config::Config::builder()`: add `config/default.toml` as base layer
- 4.3: Add environment-specific overlay: `config/paper.toml` or `config/live.toml` based on `cli_args.mode` (CLI flag `--mode paper|live`)
- 4.4: Add environment variable source with prefix `TRADING` and separator `__` (e.g., `TRADING__MAX_DAILY_LOSS_TICKS=800` maps to `trading.max_daily_loss_ticks`)
- 4.5: Call `dotenvy::dotenv().ok()` before config loading to support `.env` files in local dev
- 4.6: Deserialize merged config into `AppConfig`, map config crate errors to anyhow with context
- 4.7: Call `validate_config()` on loaded config, format all validation errors into a single error message if any fail

### Task 5: Create default configuration files (AC: layered config)
- 5.1: Create `config/default.toml` with all sections and sensible defaults for paper trading
- 5.2: Create `config/paper.toml` with paper-trading-specific overrides (paper broker endpoint, relaxed limits)
- 5.3: Create `config/live.toml` with live-trading-specific overrides (production endpoint, strict limits)
- 5.4: Create `.env.example` with placeholder broker credentials and documentation comments
- 5.5: Ensure `.env` is in `.gitignore`

### Task 6: Define CLI argument parsing (AC: CLI flag for mode)
- 6.1: In `crates/engine/src/config/mod.rs` or `main.rs`, define `CliArgs` struct with `mode: TradingMode` (paper/live) and `config_dir: Option<PathBuf>`
- 6.2: Use `clap` for CLI parsing with `--mode` flag defaulting to `paper`
- 6.3: Validate that `--mode live` requires explicit confirmation or additional safety flag

### Task 7: Unit tests (AC: all)
- 7.1: Test layered loading: default.toml values present, overlay values override defaults
- 7.2: Test env var override: set `TRADING__MAX_DAILY_LOSS_TICKS=800`, verify it overrides TOML value
- 7.3: Test semantic validation catches invalid values: `max_position_size = 0`, `max_daily_loss_ticks = -1`
- 7.4: Test semantic validation collects multiple errors: provide config with 3 invalid fields, verify all 3 reported
- 7.5: Test broker credentials use SecretString: verify Debug output does not contain credential values
- 7.6: Test missing required fields produce clear error messages
- 7.7: Test paper vs live mode loads correct overlay file

## Dev Notes

### Architecture Patterns & Constraints
- Config loading is step 1 of the startup sequence (Story 8.2) — all other steps depend on valid config
- The `config` crate (0.15.22) handles TOML parsing and layering natively; do not hand-roll merging logic
- Environment variable separator is `__` (double underscore) to map nested TOML keys: `[trading] max_daily_loss_ticks` = `TRADING__MAX_DAILY_LOSS_TICKS`
- Credentials MUST use `secrecy::SecretString` — this ensures they are zeroized on drop and redacted in Debug/Display. Never convert to plain String for logging
- Validation errors are collected, not fail-fast. The operator should see all config problems at once to fix them in one pass
- `.env` file is for local dev convenience only; production uses systemd environment files (Story 8.5)

### Project Structure Notes
```
crates/core/src/config/
├── mod.rs              (AppConfig, TradingConfig, BrokerConfig, etc.)
└── validation.rs       (validate_config, ValidationError)

crates/engine/src/config/
├── mod.rs              (CliArgs, re-exports)
└── loader.rs           (load_config — layered loading)

config/
├── default.toml        (base defaults)
├── paper.toml          (paper trading overlay)
└── live.toml           (live trading overlay)

.env.example            (credential placeholders)
```

### References
- config crate: https://docs.rs/config/0.15.22
- secrecy crate: https://docs.rs/secrecy/0.10
- Story 8.2 (Startup Sequence) depends on this story for config loading

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
