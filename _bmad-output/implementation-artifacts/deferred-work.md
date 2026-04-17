# Deferred Work

## Deferred from: code review of story-1.6 (2026-04-16)

- [x] `ConfigValidationError` missing `std::error::Error` impl [crates/core/src/config/validation.rs:3] — fixed in spec-deferred-review-fixes
- [x] `FixedPrice::from_f64` accepts non-finite values [crates/core/src/types/fixed_price.rs:53] — fixed in spec-deferred-review-fixes

## Deferred from: code review of story-2.1 (2026-04-16)

- Env var test thread safety — `unsafe set_var/remove_var` in `connection.rs` tests may race in parallel test execution. Pre-existing Rust limitation with env var mutation in tests.
