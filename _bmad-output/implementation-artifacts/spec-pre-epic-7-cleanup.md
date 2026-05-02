---
title: 'Close pre-Epic-7 determinism prep items (A-1/A-2/A-3)'
type: 'refactor'
created: '2026-05-01'
status: 'done'
baseline_commit: 'ba8ef22'
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-6-retro-2026-05-01.md'
  - '{project-root}/_bmad-output/implementation-artifacts/deferred-work.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** Three Epic 6 retrospective action items are gating Story 7.1 / 7.2 because they are determinism risks under SimClock-driven replay: (A-1) `RegimeOrchestrator::pending_transitions: Vec<RegimeTransition>` accumulates every cooldown-suppressed oscillation and is never drained, so `RegimeOrchestrator` state is unbounded and not bit-identical between replay runs that observe different oscillation counts; (A-2) `last_transition_time` field doc says "last timestamp at which a transition was acted upon," but a conservative collapse in the cooldown branch IS an acted-upon state change that does NOT reset the cooldown anchor — the doc misleads anyone trying to reason about replay determinism around cooldown boundaries; (A-3) `compute_atr` computes `r1 = high - low` without `.abs()`, so malformed bars (`high < low`) silently understate True Range — latent for fuzz/replay where data invariants are not guaranteed.

**Approach:** Mirror the `spec-pre-epic-6-cleanup` pattern: one focused commit closes A-1, A-2, A-3 before Story 7.1 starts. (A-1) Replace `pending_transitions: Vec<RegimeTransition>` with `oscillation_count: u32`; increment in the cooldown branch; **reset to 0 in the non-cooldown path** so the counter is bounded by the worst-case oscillations within a single cooldown window. Replace the public accessor with `oscillation_count(&self) -> u32`. Update test 7.6 to assert the counter; add a test asserting reset-on-cooldown-expiry. (A-2) Rewrite the `last_transition_time` field doc to state single-anchor semantics explicitly: the anchor is set ONLY by transitions that exit cooldown; conservative collapses update `current_regime` / `enabled_strategies` but DO NOT reset the anchor, and that is by design so the cooldown window stays deterministic w.r.t. the originating acted-upon transition. (A-3) Change `let r1 = high - low;` to `let r1 = (high - low).abs();`. Add a unit test exercising a `high < low` bar.

## Boundaries & Constraints

**Always:**
- `oscillation_count` resets to 0 inside the non-cooldown branch (after `last_transition_time = Some(timestamp)`), BEFORE returning the acted-upon transition. This is the determinism guarantee — the counter is bounded by `cooldown_period_secs / bar_period_secs` and recovers identically across replay runs.
- Field `oscillation_count` is `u32`; increments use `saturating_add(1)` so a pathological run cannot panic in release. The saturated ceiling is far above any realistic per-cooldown oscillation count.
- `last_transition_time` doc must explicitly call out that conservative collapses do NOT reset the anchor and explain why (replay determinism + cooldown-window semantics anchored to acted-upon-after-cooldown transitions).
- Existing public surface other than the renamed accessor is preserved. `RegimeOrchestrator::on_regime_update` signature, `RegimeTransition` struct, `RegimeOrchestrationConfig` are untouched.
- `compute_atr` change is one-character semantically equivalent for all `high >= low` bars; only the malformed-bar branch behaves differently.

**Ask First:**
- If renaming `pending_transitions()` → `oscillation_count()` breaks any caller outside `crates/engine/src/regime/`, surface the call sites before changing.

**Never:**
- Do NOT add a Tokio runtime, channel, or any new I/O to either module.
- Do NOT change cooldown semantics (timing, conservative-collapse logic, `is_more_conservative` ordering). Only the bookkeeping representation changes.
- Do NOT touch any deferred items not listed (A-4 / A-5 / A-6 / A-7 stay deferred).

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Acted-upon transition (cooldown elapsed or first observed) | `on_regime_update(new, ts, _)` with `ts` outside cooldown | `current_regime`, `enabled_strategies`, `last_transition_time` updated; `oscillation_count` reset to `0`; transition returned | N/A |
| Cooldown-suppressed transition (`conservative_on_oscillation = false`) | Within cooldown window | `current_regime`, `enabled_strategies`, `last_transition_time` UNCHANGED; `oscillation_count += 1`; transition still returned | N/A |
| Cooldown-suppressed conservative collapse | Within cooldown, proposed regime more conservative | `current_regime` and `enabled_strategies` collapse to safer regime; `last_transition_time` UNCHANGED (single-anchor); `oscillation_count += 1` | N/A |
| Counter resets across cooldown window | suppressed → suppressed → cooldown elapses → acted-upon | After acted-upon transition `oscillation_count == 0` regardless of prior suppressed count | N/A |
| Malformed bar (`high < low`) in ATR window | `Bar { high: 100, low: 105, close, open }` | `r1` contributes `|high - low| = 5` to True Range, not `-5` | N/A |
| Counter saturation | `u32::MAX` suppressed transitions in one cooldown window | `oscillation_count` saturates at `u32::MAX`, no panic | N/A |

