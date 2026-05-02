//! Event-aware trading windows — story 5.5.
//!
//! Wall-clock-scheduled restriction layer for high-impact news releases
//! (FOMC, CPI, NFP, ...). The [`EventWindowManager`] holds a vector of
//! pre-resolved windows; on each event-loop tick the engine asks
//! [`EventWindowManager::check_active_events`] which windows are currently
//! live and applies the most restrictive [`TradingRestriction`] returned by
//! [`EventWindowManager::get_trading_restriction`].
//!
//! Key invariants
//! --------------
//!   * **Wall-clock scheduled.** All time comparisons go through
//!     [`Clock::wall_clock`] — never `Clock::now()` (exchange time). In
//!     replay, `SimClock`'s wall_clock drives event activation so the
//!     simulation is deterministic.
//!   * **Separate from circuit breakers.** Event windows do NOT trip
//!     breakers. They are an orthogonal restriction layer that the event
//!     loop checks AFTER the circuit-breaker `permits_trading()` gate.
//!   * **Most-restrictive wins.** When multiple windows overlap, the
//!     restriction with the highest [`EventAction::severity`] is returned.
//!   * **Edge-triggered logging.** Activation and resumption are logged
//!     once per state transition; never per tick.
//!
//! `unsafe` is forbidden in this module (see `#![deny(unsafe_code)]` in
//! the module-level declaration of `risk` and the per-file declaration
//! below).

#![deny(unsafe_code)]

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use futures_bmad_core::{Clock, EventAction, EventWindowConfig};
use tracing::info;

/// Trading restriction surfaced by the manager to the event loop.
///
/// Mirrors [`EventAction`] one-for-one. Kept as a separate type so the
/// event loop's match doesn't have to import the config-side enum and so
/// future restrictions (synthetic, regime-driven, ...) can be added
/// without touching `core::config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradingRestriction {
    DisableStrategies,
    ReduceExposure,
    SitOut,
}

impl TradingRestriction {
    /// Convert from the config-side action.
    pub const fn from_action(action: EventAction) -> Self {
        match action {
            EventAction::DisableStrategies => TradingRestriction::DisableStrategies,
            EventAction::ReduceExposure => TradingRestriction::ReduceExposure,
            EventAction::SitOut => TradingRestriction::SitOut,
        }
    }

    /// Severity ranking — higher is more restrictive.
    pub const fn severity(self) -> u8 {
        match self {
            TradingRestriction::DisableStrategies => 1,
            TradingRestriction::ReduceExposure => 2,
            TradingRestriction::SitOut => 3,
        }
    }
}

/// One currently-active event window — surfaced to the event loop on
/// every `check_active_events` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveEvent {
    pub name: String,
    pub action: EventAction,
    /// Time remaining until the window expires. Always `>= 0` because we
    /// only return events that are currently active.
    pub remaining: Duration,
}

/// Pre-resolved internal window. The manager pre-computes the resolved
/// end (`start + duration_minutes` if duration was specified) at
/// construction time so the hot-path comparison is two simple
/// `DateTime` ordering checks.
struct ResolvedWindow {
    name: String,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    action: EventAction,
    /// Last observed activation state. Used to drive edge-triggered
    /// activation/resumption logging.
    is_active: bool,
}

impl ResolvedWindow {
    /// Compute the resolved window from a raw config entry.
    ///
    /// Validation (one-of-end-or-duration, non-zero, end > start) is
    /// performed by `core::config::validate_event_window_config` —
    /// upstream callers must run validation first. As a defence-in-depth
    /// fallback we still return `None` if the config is internally
    /// inconsistent so `EventWindowManager::new` cannot panic on a
    /// malformed entry.
    fn from_config(config: &EventWindowConfig) -> Option<Self> {
        let start = naive_to_utc(config.start);
        let end = match (config.end, config.duration_minutes) {
            (Some(end), None) => naive_to_utc(end),
            (None, Some(minutes)) if minutes > 0 => {
                start.checked_add_signed(Duration::minutes(minutes as i64))?
            }
            _ => return None,
        };
        if end <= start {
            return None;
        }
        Some(Self {
            name: config.name.clone(),
            start,
            end,
            action: config.action,
            is_active: false,
        })
    }
}

