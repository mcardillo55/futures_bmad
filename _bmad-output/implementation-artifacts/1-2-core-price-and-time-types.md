# Story 1.2: Core Price & Time Types

Status: done

## Story

As a developer,
I want foundational price and time types with guaranteed correctness,
So that all price arithmetic uses integer-only FixedPrice with saturating behavior and all timestamps use nanosecond precision.

## Acceptance Criteria (BDD)

- Given the `core` crate When `FixedPrice(i64)` is implemented Then it represents prices in quarter-ticks (e.g., 4482.25 -> 17929)
- And it implements `saturating_add`, `saturating_sub`, `saturating_mul` -- never panics on overflow
- And it implements `Display` for human-readable format (e.g., "4482.25") -- conversion to f64 only for display
- And it implements `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`
- And it provides `from_f64(price: f64) -> FixedPrice` for config loading (rounds via banker's rounding)
- And it provides `to_f64(&self) -> f64` explicitly marked as display-only
- Given the `core` crate When `UnixNanos(u64)` is implemented Then it represents timestamps in nanosecond precision
- And it implements `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`
- And it provides conversion to/from `chrono::DateTime` for display only
- Given the `core` crate When `Side` enum is implemented Then it has variants `Buy` and `Sell`
- Given the `core` crate When `Bar` struct is implemented Then it contains `open`, `high`, `low`, `close` as `FixedPrice`, `volume` as `u64`, and `timestamp` as `UnixNanos`
- Given property tests exist for `FixedPrice` When `proptest` runs arbitrary `i64` values through arithmetic operations Then no operation panics (saturating behavior verified)
- And `a.saturating_add(b).saturating_sub(b)` equals `a` for non-overflow cases
- And `from_f64(price).to_f64()` round-trips correctly for valid price values

## Tasks / Subtasks

### Task 1: Create module structure in core crate (AC: all types exist in core)
- [x] 1.1: Create `crates/core/src/types/mod.rs` with public module declarations for `fixed_price`, `unix_nanos`, `bar`, `side`
- [x] 1.2: Update `crates/core/src/lib.rs` to declare `pub mod types` and re-export key types

### Task 2: Implement FixedPrice (AC: quarter-tick representation, saturating arithmetic, Display, derives)
- [x] 2.1: Create `crates/core/src/types/fixed_price.rs` with `#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)] pub struct FixedPrice(pub(crate) i64)`
- [x] 2.2: Implement `FixedPrice::new(raw: i64) -> Self` constructor and `raw(&self) -> i64` accessor
- [x] 2.3: Implement `saturating_add(&self, other: FixedPrice) -> FixedPrice` using `i64::saturating_add`
- [x] 2.4: Implement `saturating_sub(&self, other: FixedPrice) -> FixedPrice` using `i64::saturating_sub`
- [x] 2.5: Implement `saturating_mul(&self, scalar: i64) -> FixedPrice` using `i64::saturating_mul`
- [x] 2.6: Implement `from_f64(price: f64) -> FixedPrice` with banker's rounding: `(price * 4.0).round() as i64` — verify rounding matches banker's (round half to even)
- [x] 2.7: Implement `to_f64(&self) -> f64` with doc comment marking it as display-only
- [x] 2.8: Implement `Display` trait: format as `self.0 as f64 / 4.0` with appropriate decimal places (2 for ES futures)
- [x] 2.9: Implement `Default` returning `FixedPrice(0)`

### Task 3: Implement UnixNanos (AC: nanosecond precision, chrono conversion, derives)
- [x] 3.1: Create `crates/core/src/types/unix_nanos.rs` with `#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)] pub struct UnixNanos(pub(crate) u64)`
- [x] 3.2: Implement `UnixNanos::new(nanos: u64) -> Self` and `as_nanos(&self) -> u64`
- [x] 3.3: Implement `From<chrono::DateTime<chrono::Utc>>` for UnixNanos
- [x] 3.4: Implement conversion method `to_datetime(&self) -> chrono::DateTime<chrono::Utc>` marked as display-only
- [x] 3.5: Implement `Default` returning `UnixNanos(0)`

### Task 4: Implement Side enum (AC: Buy and Sell variants)
- [x] 4.1: Create `crates/core/src/types/side.rs` (or include in a shared types file)
- [x] 4.2: Define `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum Side { Buy, Sell }`

### Task 5: Implement Bar struct (AC: OHLCV with FixedPrice and UnixNanos)
- [x] 5.1: Create `crates/core/src/types/bar.rs`
- [x] 5.2: Define `Bar { open: FixedPrice, high: FixedPrice, low: FixedPrice, close: FixedPrice, volume: u64, timestamp: UnixNanos }` with `Debug, Clone, Copy`

### Task 6: Write property tests (AC: saturating behavior, round-trip, no panics)
- [x] 6.1: Create `crates/core/tests/fixed_price_properties.rs`
- [x] 6.2: Property test: arbitrary `i64` values through `saturating_add`, `saturating_sub`, `saturating_mul` never panic
- [x] 6.3: Property test: `a.saturating_add(b).saturating_sub(b) == a` for non-overflow values (constrain range to avoid saturation)
- [x] 6.4: Property test: `from_f64(x).to_f64()` round-trips for valid quarter-tick prices (multiples of 0.25)
- [x] 6.5: Write unit tests for specific known values: 4482.25 -> FixedPrice(17929), 0.0 -> FixedPrice(0), negative prices

### Task 7: Write unit tests for UnixNanos
- [x] 7.1: Test chrono DateTime round-trip conversion
- [x] 7.2: Test ordering of timestamps
- [x] 7.3: Test default value

### Review Findings
- [x] [Review][Patch] UnixNanos::to_datetime panics on large u64 values [unix_nanos.rs:23] -- `(self.0 / 1_000_000_000) as i64` wraps for u64 > i64::MAX, then `.unwrap()` on `timestamp_opt` can panic. Replace `.unwrap()` with `.single()` or return `Option<DateTime<Utc>>`, or clamp the seconds value. The "never panic" design principle from the architecture extends beyond just FixedPrice. — fixed in review patch
- [x] [Review][Decision] Bar struct missing PartialEq/Eq derives [bar.rs:4] -- Spec only requires Debug/Clone/Copy, but downstream stories will almost certainly need equality comparison for tests and logic. Decide now whether to add PartialEq/Eq to Bar (trivial, no cost). — fixed in review patch

## Dev Notes

### Architecture Patterns & Constraints
- FixedPrice(i64) uses quarter-ticks: `price * 4`. Example: 4482.25 * 4 = 17929
- MANDATORY: saturating arithmetic everywhere, never panic, never silently wrap
- f64 is ONLY permitted for: signal output values, display formatting, config input (converted to FixedPrice at load time)
- Banker's rounding (round half to even) for `from_f64` — this matters for prices exactly on half-tick boundaries
- All types should be `Copy` — they are small value types passed by value on the hot path
- No heap allocation in any of these types

### Project Structure Notes
```
crates/core/src/
├── lib.rs
└── types/
    ├── mod.rs
    ├── fixed_price.rs
    ├── unix_nanos.rs
    ├── bar.rs
    └── side.rs

crates/core/tests/
└── fixed_price_properties.rs
```

### References
- Architecture document: `docs/architecture.md` — Section: Core Domain Types, FixedPrice specification
- Epics document: `docs/epics.md` — Epic 1, Story 1.2

## Dev Agent Record

### Agent Model Used
Claude Opus 4.6 (1M context)

### Debug Log References
N/A

### Completion Notes List
- FixedPrice: quarter-tick i64 with saturating arithmetic, banker's rounding, Display
- UnixNanos: u64 nanoseconds with chrono conversion
- Side: Buy/Sell enum
- Bar: OHLCV struct with FixedPrice + UnixNanos
- 8 unit tests + 5 property tests, all passing
- Zero clippy warnings

### Change Log
- 2026-04-16: All tasks completed

### File List
- crates/core/src/lib.rs (modified)
- crates/core/src/types/mod.rs (new)
- crates/core/src/types/fixed_price.rs (new)
- crates/core/src/types/unix_nanos.rs (new)
- crates/core/src/types/side.rs (new)
- crates/core/src/types/bar.rs (new)
- crates/core/tests/fixed_price_properties.rs (new)
