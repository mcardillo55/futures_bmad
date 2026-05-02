//! Regime orchestration (story 6.2).
//!
//! [`RegimeOrchestrator`] consumes regime classifications from a
//! [`RegimeDetector`](futures_bmad_core::RegimeDetector) and translates them
//! into the engine's "which strategies may initiate new entries" decision.
//! It is intentionally narrow:
//!
//!   * It accepts a [`RegimeState`] update plus a timestamp.
//!   * It emits a [`RegimeTransition`] event whenever the classifier
//!     reports something other than the currently-acted-upon regime.
//!   * It maintains the set of currently-enabled strategy names by looking
//!     up the new regime in the configured map.
//!   * It applies a cooldown to suppress rapid oscillation: transitions
//!     observed sooner than `cooldown_period_secs` after the previous
//!     acted-upon transition do not update the strategy permission set.
//!     With `conservative_on_oscillation = true` (the default) the
//!     orchestrator additionally collapses to the more conservative of the
//!     proposed and current regime so a brief blip into a permissive
//!     regime cannot enable trading.
//!
//! Hard constraint (architecture, story 6.2 AC):
//!
//! > Disabling a strategy NEVER cancels existing stop-loss orders; the
//! > orchestrator only gates new trade entry and **must not** hold any
//! > reference to order or position management.
//!
//! That constraint is enforced structurally by the orchestrator's API: it
//! takes no broker, order-manager, or position-tracker handles, and it
//! exposes no method that mutates anything outside its own state.

#![deny(unsafe_code)]

use std::collections::HashSet;

use futures_bmad_core::{
    Clock, RegimeOrchestrationConfig, RegimeState, RegimeTransition, UnixNanos,
};
use tracing::info;

use crate::persistence::{EngineEvent, JournalSender, RegimeTransitionRecord};

/// One nanosecond per second — used to compare cooldown windows in the
/// nanosecond-resolution `UnixNanos` domain.
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Conservative ranking for [`RegimeState`].
///
/// Higher numbers are MORE conservative. When two regimes are compared
/// during oscillation suppression the higher-ranked one is retained.
///
///   * `Unknown` (4)   — no classification, hardest stop on trading
///   * `Volatile` (3)  — wide-range, low-persistence; sit out
///   * `Rotational` (2) — mean-reversion regime; mid-restriction
///   * `Trending` (1)  — trend-following regime; least restrictive
const fn conservative_rank(state: RegimeState) -> u8 {
    match state {
        RegimeState::Unknown => 4,
        RegimeState::Volatile => 3,
        RegimeState::Rotational => 2,
        RegimeState::Trending => 1,
    }
}

/// Tracks the regime classifier's output and decides which strategies are
/// permitted to initiate new entries. See module-level docs for the full
/// contract.
pub struct RegimeOrchestrator {
    config: RegimeOrchestrationConfig,
    current_regime: RegimeState,
    /// Cooldown anchor — the timestamp of the last transition that *exited*
    /// cooldown (i.e. updated `current_regime` after the cooldown window
    /// elapsed, OR the first observed transition out of `Unknown`). `None`
    /// until the first transition has been processed.
    ///
    /// **Single-anchor semantics.** This anchor is set ONLY by transitions
    /// that exit cooldown. Conservative collapses inside the cooldown branch
    /// (when `conservative_on_oscillation = true`) DO update `current_regime`
    /// and `enabled_strategies`, but they DO NOT reset this anchor. By design.
    ///
    /// **Why.** The cooldown window is a debounce against rapid classifier
    /// oscillation. If a conservative collapse reset the anchor, every
    /// oscillation that flipped into a safer regime would extend the debounce
    /// indefinitely — a noisy classifier could pin the orchestrator in the
    /// safer regime forever even if conditions stabilised. Anchoring the
    /// debounce to the originating acted-upon transition keeps the cooldown
    /// window bounded and replay-deterministic w.r.t. the inputs that caused
    /// it.
    last_transition_time: Option<UnixNanos>,
    /// Number of cooldown-suppressed transitions observed since
    /// `last_transition_time`. Increments on every suppressed transition
    /// (including conservative collapses) and resets to `0` whenever a
    /// transition exits cooldown — bounding the counter at one cooldown
    /// window's worth of oscillations and making it replay-deterministic.
    oscillation_count: u32,
    enabled_strategies: HashSet<String>,
    /// Optional journal hand-off; when present, emitted [`RegimeTransition`]
    /// events are forwarded into the engine event journal's
    /// `regime_transitions` table.
    journal: Option<JournalSender>,
}

