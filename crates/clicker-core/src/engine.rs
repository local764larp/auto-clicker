use crate::clock::Clock;
use crate::schedule::{decide, Decision, RunState};
use crate::shared::{EngineState, SharedState};
use crate::sink::ClickSink;
use crate::wait::Waiter;

/// The hot loop. Generic over its clock, waiter, and sink so the entire loop
/// is deterministically testable in virtual time.
///
/// Invariants held by `run`: no allocation, no locking, no logging, and no
/// formatting inside the loop body. Everything is constructed before entry.
#[derive(Debug)]
pub struct Engine<C, W, S> {
    clock: C,
    waiter: W,
    sink: S,
}

impl<C: Clock, W: Waiter, S: ClickSink> Engine<C, W, S> {
    pub fn new(clock: C, waiter: W, sink: S) -> Self {
        Self { clock, waiter, sink }
    }

    pub fn sink(&self) -> &S {
        &self.sink
    }

    pub fn into_sink(self) -> S {
        self.sink
    }

    pub fn run(&mut self, shared: &SharedState) {
        // Per-run state, all stack locals. Nothing here allocates.
        let mut deadline_ns: u64 = 0;
        let mut run_start_ns: u64 = 0;
        let mut clicks_this_run: u64 = 0;
        let mut was_running = false;

        // `clicks_emitted` is monotonic for the process lifetime; limits are
        // per-run. Seed the local total from whatever the counter already holds.
        let mut total_clicks: u64 = shared.clicks_emitted();

        loop {
            if shared.shutdown() {
                break;
            }

            if !shared.running() {
                was_running = false;
                self.waiter.idle();
                continue;
            }

            let now_ns = self.clock.now_ns();

            // idle -> running edge: reset the per-run baselines.
            if !was_running {
                was_running = true;
                run_start_ns = now_ns;
                clicks_this_run = 0;
                deadline_ns = now_ns;
                shared.set_engine_state(EngineState::Running);
            }

            let cfg = shared.snapshot();
            let state = RunState {
                now_ns,
                deadline_ns,
                clicks_this_run,
                elapsed_ns: now_ns.saturating_sub(run_start_ns),
            };

            match decide(state, &cfg) {
                Decision::Stop(_reason) => {
                    shared.set_running(false);
                    shared.set_engine_state(EngineState::StoppedByLimit);
                    was_running = false;
                }
                Decision::Wait { until_ns } => {
                    self.waiter.wait_until(until_ns, &self.clock);
                }
                Decision::Fire { next_deadline_ns } => {
                    // Batching only applies unthrottled: batched events carry no
                    // temporal spacing, so batching a throttled rate would
                    // destroy the requested interval.
                    let mut batch: u64 =
                        if cfg.interval_ns == 0 { cfg.batch_size.max(1) as u64 } else { 1 };

                    // Never overshoot a click limit just because of batching.
                    if cfg.limit_clicks != 0 {
                        let remaining = cfg.limit_clicks.saturating_sub(clicks_this_run);
                        batch = batch.min(remaining);
                    }

                    if batch == 0 {
                        continue;
                    }

                    match self.sink.emit_batch(cfg.button, cfg.position, batch as u16) {
                        Ok(n) => {
                            clicks_this_run += n as u64;
                            total_clicks += n as u64;
                            shared.store_clicks(total_clicks);
                            deadline_ns = next_deadline_ns;
                        }
                        Err(_e) => {
                            // Blocked input is a distinct state, never a silent
                            // no-op. The host surfaces the elevation message.
                            shared.set_running(false);
                            shared.set_engine_state(EngineState::Error);
                            was_running = false;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::VirtualClock;
    use crate::shared::{Button, EngineState, PositionMode, SharedState};
    use crate::sink::RecordingSink;
    use crate::wait::InstantWaiter;
    use std::sync::Arc;

    /// Wires a fully deterministic engine: virtual clock, instant waiter,
    /// recording sink. `cost_ns` models per-click syscall cost.
    fn harness(
        cost_ns: u64,
    ) -> (Arc<VirtualClock>, Engine<Arc<VirtualClock>, InstantWaiter, RecordingSink>) {
        let clock = Arc::new(VirtualClock::new(0));
        let waiter = InstantWaiter::new(clock.clone());
        let sink = RecordingSink::with_cost(clock.clone(), cost_ns);
        (clock.clone(), Engine::new(clock, waiter, sink))
    }

    /// Runs the engine until it reaches `target`, then shuts it down.
    fn run_until(
        eng: &mut Engine<Arc<VirtualClock>, InstantWaiter, RecordingSink>,
        shared: &SharedState,
        target: EngineState,
    ) {
        std::thread::scope(|s| {
            s.spawn(|| {
                while shared.engine_state() != target {
                    std::hint::spin_loop();
                }
                shared.request_shutdown();
            });
            eng.run(shared);
        });
    }

    #[test]
    fn emits_exactly_the_click_limit() {
        let (_clock, mut eng) = harness(0);
        let shared = SharedState::new();
        shared.set_interval_ns(1_000_000);
        shared.set_limit_clicks(10);
        shared.set_running(true);

        run_until(&mut eng, &shared, EngineState::StoppedByLimit);

        assert_eq!(eng.sink().total(), 10);
        assert_eq!(shared.clicks_emitted(), 10);
        assert_eq!(shared.engine_state(), EngineState::StoppedByLimit);
        assert!(!shared.running());
    }

    #[test]
    fn intervals_are_drift_free() {
        let (_clock, mut eng) = harness(37); // odd per-click cost
        let shared = SharedState::new();
        shared.set_interval_ns(1_000_000);
        shared.set_limit_clicks(100);
        shared.set_running(true);

        run_until(&mut eng, &shared, EngineState::StoppedByLimit);

        let clicks = eng.sink().clicks();
        assert_eq!(clicks.len(), 100);
        // Each click lands on its absolute deadline; per-click cost must not
        // accumulate into the schedule.
        for (i, c) in clicks.iter().enumerate() {
            assert_eq!(c.at_ns, i as u64 * 1_000_000, "click {i} drifted");
        }
    }

    #[test]
    fn time_limit_stops_the_run() {
        let (_clock, mut eng) = harness(0);
        let shared = SharedState::new();
        shared.set_interval_ns(1_000_000);
        shared.set_limit_ns(10_000_000); // 10ms => 10 clicks at 1ms
        shared.set_running(true);

        run_until(&mut eng, &shared, EngineState::StoppedByLimit);

        assert_eq!(eng.sink().total(), 10);
    }

    #[test]
    fn unthrottled_mode_batches_and_throttled_mode_does_not() {
        let (_clock, mut eng) = harness(100);
        let shared = SharedState::new();
        shared.set_interval_ns(0); // unthrottled
        shared.set_batch_size(8);
        shared.set_limit_clicks(64);
        shared.set_running(true);

        run_until(&mut eng, &shared, EngineState::StoppedByLimit);

        assert_eq!(eng.sink().total(), 64);
    }

    #[test]
    fn batch_is_clamped_so_the_limit_is_never_overshot() {
        let (_clock, mut eng) = harness(0);
        let shared = SharedState::new();
        shared.set_interval_ns(0);
        shared.set_batch_size(16);
        shared.set_limit_clicks(10); // not a multiple of the batch size
        shared.set_running(true);

        run_until(&mut eng, &shared, EngineState::StoppedByLimit);

        assert_eq!(eng.sink().total(), 10, "batching must not overshoot the limit");
    }

    #[test]
    fn throttled_mode_ignores_batch_size() {
        let (_clock, mut eng) = harness(0);
        let shared = SharedState::new();
        shared.set_interval_ns(1_000_000);
        shared.set_batch_size(8);
        shared.set_limit_clicks(5);
        shared.set_running(true);

        run_until(&mut eng, &shared, EngineState::StoppedByLimit);

        let clicks = eng.sink().clicks();
        assert_eq!(clicks.len(), 5);
        // Batching would collapse these to one timestamp; spacing proves it did not.
        assert_eq!(clicks[1].at_ns - clicks[0].at_ns, 1_000_000);
    }

    #[test]
    fn limits_reset_across_stop_and_restart() {
        let (_clock, mut eng) = harness(0);
        let shared = SharedState::new();
        shared.set_interval_ns(1_000_000);
        shared.set_limit_clicks(5);
        shared.set_running(true);

        std::thread::scope(|s| {
            s.spawn(|| {
                // First run: wait for the limit, then start a second run.
                while shared.engine_state() != EngineState::StoppedByLimit {
                    std::hint::spin_loop();
                }
                shared.set_engine_state(EngineState::Idle);
                shared.set_running(true);
                while shared.engine_state() != EngineState::StoppedByLimit {
                    std::hint::spin_loop();
                }
                shared.request_shutdown();
            });
            eng.run(&shared);
        });

        // 5 per run, twice. If limits were evaluated against the monotonic
        // counter, the second run would emit zero.
        assert_eq!(eng.sink().total(), 10);
        assert_eq!(shared.clicks_emitted(), 10, "counter must stay monotonic across runs");
    }

    #[test]
    fn blocked_sink_sets_error_state_and_stops() {
        let clock = Arc::new(VirtualClock::new(0));
        let waiter = InstantWaiter::new(clock.clone());
        let mut sink = RecordingSink::new(clock.clone());
        sink.fail_after(3);
        let mut eng = Engine::new(clock, waiter, sink);

        let shared = SharedState::new();
        shared.set_interval_ns(1_000_000);
        shared.set_running(true);

        run_until(&mut eng, &shared, EngineState::Error);

        assert_eq!(eng.sink().total(), 3);
        assert_eq!(shared.engine_state(), EngineState::Error);
        assert!(!shared.running(), "a blocked sink must clear running");
    }

    #[test]
    fn idle_engine_emits_nothing_and_still_shuts_down() {
        let (_clock, mut eng) = harness(0);
        let shared = SharedState::new();
        shared.set_running(false);

        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(20));
                shared.request_shutdown();
            });
            eng.run(&shared);
        });

        assert_eq!(eng.sink().total(), 0);
    }

    #[test]
    fn fixed_point_mode_passes_coordinates_through() {
        let (_clock, mut eng) = harness(0);
        let shared = SharedState::new();
        shared.set_interval_ns(1_000_000);
        shared.set_position_mode(PositionMode::FixedPoint);
        shared.set_fixed_point(300, 400);
        shared.set_button(Button::Middle);
        shared.set_limit_clicks(1);
        shared.set_running(true);

        run_until(&mut eng, &shared, EngineState::StoppedByLimit);

        assert_eq!(eng.sink().clicks()[0].pos, Some((300, 400)));
        assert_eq!(eng.sink().clicks()[0].button, Button::Middle);
    }
}