/// TOML naive timestamps are treated as UTC — config files are documented
/// to keep event start/end strings in UTC to avoid DST surprises.
fn naive_to_utc(naive: NaiveDateTime) -> DateTime<Utc> {
    DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc)
}

/// Manages the configured event-window restrictions.
///
/// Construct once at engine startup with the `events: Vec<EventWindowConfig>`
/// field from `TradingConfig`. The manager is `Send` (windows are `String`/
/// `DateTime`/`bool`) but is intended to be called from a single thread
/// — typically the engine event loop. `check_active_events` mutates the
/// `is_active` field on each window for edge-triggered logging.
pub struct EventWindowManager {
    windows: Vec<ResolvedWindow>,
}

impl EventWindowManager {
    /// Construct a manager from a slice of validated [`EventWindowConfig`]
    /// entries. Internally inconsistent entries (duration = 0, end before
    /// start, both end and duration set, neither set) are silently
    /// dropped — caller is expected to have run validation first.
    pub fn new(configs: &[EventWindowConfig]) -> Self {
        let windows = configs
            .iter()
            .filter_map(ResolvedWindow::from_config)
            .collect();
        Self { windows }
    }

    /// Whether any windows are configured.
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// Number of windows.
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// Examine every configured window and return the active set.
    ///
    /// A window is active when `start <= now < end` (right-open interval).
    /// The right-open interval is essential: `end` is the "first instant
    /// after the restriction lifts" so that two adjacent windows
    /// (ending and starting at the same wall-clock instant) do not
    /// double-count.
    ///
    /// Edge-triggered logging:
    ///   * `inactive -> active`: `info!` with name, action, start, end.
    ///   * `active -> inactive`: `info!` with name, duration the window
    ///     was active.
    ///
    /// No log line is emitted on subsequent ticks while the state is
    /// unchanged — see story 5.5 task 4.4.
    pub fn check_active_events(&mut self, clock: &dyn Clock) -> Vec<ActiveEvent> {
        let now = clock.wall_clock();
        let mut active = Vec::new();
        for window in &mut self.windows {
            let is_active_now = now >= window.start && now < window.end;

            // Edge-triggered logging.
            match (window.is_active, is_active_now) {
                (false, true) => {
                    info!(
                        target: "event_windows",
                        event_name = %window.name,
                        action = ?window.action,
                        start = %window.start,
                        expected_end = %window.end,
                        "event window activated — trading restriction in effect"
                    );
                }
                (true, false) => {
                    let duration_secs = (window.end - window.start).num_seconds().max(0);
                    info!(
                        target: "event_windows",
                        event_name = %window.name,
                        action = ?window.action,
                        duration_seconds = duration_secs,
                        "event window expired — normal trading resumed"
                    );
                }
                _ => {}
            }
            window.is_active = is_active_now;

            if is_active_now {
                active.push(ActiveEvent {
                    name: window.name.clone(),
                    action: window.action,
                    remaining: window.end - now,
                });
            }
        }
        active
    }

    /// Most-restrictive trading restriction currently in effect, or
    /// `None` when no event window is active.
    ///
    /// Idempotent in the sense that two consecutive calls with the same
    /// clock observation produce the same answer — but the call DOES
    /// drive edge-triggered logging through the underlying
    /// [`Self::check_active_events`].
    pub fn get_trading_restriction(&mut self, clock: &dyn Clock) -> Option<TradingRestriction> {
        let active = self.check_active_events(clock);
        active
            .iter()
            .map(|e| TradingRestriction::from_action(e.action))
            .max_by_key(|r| r.severity())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use futures_bmad_core::UnixNanos;
    use futures_bmad_testkit::SimClock;

    fn naive(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(hh, mm, 0)
            .unwrap()
    }

    /// SimClock starting at the given wall-clock instant.
    fn sim_at(dt: DateTime<Utc>) -> SimClock {
        let nanos = dt.timestamp_nanos_opt().expect("in-range") as u64;
        SimClock::new(nanos)
    }

    fn fomc_duration_event() -> EventWindowConfig {
        EventWindowConfig {
            name: "FOMC".into(),
            start: naive(2026, 4, 16, 14, 0),
            end: None,
            duration_minutes: Some(120),
            action: EventAction::SitOut,
        }
    }

    fn cpi_end_event() -> EventWindowConfig {
        EventWindowConfig {
            name: "CPI".into(),
            start: naive(2026, 5, 13, 8, 30),
            end: Some(naive(2026, 5, 13, 9, 0)),
            duration_minutes: None,
            action: EventAction::DisableStrategies,
        }
    }

    fn nfp_reduce_event() -> EventWindowConfig {
        EventWindowConfig {
            name: "NFP".into(),
            start: naive(2026, 5, 1, 8, 30),
            end: None,
            duration_minutes: Some(60),
            action: EventAction::ReduceExposure,
        }
    }

    /// 7.1 — event window activates when clock.wall_clock() falls within range.
    #[test]
    fn activates_when_within_range() {
        let mut mgr = EventWindowManager::new(&[fomc_duration_event()]);
        let inside = Utc.with_ymd_and_hms(2026, 4, 16, 14, 30, 0).unwrap();
        let clock = sim_at(inside);

        let active = mgr.check_active_events(&clock);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "FOMC");
        assert_eq!(active[0].action, EventAction::SitOut);
    }

