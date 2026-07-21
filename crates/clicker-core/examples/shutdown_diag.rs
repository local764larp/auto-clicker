//! Diagnostic for the shutdown hang.
//!
//! Runs the identical start -> run -> stop -> drop sequence twice: once with a
//! sink that never calls SendInput, once with the real one. Times each stage so
//! the blocking component is identified rather than guessed at.
//!
//! Emits NO real clicks in the null-sink pass.

#[cfg(windows)]
fn main() {
    use clicker_core::runtime::EngineHandle;
    use clicker_core::shared::{Button, PositionMode, SharedState};
    use clicker_core::sink::{ClickSink, SinkError};
    use clicker_core::sink_win32::SendInputSink;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// Counts clicks; never touches the input stack.
    struct NullSink(Arc<AtomicU64>);
    impl ClickSink for NullSink {
        fn emit_batch(
            &mut self,
            _b: Button,
            _p: Option<(i32, i32)>,
            count: u16,
        ) -> Result<u16, SinkError> {
            self.0.fetch_add(count as u64, Ordering::Relaxed);
            Ok(count)
        }
    }

    fn timed_shutdown(label: &str, handle: EngineHandle, shared: &Arc<SharedState>) {
        println!("  [{label}] stopping engine...");
        let t = Instant::now();
        shared.set_running(false);
        println!("  [{label}] running=false took {:?}", t.elapsed());

        let t = Instant::now();
        drop(handle);
        println!("  [{label}] drop(handle) took {:?}", t.elapsed());
    }

    // --- Pass 1: no SendInput anywhere ---
    println!("PASS 1: NullSink (no SendInput)");
    {
        let shared = Arc::new(SharedState::new());
        shared.set_interval_ns(0); // unthrottled, same as soak
        let counter = Arc::new(AtomicU64::new(0));
        let c = counter.clone();
        let handle =
            EngineHandle::start_with_sink(shared.clone(), move |_clk| NullSink(c)).unwrap();
        shared.set_running(true);
        std::thread::sleep(Duration::from_millis(500));
        println!("  emitted {} clicks", counter.load(Ordering::Relaxed));
        timed_shutdown("null", handle, &shared);
    }

    // --- Pass 2: the real sink, clicking a harmless fixed point ---
    println!("\nPASS 2: SendInputSink (real clicks at 100 CPS for 500ms)");
    {
        let shared = Arc::new(SharedState::new());
        shared.set_interval_ns(10_000_000); // 100 CPS — gentle
        shared.set_position_mode(PositionMode::FixedPoint);
        shared.set_fixed_point(2, 2); // top-left corner, harmless
        let handle =
            EngineHandle::start_with_sink(shared.clone(), |_clk| SendInputSink::new()).unwrap();
        shared.set_running(true);
        std::thread::sleep(Duration::from_millis(500));
        println!("  emitted {} clicks", shared.clicks_emitted());
        timed_shutdown("sendinput", handle, &shared);
    }

    // --- Pass 3: soak's actual condition — unthrottled, real SendInput ---
    println!("\nPASS 3: SendInputSink UNTHROTTLED (soak's condition)");
    {
        let shared = Arc::new(SharedState::new());
        shared.set_interval_ns(0); // saturate the input stack
        shared.set_position_mode(PositionMode::FixedPoint);
        shared.set_fixed_point(2, 2);
        let handle =
            EngineHandle::start_with_sink(shared.clone(), |_clk| SendInputSink::new()).unwrap();
        shared.set_running(true);
        std::thread::sleep(Duration::from_millis(800));
        println!("  emitted {} clicks", shared.clicks_emitted());
        timed_shutdown("unthrottled", handle, &shared);
    }

    // --- Pass 4: soak's EXACT path — stopped by a real F8 keypress ---
    // This is the one condition passes 1-3 do not cover: the stop originates
    // inside the hotkey thread's message loop, not from main.
    println!("\nPASS 4: stopped by a synthesized F8 keypress (soak's exact path)");
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
            KEYEVENTF_KEYUP, VK_F8,
        };

        let shared = Arc::new(SharedState::new());
        shared.set_interval_ns(0);
        shared.set_position_mode(PositionMode::FixedPoint);
        shared.set_fixed_point(2, 2);
        let handle =
            EngineHandle::start_with_sink(shared.clone(), |_clk| SendInputSink::new()).unwrap();
        shared.set_running(true);
        std::thread::sleep(Duration::from_millis(500));
        println!("  emitted {} clicks, running={}", shared.clicks_emitted(), shared.running());

        // Synthesize F8 down+up.
        let mk = |flags: KEYBD_EVENT_FLAGS| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_F8,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let keys = [mk(KEYBD_EVENT_FLAGS(0)), mk(KEYEVENTF_KEYUP)];
        // SAFETY: `keys` is a live slice of two initialised INPUT structs and
        // the size argument matches the element type.
        let sent = unsafe { SendInput(&keys, core::mem::size_of::<INPUT>() as i32) };
        println!("  F8 synthesized ({sent} events)");

        let t = Instant::now();
        while shared.running() && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        println!("  running cleared after {:?} -> running={}", t.elapsed(), shared.running());

        let t = Instant::now();
        drop(handle);
        println!("  [f8] drop(handle) took {:?}", t.elapsed());
    }

    println!("\nall passes completed — process is exiting normally");
}

#[cfg(not(windows))]
fn main() {}
