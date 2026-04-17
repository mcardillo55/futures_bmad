---
title: 'Address deferred code-review fixes from Epic 1'
type: 'bugfix'
created: '2026-04-16'
status: 'done'
baseline_commit: 'e05e8f0'
context: []
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** Two issues deferred from Epic 1 code review: `ConfigValidationError` implements `Display` but not `std::error::Error`, limiting composability with `?` and error crates; `FixedPrice::from_f64` silently accepts `NaN`/`Infinity`, producing nonsensical internal state.

**Approach:** Add `std::error::Error` impl for `ConfigValidationError`. Change `FixedPrice::from_f64` to return `Result<FixedPrice, NonFinitePrice>` with an `is_finite()` guard, updating all callers.

## Boundaries & Constraints

**Always:** Existing tests must continue to pass. `from_f64` signature change must propagate to every call site.

**Ask First:** If any caller is ambiguous about how to handle the error.

**Never:** Don't change `FixedPrice` internal representation or arithmetic behavior. Don't add new dependencies.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Finite f64 | `from_f64(4482.25)` | `Ok(FixedPrice(17929))` | N/A |
| NaN | `from_f64(f64::NAN)` | `Err(NonFinitePrice)` | Caller decides |
| +Infinity | `from_f64(f64::INFINITY)` | `Err(NonFinitePrice)` | Caller decides |
| -Infinity | `from_f64(f64::NEG_INFINITY)` | `Err(NonFinitePrice)` | Caller decides |
| Serde deserialize NaN | JSON NaN equivalent | Serde deserialization error | `D::Error::custom` |

</frozen-after-approval>

## Code Map

- `crates/core/src/types/fixed_price.rs` -- Add `NonFinitePrice` error, change `from_f64` return type, update `Deserialize` impl
- `crates/core/src/config/validation.rs` -- Add `impl std::error::Error for ConfigValidationError`
- `crates/testkit/src/book_builder.rs` -- Update 4 `from_f64` call sites to `.expect()`
- `crates/core/tests/fixed_price_properties.rs` -- Update property test to `.unwrap()`
- `crates/core/src/types/fixed_price.rs` (tests) -- Update unit tests, add non-finite rejection tests

## Tasks & Acceptance

**Execution:**
- [x] `crates/core/src/config/validation.rs` -- Add `impl std::error::Error for ConfigValidationError {}` after the `Display` impl
- [x] `crates/core/src/types/fixed_price.rs` -- Add `NonFinitePrice` error struct with `Display`+`Error`; change `from_f64` to return `Result<FixedPrice, NonFinitePrice>`; update `Deserialize` to use `map_err(serde::de::Error::custom)`; add unit tests for NaN/Inf rejection
- [x] `crates/testkit/src/book_builder.rs` -- Change `from_f64(x)` to `from_f64(x).expect("test price must be finite")` at 4 call sites
- [x] `crates/core/tests/fixed_price_properties.rs` -- Add `.unwrap()` to `from_f64` call in roundtrip test

**Acceptance Criteria:**
- Given `ConfigValidationError`, when used with `?` operator in a function returning `Box<dyn Error>`, then it compiles without manual conversion
- Given a non-finite f64, when passed to `from_f64`, then `Err(NonFinitePrice)` is returned
- Given a finite f64, when passed to `from_f64`, then `Ok(FixedPrice)` with correct value is returned
- Given all existing tests, when `cargo test --workspace`, then all pass

## Verification

**Commands:**
- `cargo test --workspace` -- expected: all tests pass
- `cargo clippy --workspace` -- expected: no new warnings

## Suggested Review Order

**Non-finite price guard**

- Core change: `from_f64` returns `Result` with `is_finite()` guard on input and scaled value
  [`fixed_price.rs:65`](../../crates/core/src/types/fixed_price.rs#L65)

- New error type with `Display` + `Error` impls
  [`fixed_price.rs:4`](../../crates/core/src/types/fixed_price.rs#L4)

- Serde `Deserialize` propagates error via `map_err`
  [`fixed_price.rs:37`](../../crates/core/src/types/fixed_price.rs#L37)

- Export `NonFinitePrice` from types module and crate root
  [`mod.rs:9`](../../crates/core/src/types/mod.rs#L9)
  [`lib.rs:21`](../../crates/core/src/lib.rs#L21)

**Error trait impl**

- One-liner: `ConfigValidationError` now implements `std::error::Error`
  [`validation.rs:49`](../../crates/core/src/config/validation.rs#L49)

**Callers & tests**

- Test helper uses `.expect()` for clear panic messages
  [`book_builder.rs:17`](../../crates/testkit/src/book_builder.rs#L17)

- Property test `.unwrap()` — strategy generates only finite values via integer range
  [`fixed_price_properties.rs:42`](../../crates/core/tests/fixed_price_properties.rs#L42)

- New unit test: NaN, +Inf, -Inf all rejected
  [`fixed_price.rs:153`](../../crates/core/src/types/fixed_price.rs#L153)