    /// 7.2 — event window does not activate before start time.
    #[test]
    fn does_not_activate_before_start() {
        let mut mgr = EventWindowManager::new(&[fomc_duration_event()]);
        let before = Utc.with_ymd_and_hms(2026, 4, 16, 13, 59, 59).unwrap();
        let clock = sim_at(before);

        let active = mgr.check_active_events(&clock);
        assert!(active.is_empty());
    }

    /// 7.3 — event window deactivates after end time, trading resumes.
    #[test]
    fn deactivates_after_end_time() {
        let mut mgr = EventWindowManager::new(&[fomc_duration_event()]);

        // Inside.
        let inside = Utc.with_ymd_and_hms(2026, 4, 16, 14, 30, 0).unwrap();
        let clock = sim_at(inside);
        assert_eq!(mgr.check_active_events(&clock).len(), 1);

        // After: start (14:00) + 120 minutes = 16:00. Pick 16:01.
        let after = Utc.with_ymd_and_hms(2026, 4, 16, 16, 1, 0).unwrap();
        let nanos = after.timestamp_nanos_opt().unwrap() as u64;
        clock.set_time(UnixNanos::new(nanos));

        let active = mgr.check_active_events(&clock);
        assert!(active.is_empty(), "expected no active windows after end");
    }

    /// 7.4 — duration-based events compute correct end time from start + duration.
    #[test]
    fn duration_based_end_resolution() {
        let cfg = fomc_duration_event();
        let resolved = ResolvedWindow::from_config(&cfg).expect("valid config");
        let expected_end = Utc.with_ymd_and_hms(2026, 4, 16, 16, 0, 0).unwrap();
        assert_eq!(resolved.end, expected_end);
    }

    /// 7.4 (variant) — explicit `end` is used as-is.
    #[test]
    fn explicit_end_is_used() {
        let cfg = cpi_end_event();
        let resolved = ResolvedWindow::from_config(&cfg).expect("valid config");
        let expected_end = Utc.with_ymd_and_hms(2026, 5, 13, 9, 0, 0).unwrap();
        assert_eq!(resolved.end, expected_end);
    }

    /// 7.5 — DisableStrategies surfaces as the corresponding TradingRestriction.
    #[test]
    fn disable_strategies_action_yields_disable_restriction() {
        let mut mgr = EventWindowManager::new(&[cpi_end_event()]);
        let inside = Utc.with_ymd_and_hms(2026, 5, 13, 8, 45, 0).unwrap();
        let clock = sim_at(inside);

        let restriction = mgr.get_trading_restriction(&clock);
        assert_eq!(restriction, Some(TradingRestriction::DisableStrategies));
    }

    /// 7.6 — ReduceExposure surfaces as the corresponding restriction.
    #[test]
    fn reduce_exposure_action_yields_reduce_restriction() {
        let mut mgr = EventWindowManager::new(&[nfp_reduce_event()]);
        let inside = Utc.with_ymd_and_hms(2026, 5, 1, 9, 0, 0).unwrap();
        let clock = sim_at(inside);

        let restriction = mgr.get_trading_restriction(&clock);
        assert_eq!(restriction, Some(TradingRestriction::ReduceExposure));
    }