</frozen-after-approval>

## Code Map

- `crates/engine/src/regime/orchestrator.rs` — A-1 + A-2: replace `pending_transitions: Vec<RegimeTransition>` field (line 77) with `oscillation_count: u32`; remove the `pending_transitions()` accessor (line 132) and add `oscillation_count(&self) -> u32`; in the cooldown branch (line 165) replace `self.pending_transitions.push(transition)` with `self.oscillation_count = self.oscillation_count.saturating_add(1)`; in the non-cooldown branch (line 190) reset `self.oscillation_count = 0` after `self.last_transition_time = Some(timestamp)`; rewrite the `last_transition_time` field doc (lines 70–72) with single-anchor semantics. Update test 7.6 (`oscillation_in_cooldown_still_returns_transition`, line 387) to assert the counter; add a new test `oscillation_count_resets_after_cooldown_expiry`.
- `crates/engine/src/regime/threshold.rs` — A-3: change `let r1 = high - low;` (line 155) to `let r1 = (high - low).abs();`. Add a `#[cfg(test)] fn` covering the `high < low` malformed-bar case in the existing tests module.
- `_bmad-output/implementation-artifacts/deferred-work.md` — mark 6.2 S-1, 6.2 S-2, 6.1 N-2 `[x]` with one-line resolution notes (commit hash). Leave A-4 / A-5 / A-6 / A-7 entries untouched.

## Tasks & Acceptance

**Execution:**
- [x] `crates/engine/src/regime/orchestrator.rs` -- replace `pending_transitions: Vec<RegimeTransition>` with `oscillation_count: u32`; replace pushes with `saturating_add(1)`; reset to `0` in the non-cooldown branch; replace accessor with `oscillation_count() -> u32` -- A-1 fix.
- [x] `crates/engine/src/regime/orchestrator.rs` -- rewrite `last_transition_time` field doc explaining single-anchor semantics (~5 LoC; conservative collapses do NOT reset the anchor, by design, for replay determinism) -- A-2 fix.
- [x] `crates/engine/src/regime/orchestrator.rs` (tests) -- update `oscillation_in_cooldown_still_returns_transition` (test 7.6) to assert `orch.oscillation_count() == 1` instead of `pending_transitions().len() == 1`; add new `oscillation_count_resets_after_cooldown_expiry` test that runs N suppressed transitions then advances past cooldown and asserts the counter is `0`.
- [x] `crates/engine/src/regime/threshold.rs` -- change `let r1 = high - low;` to `let r1 = (high - low).abs();` in `compute_atr` -- A-3 fix.
- [x] `crates/engine/src/regime/threshold.rs` (tests) -- add `compute_atr_handles_malformed_bar_with_low_above_high` (or similar) asserting True Range correctness when a single bar has `high < low`.
- [x] `_bmad-output/implementation-artifacts/deferred-work.md` -- mark 6.2 S-1 (line 143), 6.2 S-2 (line 144), 6.1 N-2 (line 150) `[x]` with commit-hash resolution notes.

