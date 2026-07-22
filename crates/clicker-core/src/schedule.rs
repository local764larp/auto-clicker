use crate::shared::Config;

/// How many periods late counts as a stall rather than jitter.
pub const SNAP_PERIODS: u64 = 4;

/// Lower bound on the snap-forward threshold. Load-bearing: at 10_000 CPS a
/// bare `interval * 4` is 400us, so ordinary scheduler noise would trip
/// snap-forward on nearly every iteration and it would stop meaning
/// "recovered from a stall".
pub const SNAP_FLOOR_NS: u64 = 2_000_000; // 2ms

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    ClickLimit,
    TimeLimit,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Emit now. `next_deadline_ns` becomes the deadline for the next iteration.
    Fire { next_deadline_ns: u64 },
    /// Not yet. Sleep/spin until `until_ns`.
    Wait { until_ns: u64 },
    /// A configured limit tripped.
    Stop(StopReason),
}

/// Everything the decision depends on that is not configuration.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RunState {
    pub now_ns: u64,
    pub deadline_ns: u64,
    /// Clicks emitted since the current idle->running edge, not since process start.
    pub clicks_this_run: u64,
    /// Nanoseconds since the current idle->running edge.
    pub elapsed_ns: u64,
}

/// Pure. No clock, no I/O, no allocation. This is the whole timing brain.
pub fn decide(run: RunState, cfg: &Config) -> Decision {
    // Limits are evaluated BEFORE firing, so `limit_clicks = N` emits exactly N.
    if cfg.limit_clicks != 0 && run.clicks_this_run >= cfg.limit_clicks {
        return Decision::Stop(StopReason::ClickLimit);
    }
    if cfg.limit_ns != 0 && run.elapsed_ns >= cfg.limit_ns {
        return Decision::Stop(StopReason::TimeLimit);
    }

    // Unthrottled: the ceiling case never enters deadline arithmetic, so the
    // snap-forward branch below keeps meaning only "recovered from a stall".
    if cfg.interval_ns == 0 {
        return Decision::Fire { next_deadline_ns: run.now_ns };
    }

    if run.now_ns < run.deadline_ns {
        return Decision::Wait { until_ns: run.deadline_ns };
    }

    let lateness = run.now_ns - run.deadline_ns;
    let threshold = core::cmp::max(cfg.interval_ns.saturating_mul(SNAP_PERIODS), SNAP_FLOOR_NS);

    let next_deadline_ns = if lateness > threshold {
        // Stalled (preemption, suspend). Discard the backlog rather than
        // firing a burst of catch-up clicks — a burst is never what was wanted.
        run.now_ns.saturating_add(cfg.interval_ns)
    } else {
        // Absolute advancement. Never `now + interval`: that accumulates drift
        // proportional to per-iteration overhead.
        run.deadline_ns.saturating_add(cfg.interval_ns)
    };

    Decision::Fire { next_deadline_ns }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::{Button, Config};

    fn cfg(interval_ns: u64) -> Config {
        Config {
            interval_ns,
            button: Button::Left,
            position: None,
            limit_clicks: 0,
            limit_ns: 0,
            batch_size: 1,
            duty_pct: 0,
            randomize_pct: 0,
            click_kind: 0,
            key_vk: 0x20,
            sequence: false,
            stop_when_complete: false,
        }
    }

    fn run(now_ns: u64, deadline_ns: u64) -> RunState {
        RunState { now_ns, deadline_ns, clicks_this_run: 0, elapsed_ns: 0 }
    }

    #[test]
    fn waits_when_deadline_is_in_the_future() {
        let d = decide(run(1_000, 5_000), &cfg(10_000));
        assert_eq!(d, Decision::Wait { until_ns: 5_000 });
    }

    #[test]
    fn advances_by_exact_interval_not_from_now() {
        // Deadline was 10_000; we woke 300ns late. Next deadline must be
        // 20_000 (deadline + interval), NOT 20_300 (now + interval).
        let d = decide(run(10_300, 10_000), &cfg(10_000));
        assert_eq!(d, Decision::Fire { next_deadline_ns: 20_000 });
    }

    #[test]
    fn repeated_late_wakes_accumulate_no_drift() {
        // 1000 iterations, each waking 300ns late, must land exactly on
        // start + 1000 * interval.
        let interval = 1_000_000u64;
        let mut deadline = 0u64;
        for i in 0..1000u64 {
            let now = deadline + 300;
            match decide(run(now, deadline), &cfg(interval)) {
                Decision::Fire { next_deadline_ns } => deadline = next_deadline_ns,
                other => panic!("iteration {i}: expected Fire, got {other:?}"),
            }
        }
        assert_eq!(deadline, 1000 * interval);
    }

    #[test]
    fn small_jitter_does_not_trigger_snap_forward() {
        // interval 100us, threshold = max(400us, 2ms) = 2ms.
        // 1ms late is ordinary scheduler noise: keep absolute advancement.
        let interval = 100_000u64;
        let d = decide(run(10_000_000 + 1_000_000, 10_000_000), &cfg(interval));
        assert_eq!(d, Decision::Fire { next_deadline_ns: 10_100_000 });
    }

    #[test]
    fn long_stall_snaps_forward_instead_of_bursting() {
        // interval 1ms, threshold = max(4ms, 2ms) = 4ms. 500ms late is a stall.
        let interval = 1_000_000u64;
        let now = 500_000_000u64;
        let d = decide(run(now, 0), &cfg(interval));
        assert_eq!(d, Decision::Fire { next_deadline_ns: now + interval });
    }

    #[test]
    fn snap_floor_protects_high_rates() {
        // interval 10us => interval*4 = 40us, but the 2ms floor governs.
        // 1ms late must NOT snap.
        let interval = 10_000u64;
        let d = decide(run(1_000_000, 0), &cfg(interval));
        assert_eq!(d, Decision::Fire { next_deadline_ns: interval });
    }

    #[test]
    fn unthrottled_always_fires_immediately() {
        let d = decide(run(12_345, 999_999_999), &cfg(0));
        assert_eq!(d, Decision::Fire { next_deadline_ns: 12_345 });
    }

    #[test]
    fn click_limit_trips_at_exact_boundary() {
        let mut c = cfg(1_000);
        c.limit_clicks = 10;

        let mut r = run(0, 0);
        r.clicks_this_run = 9;
        assert!(matches!(decide(r, &c), Decision::Fire { .. }), "9 of 10 must still fire");

        r.clicks_this_run = 10;
        assert_eq!(decide(r, &c), Decision::Stop(StopReason::ClickLimit));

        r.clicks_this_run = 11;
        assert_eq!(decide(r, &c), Decision::Stop(StopReason::ClickLimit));
    }

    #[test]
    fn zero_click_limit_means_unlimited() {
        let c = cfg(1_000);
        let mut r = run(0, 0);
        r.clicks_this_run = u64::MAX - 1;
        assert!(matches!(decide(r, &c), Decision::Fire { .. }));
    }

    #[test]
    fn time_limit_trips_at_exact_boundary() {
        let mut c = cfg(1_000);
        c.limit_ns = 5_000_000;

        let mut r = run(0, 0);
        r.elapsed_ns = 4_999_999;
        assert!(matches!(decide(r, &c), Decision::Fire { .. }));

        r.elapsed_ns = 5_000_000;
        assert_eq!(decide(r, &c), Decision::Stop(StopReason::TimeLimit));
    }

    #[test]
    fn limits_are_checked_before_unthrottled_fire() {
        // Unthrottled mode must not bypass limit enforcement.
        let mut c = cfg(0);
        c.limit_clicks = 5;
        let mut r = run(0, 0);
        r.clicks_this_run = 5;
        assert_eq!(decide(r, &c), Decision::Stop(StopReason::ClickLimit));
    }

    #[test]
    fn click_limit_takes_precedence_over_time_limit() {
        let mut c = cfg(1_000);
        c.limit_clicks = 5;
        c.limit_ns = 1_000;
        let mut r = run(0, 0);
        r.clicks_this_run = 5;
        r.elapsed_ns = 9_999;
        assert_eq!(decide(r, &c), Decision::Stop(StopReason::ClickLimit));
    }

    #[test]
    fn interval_change_midrun_takes_effect_on_next_advance() {
        // Deadline 10_000 reached; interval has been changed to 50_000.
        let d = decide(run(10_000, 10_000), &cfg(50_000));
        assert_eq!(d, Decision::Fire { next_deadline_ns: 60_000 });
    }

    #[test]
    fn saturates_rather_than_overflowing_near_u64_max() {
        let d = decide(run(u64::MAX, u64::MAX), &cfg(u64::MAX));
        assert_eq!(d, Decision::Fire { next_deadline_ns: u64::MAX });
    }
}