    /// 7.7 — SitOut surfaces as the corresponding (most restrictive) restriction.
    #[test]
    fn sit_out_action_yields_sit_out_restriction() {
        let mut mgr = EventWindowManager::new(&[fomc_duration_event()]);
        let inside = Utc.with_ymd_and_hms(2026, 4, 16, 14, 30, 0).unwrap();
        let clock = sim_at(inside);

        let restriction = mgr.get_trading_restriction(&clock);
        assert_eq!(restriction, Some(TradingRestriction::SitOut));
    }

    /// 7.8 — multiple simultaneous events: most restrictive action wins.
    #[test]
    fn most_restrictive_when_overlapping() {
        // Two overlapping windows — DisableStrategies and SitOut both active
        // at 14:30. SitOut is more restrictive and must win.
        let lower = EventWindowConfig {
            name: "lower".into(),
            start: naive(2026, 4, 16, 14, 0),
            end: None,
            duration_minutes: Some(60),
            action: EventAction::DisableStrategies,
        };
        let higher = EventWindowConfig {
            name: "higher".into(),
            start: naive(2026, 4, 16, 14, 15),
            end: None,
            duration_minutes: Some(30),
            action: EventAction::SitOut,
        };
        let mut mgr = EventWindowManager::new(&[lower, higher]);
        let inside = Utc.with_ymd_and_hms(2026, 4, 16, 14, 30, 0).unwrap();
        let clock = sim_at(inside);

        let active = mgr.check_active_events(&clock);
        assert_eq!(active.len(), 2);
        assert_eq!(
            mgr.get_trading_restriction(&clock),
            Some(TradingRestriction::SitOut),
            "SitOut must beat DisableStrategies"
        );
    }

    /// 7.8 (variant) — ReduceExposure beats DisableStrategies.
    #[test]
    fn reduce_exposure_beats_disable_strategies() {
        let mut mgr = EventWindowManager::new(&[cpi_end_event(), nfp_reduce_event()]);
        // CPI alone (DisableStrategies) at 8:45.
        let only_cpi = Utc.with_ymd_and_hms(2026, 5, 13, 8, 45, 0).unwrap();
        let clock_cpi = sim_at(only_cpi);
        assert_eq!(
            mgr.get_trading_restriction(&clock_cpi),
            Some(TradingRestriction::DisableStrategies)
        );

        // NFP alone (ReduceExposure) at 9:00 on 2026-05-01.
        let only_nfp = Utc.with_ymd_and_hms(2026, 5, 1, 9, 0, 0).unwrap();
        let nanos = only_nfp.timestamp_nanos_opt().unwrap() as u64;
        clock_cpi.set_time(UnixNanos::new(nanos));
        assert_eq!(
            mgr.get_trading_restriction(&clock_cpi),
            Some(TradingRestriction::ReduceExposure)
        );
    }

    /// 7.9 — with SimClock, event activation is deterministic based on
    /// simulated wall time. Same time -> same answer, repeatable.
    #[test]
    fn deterministic_with_sim_clock() {
        let mut mgr_a = EventWindowManager::new(&[fomc_duration_event()]);
        let mut mgr_b = EventWindowManager::new(&[fomc_duration_event()]);

        let inside = Utc.with_ymd_and_hms(2026, 4, 16, 14, 30, 0).unwrap();
        let clock_a = sim_at(inside);
        let clock_b = sim_at(inside);

        let restriction_a = mgr_a.get_trading_restriction(&clock_a);
        let restriction_b = mgr_b.get_trading_restriction(&clock_b);
        assert_eq!(restriction_a, restriction_b);
        assert_eq!(restriction_a, Some(TradingRestriction::SitOut));
    }

    /// 7.10 — lifecycle logging: activation and resumption events occur on
    /// transitions only. Verified by tracking the underlying `is_active`
    /// state that the edge-triggered log block reads.
    #[test]
    fn lifecycle_logs_only_on_transitions() {
        let mut mgr = EventWindowManager::new(&[fomc_duration_event()]);

        // Tick 1: before window — inactive.
        let before = Utc.with_ymd_and_hms(2026, 4, 16, 13, 30, 0).unwrap();
        let clock = sim_at(before);
        mgr.check_active_events(&clock);
        assert!(!mgr.windows[0].is_active);

        // Tick 2: still before — inactive, no transition.
        mgr.check_active_events(&clock);
        assert!(!mgr.windows[0].is_active);

        // Tick 3: enter window — transition false -> true.
        let inside = Utc.with_ymd_and_hms(2026, 4, 16, 14, 30, 0).unwrap();
        let nanos = inside.timestamp_nanos_opt().unwrap() as u64;
        clock.set_time(UnixNanos::new(nanos));
        mgr.check_active_events(&clock);
        assert!(mgr.windows[0].is_active);

        // Tick 4: still inside — no transition.
        mgr.check_active_events(&clock);
        assert!(mgr.windows[0].is_active);

        // Tick 5: after window — transition true -> false.
        let after = Utc.with_ymd_and_hms(2026, 4, 16, 16, 30, 0).unwrap();
        let nanos = after.timestamp_nanos_opt().unwrap() as u64;
        clock.set_time(UnixNanos::new(nanos));
        mgr.check_active_events(&clock);
        assert!(!mgr.windows[0].is_active);
    }

