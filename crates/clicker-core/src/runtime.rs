use crate::affinity::{pin_current_thread_to_best, ThreadPriorityGuard};
use crate::clock::QpcClock;
use crate::engine::Engine;
use crate::hotkey::{HotkeyError, PanicHotkey, DEFAULT_PANIC_VK};
use crate::shared::SharedState;
use crate::sink_win32::SystemInputSink;
use crate::wait::HybridWaiter;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Owns the engine thread and the emergency-stop hotkey.
#[derive(Debug)]
pub struct EngineHandle {
    shared: Arc<SharedState>,
    thread: Option<std::thread::JoinHandle<()>>,
    pinned_core: Arc<AtomicU32>,
    /// Set by the engine thread once `run` returns. Lets shutdown distinguish
    /// "loop finished" from "thread wedged in a syscall".
    finished: Arc<AtomicBool>,
    _hotkey: PanicHotkey,
}

const NOT_PINNED: u32 = u32::MAX;

/// How long shutdown waits for the engine loop to exit before detaching.
///
/// The loop itself exits in well under a millisecond — measured at 0.4–2 ms
/// across null-sink, throttled, unthrottled, and F8-stopped shutdowns — so this
/// budget is enormous for the healthy case and only matters when the thread is
/// genuinely stuck.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(2000);

impl EngineHandle {
    /// Start with the production `SystemInputSink` (mouse or keyboard, chosen
    /// at runtime by `SharedState::click_kind`).
    pub fn start(shared: Arc<SharedState>) -> Result<Self, HotkeyError> {
        let for_sink = shared.clone();
        Self::start_with_sink(shared, move |_clock| SystemInputSink::new(for_sink))
    }

    /// Start with a caller-supplied sink, built on the engine thread.
    ///
    /// `make_sink` receives the engine's `QpcClock` so a wrapper can timestamp
    /// against the same clock the scheduler uses. This exists so the bench can
    /// wrap `SendInputSink` in a `ProbedSink` and measure emission timing
    /// without the receiver's message loop in the path — the delivered-side
    /// intervals are bounded by that loop's cadence and cannot characterise
    /// the engine.
    pub fn start_with_sink<F, S>(
        shared: Arc<SharedState>,
        make_sink: F,
    ) -> Result<Self, HotkeyError>
    where
        F: FnOnce(QpcClock) -> S + Send + 'static,
        S: crate::sink::ClickSink + Send + 'static,
    {
        // The kill switch is registered BEFORE the engine thread exists, so
        // there is never a window in which clicks can be emitted with no way
        // to stop them.
        let hotkey = PanicHotkey::register(shared.clone(), DEFAULT_PANIC_VK)?;

        let pinned_core = Arc::new(AtomicU32::new(NOT_PINNED));
        let pinned_for_thread = pinned_core.clone();
        let shared_for_thread = shared.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_for_thread = finished.clone();

        // `start` must not return before the engine thread has pinned itself,
        // raised its priority, and constructed everything the loop touches.
        // Without this the caller races the thread's setup: `pinned_core()`
        // reads NOT_PINNED and reports "unpinned" for a thread that is about
        // to pin perfectly well, and a caller could set `running` before the
        // engine is configured.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();

        let thread = std::thread::Builder::new()
            .name("clicker-engine".into())
            .spawn(move || {
                // Pin first so the priority raise applies to the final core.
                if let Some(core) = pin_current_thread_to_best() {
                    pinned_for_thread.store(core, Ordering::Relaxed);
                }
                // Guard restores the previous priority on every exit path,
                // including unwind.
                let _priority = ThreadPriorityGuard::time_critical();

                // Everything the loop touches is built before entering it.
                let clock = QpcClock::new();
                let waiter = HybridWaiter::new();
                let sink = make_sink(clock);
                let mut engine = Engine::new(clock, waiter, sink);

                // Setup complete; the loop is about to start.
                let _ = ready_tx.send(());

                engine.run(&shared_for_thread);
                finished_for_thread.store(true, Ordering::Release);
            })
            .expect("failed to spawn engine thread");

        // If the thread died during setup the channel closes and recv errors;
        // either way we proceed only once setup is settled.
        let _ = ready_rx.recv();

        Ok(Self { shared, thread: Some(thread), pinned_core, finished, _hotkey: hotkey })
    }

    /// The cpu set the engine thread was pinned to, if pinning succeeded.
    pub fn pinned_core(&self) -> Option<u32> {
        match self.pinned_core.load(Ordering::Relaxed) {
            NOT_PINNED => None,
            c => Some(c),
        }
    }
}