**Acceptance Criteria:**
- Given `conservative_on_oscillation = false` and a fresh orchestrator, when N transitions arrive within one cooldown window followed by one transition outside the window, then `oscillation_count == 0` after the final transition.
- Given `RegimeOrchestrator` source, when `rg 'pending_transitions' crates/engine/`, then zero matches.
- Given the `last_transition_time` field doc comment, when read by a future maintainer, then it explicitly states (a) the anchor is set ONLY by acted-upon-after-cooldown transitions, (b) conservative collapses in the cooldown branch do NOT reset the anchor, and (c) why this matters for replay determinism.
- Given a `Bar { high: 100, low: 105, .. }` (malformed), when `compute_atr` includes that bar in its window, then the bar contributes `|high - low|` to True Range, not the negative raw difference.
- Given the workspace, when `cargo build --workspace`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings` run, then all succeed with no new warnings.

## Spec Change Log

(empty)

## Design Notes

**A-1 reset placement.** The reset must happen inside the non-cooldown branch AFTER `last_transition_time = Some(timestamp)` so that the counter and the anchor advance atomically. Resetting in `on_regime_update`'s entry would lose the count-since-anchor invariant; resetting in `is_in_cooldown` would conflate a query method with state mutation.

**A-1 saturating_add.** Realistic per-cooldown oscillation counts are O(cooldown_secs / bar_secs) — at the architecture default (300 s cooldown, 1 s bars) that is 300, far below `u32::MAX`. `saturating_add` is paranoid against pathological future bar cadences and pathological replay corpora.

**A-2 why the anchor doesn't reset on collapse.** The cooldown window is a debounce against rapid classifier oscillation. If a conservative collapse reset the anchor, every oscillation that happened to flip into a safer regime would extend the debounce window indefinitely — a noisy classifier could pin the orchestrator in the safer regime forever even if conditions stabilised. Single-anchor semantics keep the debounce bounded by the originating transition.

**A-3 latent risk.** Real CME data always has `high >= low`. The `.abs()` change matters only for fuzz/replay corpora that may not enforce that invariant — exactly the regime Story 7.2 will exercise. One-character fix; no behavior change for live data.

## Verification

**Commands:**
- `cargo build --workspace` -- expected: clean build.
- `cargo test --workspace --no-fail-fast` -- expected: existing + new tests pass; all 11 threshold-detector tests + 13 orchestrator tests + 5 config tests + new oscillation-count-reset + new ATR malformed-bar test all green.
- `cargo clippy --workspace --all-targets -- -D warnings` -- expected: no new lints.

**Manual checks:**
- `rg 'pending_transitions' crates/engine/` -- expected: zero matches.
- `rg 'oscillation_count' crates/engine/src/regime/orchestrator.rs` -- expected: ≥4 matches (field + accessor + increment + reset).

## Suggested Review Order

**A-2 — `last_transition_time` single-anchor semantics**

- Field doc rewrite: anchor set ONLY by exit-cooldown transitions; collapses do NOT reset; rationale (replay determinism + bounded debounce).
  [`orchestrator.rs:88`](../../crates/engine/src/regime/orchestrator.rs#L88)

**A-1 — `oscillation_count: u32` replaces unbounded `Vec`**

- New field replacing `pending_transitions: Vec<RegimeTransition>` — finite-domain state for replay determinism.
  [`orchestrator.rs:94`](../../crates/engine/src/regime/orchestrator.rs#L94)

- Suppressed transitions increment via `saturating_add(1)` — paranoid against pathological replay corpora.
  [`orchestrator.rs:187`](../../crates/engine/src/regime/orchestrator.rs#L187)

- Counter reset is atomic with anchor advance: zeroed AFTER `last_transition_time = Some(timestamp)`, BEFORE strategy permissions reapply.
  [`orchestrator.rs:215`](../../crates/engine/src/regime/orchestrator.rs#L215)

- Public accessor — diagnostic surface for downstream replay analysis.
  [`orchestrator.rs:150`](../../crates/engine/src/regime/orchestrator.rs#L150)

**A-3 — ATR robust to malformed bars (high < low)**

- One-character correctness fix: `r1` now uses `|high - low|` like its `r2` / `r3` siblings.
  [`threshold.rs:155`](../../crates/engine/src/regime/threshold.rs#L155)

**Tests**

- Counter resets to 0 across cooldown boundaries — locks the determinism guarantee against future regressions.
  [`orchestrator.rs:428`](../../crates/engine/src/regime/orchestrator.rs#L428)

- Conservative collapse path now asserts counter increments — covers I/O matrix row added during review.
  [`orchestrator.rs:531`](../../crates/engine/src/regime/orchestrator.rs#L531)

- Test 7.6 updated to assert counter instead of vec length.
  [`orchestrator.rs:420`](../../crates/engine/src/regime/orchestrator.rs#L420)

- Malformed bar with `prev_close` between swapped `high` / `low` — only `r1` produces correct TR magnitude.
  [`threshold.rs:479`](../../crates/engine/src/regime/threshold.rs#L479)

**Bookkeeping**

- 6.2 S-1 / 6.2 S-2 / 6.1 N-2 marked resolved; three new defers from this review (cooldown same-timestamp, cooldown-secs validation, atr_period validation) appended.
  [`deferred-work.md`](./deferred-work.md)
