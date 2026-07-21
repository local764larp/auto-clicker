use crate::clock::Clock;

/// Idle poll period. Idle is not the hot path; this bounds start latency to
/// 1ms without spinning a core.
pub const IDLE_POLL_NS: u64 = 1_000_000;

/// Above this remaining time, use the high-resolution waitable timer.
pub const TIER_TIMER_NS: u64 = 2_000_000; // 2ms
/// Above this remaining time, yield. Below it, spin.
pub const TIER_YIELD_NS: u64 = 50_000; // 50us
/// Margin left for the timer to wake early rather than late.
pub const TIMER_MARGIN_NS: u64 = 1_500_000;

pub trait Waiter {
    /// Block until `clock.now_ns() >= target_ns`.
    fn wait_until<C: Clock>(&mut self, target_ns: u64, clock: &C);
    /// Called when the engine is not running. Must not spin.
    fn idle(&mut self);
}

/// Test waiter: "waiting" is jumping the virtual clock forward. No sleeping,
/// so engine tests are deterministic and finish instantly.
#[derive(Debug)]
pub struct InstantWaiter {
    clock: std::sync::Arc<crate::clock::VirtualClock>,
}

impl InstantWaiter {
    pub fn new(clock: std::sync::Arc<crate::clock::VirtualClock>) -> Self {
        Self { clock }
    }
}

impl Waiter for InstantWaiter {
    fn wait_until<C: Clock>(&mut self, target_ns: u64, _clock: &C) {
        self.clock.set_at_least(target_ns);
    }
    fn idle(&mut self) {
        self.clock.advance(IDLE_POLL_NS);
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
    use windows::Win32::System::Threading::{
        CreateWaitableTimerExW, SetWaitableTimerEx, SwitchToThread, WaitForSingleObject,
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS,
    };

    /// RAII guard for `timeBeginPeriod`. Leaking a raised global timer
    /// resolution measurably degrades system battery life, so the pairing must
    /// survive every exit path including unwind.
    #[derive(Debug)]
    pub struct TimerResolutionGuard {
        period: u32,
    }

    impl TimerResolutionGuard {
        pub fn new(period: u32) -> Self {
            // SAFETY: timeBeginPeriod takes a plain u32 and has no pointer
            // arguments. The matching timeEndPeriod is issued in Drop below.
            unsafe {
                timeBeginPeriod(period);
            }
            Self { period }
        }
    }

    impl Drop for TimerResolutionGuard {
        fn drop(&mut self) {
            // SAFETY: `period` is exactly the value passed to timeBeginPeriod
            // in `new`, and this runs at most once because Drop runs once.
            unsafe {
                timeEndPeriod(self.period);
            }
        }
    }

    /// RAII guard for a waitable timer HANDLE.
    #[derive(Debug)]
    struct TimerHandle(HANDLE);

    // SAFETY: a waitable timer HANDLE is a kernel object reference that is
    // valid process-wide and safe to use from any thread. The engine creates
    // the handle on one thread and uses it only from the thread that owns the
    // `HybridWaiter`.
    unsafe impl Send for TimerHandle {}

    impl Drop for TimerHandle {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from CreateWaitableTimerExW and is closed
            // exactly once here. It is not used after this point.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Three-tier hybrid wait:
    ///   > 2ms      high-resolution waitable timer (~0.5ms granularity)
    ///   50us..2ms  SwitchToThread yield loop
    ///   < 50us     spin on the clock with PAUSE
    #[derive(Debug)]
    pub struct HybridWaiter {
        timer: Option<TimerHandle>,
        _resolution: Option<TimerResolutionGuard>,
    }

    impl HybridWaiter {
        pub fn new() -> Self {
            // SAFETY: all pointer arguments are None/null, which the API accepts
            // for an unnamed timer with no security attributes. The returned
            // handle is owned by TimerHandle, which closes it exactly once.
            let timer = unsafe {
                CreateWaitableTimerExW(
                    None,
                    None,
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS.0,
                )
            }
            .ok()
            .map(TimerHandle);

            // Only raise the global timer resolution if the high-resolution
            // timer is unavailable (pre-1803). Otherwise we pay nothing.
            let resolution = if timer.is_none() {
                Some(TimerResolutionGuard::new(1))
            } else {
                None
            };

            Self { timer, _resolution: resolution }
        }

        fn sleep_via_timer(&self, ns: u64) -> bool {
            let Some(t) = &self.timer else { return false };
            // Negative 100ns units = relative due time.
            let due = -((ns / 100) as i64);
            // SAFETY: `t.0` is a live timer handle owned by self. `&due` is a
            // valid aligned i64 read only for the duration of the call. No
            // completion routine is supplied, so the None arguments are correct.
            let set = unsafe { SetWaitableTimerEx(t.0, &due, 0, None, None, None, 0) };
            if set.is_err() {
                return false;
            }
            // SAFETY: `t.0` is a live waitable timer handle owned by self.
            let r = unsafe { WaitForSingleObject(t.0, 50) };
            r == WAIT_OBJECT_0
        }
    }

    impl Default for HybridWaiter {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Waiter for HybridWaiter {
        fn wait_until<C: Clock>(&mut self, target_ns: u64, clock: &C) {
            loop {
                let now = clock.now_ns();
                if now >= target_ns {
                    return;
                }
                let remaining = target_ns - now;

                if remaining > TIER_TIMER_NS {
                    // Wake early on purpose and let the finer tiers close the gap.
                    if !self.sleep_via_timer(remaining - TIMER_MARGIN_NS) {
                        // SAFETY: SwitchToThread takes no arguments and cannot fail
                        // in a way that affects memory safety.
                        unsafe {
                            let _ = SwitchToThread();
                        }
                    }
                } else if remaining > TIER_YIELD_NS {
                    // SAFETY: as above — no pointer arguments.
                    unsafe {
                        let _ = SwitchToThread();
                    }
                } else {
                    // PAUSE. Meaningfully better for SMT siblings and power
                    // than a bare loop.
                    core::hint::spin_loop();
                }
            }
        }

        fn idle(&mut self) {
            if !self.sleep_via_timer(IDLE_POLL_NS) {
                // SAFETY: no pointer arguments.
                unsafe {
                    let _ = SwitchToThread();
                }
            }
        }
    }
}

#[cfg(windows)]
pub use win::{HybridWaiter, TimerResolutionGuard};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Clock, VirtualClock};
    use std::sync::Arc;

    #[test]
    fn instant_waiter_jumps_the_clock_to_the_target() {
        let clock = Arc::new(VirtualClock::new(0));
        let mut w = InstantWaiter::new(clock.clone());
        w.wait_until(5_000_000, &clock);
        assert_eq!(clock.now_ns(), 5_000_000);
    }

    #[test]
    fn instant_waiter_never_rewinds() {
        let clock = Arc::new(VirtualClock::new(9_000));
        let mut w = InstantWaiter::new(clock.clone());
        w.wait_until(1_000, &clock);
        assert_eq!(clock.now_ns(), 9_000);
    }

    #[test]
    fn idle_advances_by_the_documented_poll_period() {
        let clock = Arc::new(VirtualClock::new(0));
        let mut w = InstantWaiter::new(clock.clone());
        w.idle();
        assert_eq!(clock.now_ns(), IDLE_POLL_NS);
    }
}