    /// Right-open interval: at exact `end` instant the window must be inactive.
    #[test]
    fn end_instant_is_exclusive() {
        let mut mgr = EventWindowManager::new(&[fomc_duration_event()]);
        let exact_end = Utc.with_ymd_and_hms(2026, 4, 16, 16, 0, 0).unwrap();
        let clock = sim_at(exact_end);
        let active = mgr.check_active_events(&clock);
        assert!(
            active.is_empty(),
            "exact end instant must be treated as past the window"
        );
    }

    /// Right-open interval: at exact `start` instant the window IS active.
    #[test]
    fn start_instant_is_inclusive() {
        let mut mgr = EventWindowManager::new(&[fomc_duration_event()]);
        let exact_start = Utc.with_ymd_and_hms(2026, 4, 16, 14, 0, 0).unwrap();
        let clock = sim_at(exact_start);
        let active = mgr.check_active_events(&clock);
        assert_eq!(active.len(), 1);
    }

    /// `remaining` decreases as time advances inside a window.
    #[test]
    fn remaining_duration_tracks_time() {
        let mut mgr = EventWindowManager::new(&[fomc_duration_event()]);
        let early = Utc.with_ymd_and_hms(2026, 4, 16, 14, 5, 0).unwrap();
        let clock = sim_at(early);
        let active = mgr.check_active_events(&clock);
        assert_eq!(active[0].remaining, Duration::minutes(115));

        // Advance 30 minutes.
        let later = Utc.with_ymd_and_hms(2026, 4, 16, 14, 35, 0).unwrap();
        let nanos = later.timestamp_nanos_opt().unwrap() as u64;
        clock.set_time(UnixNanos::new(nanos));
        let active = mgr.check_active_events(&clock);
        assert_eq!(active[0].remaining, Duration::minutes(85));
    }

    /// 7.11 — config validation rejects events without end or duration.
    /// (Validation lives in `core::config::validation`; the manager itself
    /// silently drops malformed entries as a defence-in-depth fallback.)
    #[test]
    fn manager_drops_malformed_entries() {
        let bad = EventWindowConfig {
            name: "Bad".into(),
            start: naive(2026, 4, 16, 14, 0),
            end: None,
            duration_minutes: None,
            action: EventAction::SitOut,
        };
        let mgr = EventWindowManager::new(&[bad]);
        assert!(mgr.is_empty());
    }

    /// Empty manager returns no restriction.
    #[test]
    fn empty_manager_returns_none() {
        let mut mgr = EventWindowManager::new(&[]);
        let clock = SimClock::new(0);
        assert!(mgr.check_active_events(&clock).is_empty());
        assert_eq!(mgr.get_trading_restriction(&clock), None);
    }

    /// TradingRestriction::from_action is total and identity-respecting.
    #[test]
    fn from_action_total() {
        assert_eq!(
            TradingRestriction::from_action(EventAction::DisableStrategies),
            TradingRestriction::DisableStrategies
        );
        assert_eq!(
            TradingRestriction::from_action(EventAction::ReduceExposure),
            TradingRestriction::ReduceExposure
        );
        assert_eq!(
            TradingRestriction::from_action(EventAction::SitOut),
            TradingRestriction::SitOut
        );
    }

    /// Severity ordering: SitOut > ReduceExposure > DisableStrategies.
    #[test]
    fn severity_ordering() {
        assert!(
            TradingRestriction::SitOut.severity() > TradingRestriction::ReduceExposure.severity()
        );
        assert!(
            TradingRestriction::ReduceExposure.severity()
                > TradingRestriction::DisableStrategies.severity()
        );
    }
}