impl RegimeOrchestrator {
    /// Construct a new orchestrator. The current regime is initialised to
    /// [`RegimeState::Unknown`] and no strategies are enabled.
    pub fn new(config: RegimeOrchestrationConfig) -> Self {
        Self {
            config,
            current_regime: RegimeState::Unknown,
            last_transition_time: None,
            oscillation_count: 0,
            enabled_strategies: HashSet::new(),
            journal: None,
        }
    }

    /// Construct an orchestrator wired to a journal sender so transitions
    /// are persisted to the `regime_transitions` table in addition to
    /// being structured-logged.
    pub fn with_journal(config: RegimeOrchestrationConfig, journal: JournalSender) -> Self {
        Self {
            journal: Some(journal),
            ..Self::new(config)
        }
    }

    /// Read-only access to the active configuration.
    pub fn config(&self) -> &RegimeOrchestrationConfig {
        &self.config
    }

    /// Currently-acted-upon regime.
    pub fn current_regime(&self) -> RegimeState {
        self.current_regime
    }

    /// Read-only accessor for the current enabled-strategy set.
    pub fn enabled_strategies(&self) -> &HashSet<String> {
        &self.enabled_strategies
    }

    /// Whether the named strategy is currently permitted to initiate new
    /// entries. Used by the engine's trade-decision logic.
    pub fn is_strategy_enabled(&self, strategy_name: &str) -> bool {
        self.enabled_strategies.contains(strategy_name)
    }

    /// Number of cooldown-suppressed transitions observed since the last
    /// acted-upon transition. Resets to `0` whenever a transition exits
    /// cooldown.
    pub fn oscillation_count(&self) -> u32 {
        self.oscillation_count
    }

    /// Drive the orchestrator with the latest classifier output.
    ///
    /// Returns:
    ///   * `None` if `new_regime == current_regime` (no transition).
    ///   * `Some(RegimeTransition)` otherwise — even when the transition is
    ///     suppressed by cooldown. Callers (and the journal) should always
    ///     record the event.
    ///
    /// `clock` is accepted for API symmetry with [`RegimeDetector`] but is
    /// not required: the timestamp passed in is authoritative. The clock
    /// argument exists so future logic can compare against `clock.now()`
    /// without breaking the signature.
    pub fn on_regime_update(
        &mut self,
        new_regime: RegimeState,
        timestamp: UnixNanos,
        _clock: &dyn Clock,
    ) -> Option<RegimeTransition> {
        if new_regime == self.current_regime {
            return None;
        }

        let transition = RegimeTransition {
            from: self.current_regime,
            to: new_regime,
            timestamp,
        };

        let in_cooldown = self.is_in_cooldown(timestamp);
        if in_cooldown {
            // Surface the oscillation through structured logging and the
            // journal but do NOT update the acted-upon regime or the
            // enabled-strategy set.
            self.oscillation_count = self.oscillation_count.saturating_add(1);
            self.log_transition(&transition, true);
            self.forward_to_journal(&transition);

            if self.config.conservative_on_oscillation {
                let safer = Self::is_more_conservative(self.current_regime, new_regime);
                // Even though we suppress strategy changes, if the proposed
                // regime is MORE conservative than the current one we still
                // collapse down to it: the safety hedge says oscillation
                // should never produce a *less* restrictive state, but a
                // *more* restrictive state is fine. (For the test-case where
                // current is already the more-conservative one, this is a
                // no-op.)
                if safer != self.current_regime {
                    self.current_regime = safer;
                    self.apply_strategy_permissions(safer);
                }
            }
            return Some(transition);
        }

        // Cooldown elapsed (or first observed transition): act on it.
        self.current_regime = new_regime;
        self.last_transition_time = Some(timestamp);
        // Reset the oscillation counter atomically with the anchor advance:
        // the counter is by definition "suppressed transitions since the
        // anchor", and the anchor just moved. Replay determinism depends on
        // this reset happening here (not on entry, not in `is_in_cooldown`).
        self.oscillation_count = 0;
        self.apply_strategy_permissions(new_regime);
        self.log_transition(&transition, false);
        self.forward_to_journal(&transition);
        Some(transition)
    }

