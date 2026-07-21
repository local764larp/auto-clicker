use core::sync::atomic::{AtomicU64, Ordering};

/// Monotonic nanosecond time source.
pub trait Clock {
    fn now_ns(&self) -> u64;
}

impl<T: Clock + ?Sized> Clock for std::sync::Arc<T> {
    fn now_ns(&self) -> u64 {
        (**self).now_ns()
    }
}

/// Test clock. Time advances only when a test or an `InstantWaiter` says so,
/// which makes every engine test deterministic and instant.
#[derive(Debug)]
pub struct VirtualClock {
    now: AtomicU64,
}

impl VirtualClock {
    pub fn new(start_ns: u64) -> Self {
        Self { now: AtomicU64::new(start_ns) }
    }
    pub fn advance(&self, ns: u64) {
        self.now.fetch_add(ns, Ordering::Relaxed);
    }
    /// Jump forward to `ns`, never backwards.
    pub fn set_at_least(&self, ns: u64) {
        self.now.fetch_max(ns, Ordering::Relaxed);
    }
}

impl Clock for VirtualClock {
    fn now_ns(&self) -> u64 {
        self.now.load(Ordering::Relaxed)
    }
}

#[cfg(windows)]
mod qpc {
    use super::Clock;
    use windows::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };

    /// `QueryPerformanceCounter` with the frequency cached at construction.
    /// The frequency is fixed for the boot session; querying it in the loop is
    /// a wasted call.
    #[derive(Debug, Clone, Copy)]
    pub struct QpcClock {
        freq: u64,
    }

    impl QpcClock {
        pub fn new() -> Self {
            let mut f = 0i64;
            // SAFETY: `f` is a valid, aligned, writable i64 for the duration of
            // the call. QueryPerformanceFrequency is documented never to fail on
            // Windows XP or later, so the frequency is always non-zero; we guard
            // against 0 anyway to make the later division total.
            unsafe {
                let _ = QueryPerformanceFrequency(&mut f);
            }
            Self { freq: if f > 0 { f as u64 } else { 1 } }
        }
    }

    impl Default for QpcClock {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Clock for QpcClock {
        fn now_ns(&self) -> u64 {
            let mut t = 0i64;
            // SAFETY: `t` is a valid, aligned, writable i64 for the duration of
            // the call. QueryPerformanceCounter cannot fail on supported Windows.
            unsafe {
                let _ = QueryPerformanceCounter(&mut t);
            }
            // u128 intermediate: `t * 1e9` overflows u64 after ~2 seconds at a
            // 10MHz QPC frequency, so the widening is required, not defensive.
            ((t as u128 * 1_000_000_000u128) / self.freq as u128) as u64
        }
    }
}

#[cfg(windows)]
pub use qpc::QpcClock;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn virtual_clock_advances_only_when_told() {
        let c = VirtualClock::new(1_000);
        assert_eq!(c.now_ns(), 1_000);
        c.advance(500);
        assert_eq!(c.now_ns(), 1_500);
    }

    #[test]
    fn set_at_least_never_moves_time_backwards() {
        let c = VirtualClock::new(5_000);
        c.set_at_least(1_000);
        assert_eq!(c.now_ns(), 5_000);
        c.set_at_least(9_000);
        assert_eq!(c.now_ns(), 9_000);
    }

    #[test]
    fn arc_forwards_to_inner_clock() {
        let c = Arc::new(VirtualClock::new(42));
        assert_eq!(c.now_ns(), 42);
        fn takes_clock<C: Clock>(c: &C) -> u64 {
            c.now_ns()
        }
        assert_eq!(takes_clock(&c), 42);
    }
}
