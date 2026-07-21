//! Emit-side timing probe.
//!
//! The bench's receiver can only observe when the *message loop* consumed a
//! click, which is bounded by that loop's own cadence. To characterise the
//! engine's own emission timing, the timestamp has to be taken on the emit
//! side, inside the hot loop.
//!
//! That means the probe must obey the hot-loop rules: it allocates nothing,
//! locks nothing, and logs nothing. It is a fixed-capacity ring of atomics
//! written with `Relaxed` stores.

use crate::clock::Clock;
use crate::shared::Button;
use crate::sink::{ClickSink, SinkError};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Fixed-capacity ring buffer of emission timestamps.
///
/// The engine writes; any other thread may read via [`EmitProbe::snapshot`].
/// Writes are `Relaxed` stores into a preallocated `Vec<AtomicU64>` — no
/// allocation and no locking occur after construction.
#[derive(Debug)]
pub struct EmitProbe {
    buf: Vec<AtomicU64>,
    /// Total records written, monotonic. Wraps into `buf` modulo capacity.
    written: AtomicUsize,
}

impl EmitProbe {
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        let mut buf = Vec::with_capacity(cap);
        buf.resize_with(cap, || AtomicU64::new(0));
        Self { buf, written: AtomicUsize::new(0) }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Record one emission timestamp. Hot-path: one relaxed RMW on the index
    /// and one relaxed store. No allocation, no locking, no branching on I/O.
    #[inline]
    pub fn record(&self, t_ns: u64) {
        let i = self.written.fetch_add(1, Ordering::Relaxed);
        self.buf[i % self.buf.len()].store(t_ns, Ordering::Relaxed);
    }

    pub fn written(&self) -> usize {
        self.written.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.written.store(0, Ordering::Relaxed);
    }

    /// Timestamps in write order, oldest first. If more records were written
    /// than the ring holds, only the most recent `capacity()` survive.
    pub fn snapshot(&self) -> Vec<u64> {
        let n = self.written();
        let cap = self.buf.len();
        if n <= cap {
            (0..n).map(|i| self.buf[i].load(Ordering::Relaxed)).collect()
        } else {
            let start = n % cap;
            (0..cap).map(|k| self.buf[(start + k) % cap].load(Ordering::Relaxed)).collect()
        }
    }

    /// Consecutive differences of [`EmitProbe::snapshot`].
    pub fn intervals(&self) -> Vec<u64> {
        let t = self.snapshot();
        t.windows(2).map(|w| w[1].saturating_sub(w[0])).collect()
    }
}

/// Wraps a [`ClickSink`], timestamping each emitted click into an [`EmitProbe`]
/// before delegating. Used by the bench to measure engine-side timing without
/// the receiver's message loop in the path.
#[derive(Debug)]
pub struct ProbedSink<S, C> {
    inner: S,
    clock: C,
    probe: std::sync::Arc<EmitProbe>,
}

impl<S, C> ProbedSink<S, C> {
    pub fn new(inner: S, clock: C, probe: std::sync::Arc<EmitProbe>) -> Self {
        Self { inner, clock, probe }
    }
}

impl<S: ClickSink, C: Clock> ClickSink for ProbedSink<S, C> {
    fn emit_batch(
        &mut self,
        button: Button,
        pos: Option<(i32, i32)>,
        count: u16,
    ) -> Result<u16, SinkError> {
        // Timestamp before delegating: this measures when the engine decided to
        // fire, which is the quantity the schedule is responsible for.
        let t = self.clock.now_ns();
        for _ in 0..count {
            self.probe.record(t);
        }
        self.inner.emit_batch(button, pos, count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::VirtualClock;
    use crate::sink::RecordingSink;
    use std::sync::Arc;

    #[test]
    fn records_in_order_below_capacity() {
        let p = EmitProbe::new(8);
        for t in [10u64, 20, 30] {
            p.record(t);
        }
        assert_eq!(p.snapshot(), vec![10, 20, 30]);
        assert_eq!(p.intervals(), vec![10, 10]);
    }

    #[test]
    fn keeps_the_most_recent_records_when_it_wraps() {
        let p = EmitProbe::new(4);
        for t in [1u64, 2, 3, 4, 5, 6] {
            p.record(t);
        }
        // Oldest two are overwritten; the surviving window stays ordered.
        assert_eq!(p.snapshot(), vec![3, 4, 5, 6]);
    }

    #[test]
    fn reset_clears_the_record_count() {
        let p = EmitProbe::new(4);
        p.record(1);
        p.reset();
        assert_eq!(p.written(), 0);
        assert!(p.snapshot().is_empty());
    }

    #[test]
    fn zero_capacity_is_clamped_rather_than_panicking() {
        let p = EmitProbe::new(0);
        p.record(5);
        assert_eq!(p.capacity(), 1);
    }

    #[test]
    fn probed_sink_timestamps_and_delegates() {
        let clock = Arc::new(VirtualClock::new(0));
        let probe = Arc::new(EmitProbe::new(16));
        let inner = RecordingSink::with_cost(clock.clone(), 100);
        let mut s = ProbedSink::new(inner, clock.clone(), probe.clone());

        s.emit(Button::Left, None).unwrap();
        s.emit(Button::Left, None).unwrap();

        // Delegation happened.
        assert_eq!(s.inner.total(), 2);
        // And the emit-side timestamps advanced by the modelled cost.
        assert_eq!(probe.snapshot(), vec![0, 100]);
    }
}