    /// Pick the more conservative of two regimes per the ordering documented
    /// in [`conservative_rank`]. Made `pub(crate)` rather than private so
    /// the unit tests in this file (and future siblings) can exercise it
    /// directly.
    pub(crate) fn is_more_conservative(a: RegimeState, b: RegimeState) -> RegimeState {
        if conservative_rank(a) >= conservative_rank(b) {
            a
        } else {
            b
        }
    }

    /// Replace the enabled-strategy set with the entries permitted in
    /// `regime`. CRITICAL: this method is the only place the orchestrator
    /// mutates strategy permissions, and it touches NOTHING outside
    /// `self.enabled_strategies`. In particular it does NOT cancel,
    /// modify, or even reference any order or position state — disabling a
    /// strategy is a *gate on new entries*, not a teardown of existing
    /// trades.
    fn apply_strategy_permissions(&mut self, regime: RegimeState) {
        self.enabled_strategies.clear();
        if let Some(allowed) = self.config.regime_strategy_map.get(&regime) {
            self.enabled_strategies.extend(allowed.iter().cloned());
        }
    }

    /// Whether `now` falls within the cooldown window after the last
    /// acted-upon transition. The very first observed transition (when
    /// `last_transition_time == None`) is never in cooldown.
    fn is_in_cooldown(&self, now: UnixNanos) -> bool {
        let Some(last) = self.last_transition_time else {
            return false;
        };
        let cooldown_nanos = self
            .config
            .cooldown_period_secs
            .saturating_mul(NANOS_PER_SECOND);
        if cooldown_nanos == 0 {
            return false;
        }
        let last_nanos = last.as_nanos();
        let now_nanos = now.as_nanos();
        // If the new timestamp is at-or-before the last (clock skew,
        // out-of-order bar) treat as in-cooldown defensively.
        if now_nanos <= last_nanos {
            return true;
        }
        (now_nanos - last_nanos) < cooldown_nanos
    }

    /// Emit a structured `tracing::info!` entry for `transition`.
    ///
    /// The `cooldown_suppressed` flag distinguishes acted-upon transitions
    /// (`false`) from oscillations swallowed by the cooldown (`true`).
    /// Designed to be filterable by `target = engine::regime::orchestrator`
    /// for downstream log shipping.
    fn log_transition(&self, transition: &RegimeTransition, cooldown_suppressed: bool) {
        let from_unknown_to_known =
            transition.from == RegimeState::Unknown && transition.to != RegimeState::Unknown;
        if from_unknown_to_known && !cooldown_suppressed {
            info!(
                target: "engine::regime::orchestrator",
                from = transition.from.as_str(),
                to = transition.to.as_str(),
                timestamp = transition.timestamp.as_nanos(),
                cooldown_suppressed = cooldown_suppressed,
                "regime detection initialised: first transition from Unknown to known regime"
            );
        } else {
            info!(
                target: "engine::regime::orchestrator",
                from = transition.from.as_str(),
                to = transition.to.as_str(),
                timestamp = transition.timestamp.as_nanos(),
                cooldown_suppressed = cooldown_suppressed,
                "regime transition"
            );
        }
    }

    /// Forward `transition` to the journal worker (if attached). The journal
    /// sender is non-blocking; on backpressure the event is dropped and a
    /// warning is emitted by the sender itself.
    fn forward_to_journal(&self, transition: &RegimeTransition) {
        if let Some(journal) = &self.journal {
            let record: RegimeTransitionRecord = (*transition).into();
            journal.send(EngineEvent::RegimeTransition(record));
        }
    }
}