impl EngineHandle {
    /// Signal shutdown and wait up to [`SHUTDOWN_TIMEOUT`] for the engine loop
    /// to exit. Returns `true` if it exited, `false` if it is still wedged.
    ///
    /// Called automatically by `Drop`; exposed so callers can distinguish a
    /// clean stop from a stuck one.
    pub fn shutdown(&mut self) -> bool {
        self.shared.set_running(false);
        self.shared.request_shutdown();

        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        while !self.finished.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }

        if self.finished.load(Ordering::Acquire) {
            // The loop is out; the join is now guaranteed not to block.
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            true
        } else {
            // The thread is stuck inside a call we cannot interrupt — most
            // plausibly SendInput blocking on a saturated input queue, which is
            // reachable in follow-cursor mode at ceiling rates. Joining here
            // would hang the process forever, which is strictly worse than
            // letting it exit: an unkillable clicker is the failure mode this
            // whole design exists to prevent.
            //
            // Dropping the JoinHandle detaches. The thread owns its own Arc
            // clones, so detaching is sound; the OS reclaims it at process exit.
            self.thread.take();
            false
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        if !self.shutdown() {
            // Not a panic: Drop must not unwind, and the process is on its way
            // out regardless. Say so plainly rather than exiting silently.
            eprintln!(
                "clicker-core: engine thread did not exit within {:?}; detaching. \
                 It is most likely blocked inside SendInput because the target's \
                 input queue is saturated.",
                SHUTDOWN_TIMEOUT
            );
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::shared::SharedState;
    use crate::testlock::f8_guard;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn starts_idle_and_shuts_down_cleanly() {
        let _serial = f8_guard();
        let shared = Arc::new(SharedState::new());
        let h = EngineHandle::start(shared.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(shared.clicks_emitted(), 0, "an idle engine must emit nothing");
        drop(h);
        assert!(shared.shutdown());
    }

    #[test]
    fn pinned_core_is_reported_without_racing_thread_setup() {
        let _serial = f8_guard();
        // `start` returning before the engine thread pinned itself made
        // `pinned_core()` report None for a thread that pinned fine.
        let shared = Arc::new(SharedState::new());
        let h = EngineHandle::start(shared.clone()).unwrap();
        assert!(
            h.pinned_core().is_some(),
            "engine thread should be pinned by the time start() returns"
        );
    }

    /// A sink wedged inside an uninterruptible call must not hang the process.
    ///
    /// `SendInput` blocks when the target thread's input queue saturates, which
    /// is reachable in follow-cursor mode at ceiling rates against unresponsive
    /// desktop UI. Observed in the wild: a soak run survived 2m12s past its 20s
    /// bound with the engine thread parked in a syscall. A clicker that cannot
    /// be terminated is a worse failure than the one the kill switch solves.
    #[test]
    fn shutdown_is_bounded_even_if_the_sink_wedges() {
        use crate::sink::{ClickSink, SinkError};
        use crate::shared::Button;

        struct WedgedSink;
        impl ClickSink for WedgedSink {
            fn emit_batch(
                &mut self,
                _b: Button,
                _p: Option<(i32, i32)>,
                _count: u16,
            ) -> Result<u16, SinkError> {
                // Stands in for SendInput blocking on a saturated input queue.
                std::thread::sleep(Duration::from_secs(3600));
                Ok(0)
            }
        }

        let _serial = f8_guard();
        let shared = Arc::new(SharedState::new());
        shared.set_interval_ns(0);
        let h = EngineHandle::start_with_sink(shared.clone(), |_clk| WedgedSink).unwrap();
        shared.set_running(true);
        // Give the engine time to enter the wedged emit call.
        std::thread::sleep(Duration::from_millis(100));

        let t = Instant::now();
        drop(h);
        let elapsed = t.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "drop() hung on a wedged engine thread: {elapsed:?}"
        );
    }

    /// Emits real clicks. Ignored by default so it never fires during an
    /// ordinary `cargo test` run and starts clicking the developer's desktop.
    #[test]
    #[ignore = "moves the real mouse; run explicitly"]
    fn emits_a_bounded_burst_and_stops_at_the_limit() {
        let _serial = f8_guard();
        let shared = Arc::new(SharedState::new());
        shared.set_interval_ns(10_000_000); // 100 CPS
        shared.set_limit_clicks(5);
        let _h = EngineHandle::start(shared.clone()).unwrap();
        shared.set_running(true);

        let start = Instant::now();
        while shared.clicks_emitted() < 5 && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(shared.clicks_emitted(), 5);
        assert!(!shared.running());
    }
}