/// Conversion from the core-domain [`RegimeTransition`] event to the
/// journal-side [`RegimeTransitionRecord`]. Lives here (rather than in the
/// journal module) because it is the orchestrator that owns this mapping —
/// keeping the two crates' event vocabularies decoupled.
impl From<RegimeTransition> for RegimeTransitionRecord {
    fn from(t: RegimeTransition) -> Self {
        Self {
            timestamp: t.timestamp,
            from_regime: t.from.as_str().to_string(),
            to_regime: t.to.as_str().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use futures_bmad_testkit::SimClock;

    /// Build a config with the requested cooldown and a tight strategy map
    /// that maps each regime to a single named strategy (or none) for easy
    /// assertion.
    fn test_config(
        cooldown_secs: u64,
        conservative_on_oscillation: bool,
    ) -> RegimeOrchestrationConfig {
        let mut map: HashMap<RegimeState, Vec<String>> = HashMap::new();
        map.insert(RegimeState::Trending, vec!["trend_strategy".to_string()]);
        map.insert(RegimeState::Rotational, vec!["mean_revert".to_string()]);
        map.insert(RegimeState::Volatile, Vec::new());
        map.insert(RegimeState::Unknown, Vec::new());
        RegimeOrchestrationConfig {
            regime_strategy_map: map,
            cooldown_period_secs: cooldown_secs,
            conservative_on_oscillation,
        }
    }

    fn ts_secs(s: u64) -> UnixNanos {
        UnixNanos::new(s.saturating_mul(NANOS_PER_SECOND))
    }

    /// 7.2 — Unknown -> Trending emits a RegimeTransition with the right fields.
    #[test]
    fn first_transition_emits_event_with_correct_fields() {
        let clock = SimClock::new(0);
        let mut orch = RegimeOrchestrator::new(test_config(60, true));

        let t = orch
            .on_regime_update(RegimeState::Trending, ts_secs(10), &clock)
            .expect("transition emitted");

        assert_eq!(t.from, RegimeState::Unknown);
        assert_eq!(t.to, RegimeState::Trending);
        assert_eq!(t.timestamp, ts_secs(10));
        assert_eq!(orch.current_regime(), RegimeState::Trending);
    }

    /// 7.3 — Regime change updates the enabled-strategy set per the config map.
    #[test]
    fn transition_updates_enabled_strategies() {
        let clock = SimClock::new(0);
        let mut orch = RegimeOrchestrator::new(test_config(60, true));

        orch.on_regime_update(RegimeState::Trending, ts_secs(10), &clock);
        assert!(orch.is_strategy_enabled("trend_strategy"));
        assert!(!orch.is_strategy_enabled("mean_revert"));
    }

    /// 7.4 — Same regime reported twice produces no transition.
    #[test]
    fn same_regime_returns_none() {
        let clock = SimClock::new(0);
        let mut orch = RegimeOrchestrator::new(test_config(60, true));
        orch.on_regime_update(RegimeState::Trending, ts_secs(10), &clock);
        let again = orch.on_regime_update(RegimeState::Trending, ts_secs(20), &clock);
        assert!(again.is_none());
    }

    /// 7.5 — Rapid oscillation within cooldown does NOT change enabled strategies.
    #[test]
    fn oscillation_in_cooldown_keeps_strategies_unchanged() {
        let clock = SimClock::new(0);
        let mut orch = RegimeOrchestrator::new(test_config(300, false));
        // First: Unknown -> Trending (acted upon).
        orch.on_regime_update(RegimeState::Trending, ts_secs(10), &clock);
        let baseline: HashSet<String> = orch.enabled_strategies().clone();
        // Second, within cooldown: Trending -> Rotational. Should be suppressed.
        let _ = orch.on_regime_update(RegimeState::Rotational, ts_secs(60), &clock);
        // Strategy set unchanged.
        assert_eq!(orch.enabled_strategies(), &baseline);
    }

    /// 7.6 — Rapid oscillation is still surfaced (transition event returned)
    /// and the oscillation counter increments.
    #[test]
    fn oscillation_in_cooldown_still_returns_transition() {
        let clock = SimClock::new(0);
        let mut orch = RegimeOrchestrator::new(test_config(300, false));
        orch.on_regime_update(RegimeState::Trending, ts_secs(10), &clock);
        let suppressed = orch.on_regime_update(RegimeState::Rotational, ts_secs(60), &clock);
        let suppressed = suppressed.expect("transition still emitted");
        assert_eq!(suppressed.from, RegimeState::Trending);
        assert_eq!(suppressed.to, RegimeState::Rotational);
        // And the oscillation counter records the suppression.
        assert_eq!(orch.oscillation_count(), 1);
    }

    /// `oscillation_count` resets to 0 when a transition exits cooldown.
    /// Determinism guarantee: the counter is bounded by one cooldown
    /// window's worth of oscillations and recovers identically across
    /// replay runs.
    #[test]
    fn oscillation_count_resets_after_cooldown_expiry() {
        let clock = SimClock::new(0);
        let mut orch = RegimeOrchestrator::new(test_config(60, false));
        // Acted-upon transition at t=10s — anchors the cooldown window.
        orch.on_regime_update(RegimeState::Trending, ts_secs(10), &clock);
        assert_eq!(orch.oscillation_count(), 0);

        // Three suppressed transitions within the 60s cooldown window.
        // Each proposed regime must DIFFER from current_regime (Trending) —
        // a same-regime update short-circuits before the cooldown branch.
        // current_regime stays Trending throughout suppression because
        // conservative_on_oscillation = false.
        orch.on_regime_update(RegimeState::Rotational, ts_secs(20), &clock);
        orch.on_regime_update(RegimeState::Volatile, ts_secs(30), &clock);
        orch.on_regime_update(RegimeState::Rotational, ts_secs(40), &clock);
        assert_eq!(orch.oscillation_count(), 3);
        // Anchor untouched while in cooldown; current_regime untouched.
        assert_eq!(orch.current_regime(), RegimeState::Trending);

        // Advance past the cooldown window — the next transition exits
        // cooldown and the counter must reset atomically with the anchor
        // advance.
        orch.on_regime_update(RegimeState::Rotational, ts_secs(80), &clock);
        assert_eq!(orch.current_regime(), RegimeState::Rotational);
        assert_eq!(orch.oscillation_count(), 0);
    }

    /// 7.7 — After cooldown elapses, the next transition is acted upon.
    #[test]
    fn after_cooldown_transition_is_acted_upon() {
        let clock = SimClock::new(0);
        let mut orch = RegimeOrchestrator::new(test_config(60, false));
        orch.on_regime_update(RegimeState::Trending, ts_secs(10), &clock);
        // Suppressed (within 60s cooldown).
        orch.on_regime_update(RegimeState::Rotational, ts_secs(40), &clock);
        assert_eq!(orch.current_regime(), RegimeState::Trending);
        // After cooldown elapses (>= 60s after t=10s, so t=80s).
        orch.on_regime_update(RegimeState::Rotational, ts_secs(80), &clock);
        assert_eq!(orch.current_regime(), RegimeState::Rotational);
        assert!(orch.is_strategy_enabled("mean_revert"));
        assert!(!orch.is_strategy_enabled("trend_strategy"));
    }

    /// 7.8 — `is_strategy_enabled` reflects the current regime.
    #[test]
    fn is_strategy_enabled_tracks_current_regime() {
        let clock = SimClock::new(0);
        let mut orch = RegimeOrchestrator::new(test_config(0, false));
        // Cooldown disabled (0s) so every transition is acted upon.

        // Initially Unknown -> nothing enabled.
        assert!(!orch.is_strategy_enabled("trend_strategy"));
        orch.on_regime_update(RegimeState::Trending, ts_secs(1), &clock);
        assert!(orch.is_strategy_enabled("trend_strategy"));
        orch.on_regime_update(RegimeState::Rotational, ts_secs(2), &clock);
        assert!(!orch.is_strategy_enabled("trend_strategy"));
        assert!(orch.is_strategy_enabled("mean_revert"));
    }

    /// 7.9 — Disabling a strategy does not touch any order/position state.
    /// Verified structurally: the orchestrator's public surface offers no
    /// way to reach order management, and `apply_strategy_permissions` only
    /// mutates the in-memory `enabled_strategies` set. We assert the type
    /// has no broker/order/position fields by checking that constructing
    /// it requires nothing of the sort.
    #[test]
    fn disabling_strategy_has_no_access_to_order_state() {
        let clock = SimClock::new(0);
        // Construction requires only a config — no broker, no order manager,
        // no position store. If a future regression added one of those,
        // this test would no longer compile.
        let mut orch = RegimeOrchestrator::new(test_config(0, false));
        orch.on_regime_update(RegimeState::Trending, ts_secs(1), &clock);
        // Transition Trending -> Volatile (which empties the strategy set):
        orch.on_regime_update(RegimeState::Volatile, ts_secs(2), &clock);
        assert!(orch.enabled_strategies().is_empty());
        // The orchestrator state is the only thing that changed; no
        // observable side-effect on any external system because none is
        // wired in (and structurally cannot be).
    }

    /// 7.10 — First transition from Unknown to a known regime is logged
    /// (we cannot easily assert the log line content without a tracing test
    /// subscriber, but we can assert the transition event itself was
    /// returned and the orchestrator left the Unknown state).
    #[test]
    fn first_unknown_to_known_transition_is_acted_upon_and_logged() {
        let clock = SimClock::new(0);
        let mut orch = RegimeOrchestrator::new(test_config(60, true));
        let t = orch
            .on_regime_update(RegimeState::Rotational, ts_secs(5), &clock)
            .expect("transition emitted");
        assert_eq!(t.from, RegimeState::Unknown);
        assert_eq!(t.to, RegimeState::Rotational);
        assert_eq!(orch.current_regime(), RegimeState::Rotational);
    }

    /// 7.11 — `conservative_on_oscillation` keeps the more restrictive
    /// regime during rapid changes. From Trending (least restrictive),
    /// during cooldown the proposed Volatile (more restrictive) collapses
    /// the orchestrator into Volatile even though strategy permissions are
    /// suppressed in the normal cooldown sense.
    #[test]
    fn conservative_on_oscillation_collapses_to_safer_regime() {
        let clock = SimClock::new(0);
        let mut orch = RegimeOrchestrator::new(test_config(300, true));
        orch.on_regime_update(RegimeState::Trending, ts_secs(10), &clock);
        assert!(orch.is_strategy_enabled("trend_strategy"));
        assert_eq!(orch.oscillation_count(), 0);
        // Within cooldown, propose Volatile — more conservative.
        orch.on_regime_update(RegimeState::Volatile, ts_secs(20), &clock);
        assert_eq!(orch.current_regime(), RegimeState::Volatile);
        assert!(!orch.is_strategy_enabled("trend_strategy"));
        // Counter increments on the suppressed-but-collapsed transition.
        assert_eq!(orch.oscillation_count(), 1);
        // ...but propose Rotational (less conservative than Volatile) and
        // we should NOT degrade further.
        orch.on_regime_update(RegimeState::Rotational, ts_secs(30), &clock);
        assert_eq!(orch.current_regime(), RegimeState::Volatile);
        // Counter still increments — every suppressed transition counts,
        // collapse or no collapse.
        assert_eq!(orch.oscillation_count(), 2);
    }

    /// `is_more_conservative` honours the documented ordering.
    #[test]
    fn conservative_ordering_is_correct() {
        // Unknown beats everything.
        assert_eq!(
            RegimeOrchestrator::is_more_conservative(RegimeState::Unknown, RegimeState::Volatile),
            RegimeState::Unknown
        );
        // Volatile beats Rotational.
        assert_eq!(
            RegimeOrchestrator::is_more_conservative(
                RegimeState::Rotational,
                RegimeState::Volatile,
            ),
            RegimeState::Volatile
        );
        // Rotational beats Trending.
        assert_eq!(
            RegimeOrchestrator::is_more_conservative(
                RegimeState::Rotational,
                RegimeState::Trending,
            ),
            RegimeState::Rotational
        );
        // Tie returns the first argument (deterministic).
        assert_eq!(
            RegimeOrchestrator::is_more_conservative(RegimeState::Trending, RegimeState::Trending),
            RegimeState::Trending
        );
    }

    /// Cooldown of 0 disables the suppression entirely.
    #[test]
    fn cooldown_zero_disables_suppression() {
        let clock = SimClock::new(0);
        let mut orch = RegimeOrchestrator::new(test_config(0, false));
        orch.on_regime_update(RegimeState::Trending, ts_secs(1), &clock);
        orch.on_regime_update(RegimeState::Rotational, ts_secs(1), &clock);
        assert_eq!(orch.current_regime(), RegimeState::Rotational);
    }

    /// `RegimeTransition` -> `RegimeTransitionRecord` round-trips the fields.
    #[test]
    fn regime_transition_to_record_round_trip() {
        let t = RegimeTransition {
            from: RegimeState::Unknown,
            to: RegimeState::Trending,
            timestamp: ts_secs(42),
        };
        let r: RegimeTransitionRecord = t.into();
        assert_eq!(r.from_regime, "Unknown");
        assert_eq!(r.to_regime, "Trending");
        assert_eq!(r.timestamp, ts_secs(42));
    }
}
