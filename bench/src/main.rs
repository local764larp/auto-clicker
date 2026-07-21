//! Phase 2 measurement harness.
//!
//! Reports emitted vs. delivered CPS and the interval distribution across a
//! sweep of target rates and batch sizes. Every performance claim about this
//! project must trace back to output from this binary.

mod stats;

#[cfg(not(windows))]
fn main() {
    eprintln!("bench is Windows-only");
}

#[cfg(windows)]
fn main() {
    windows_main::run();
}

#[cfg(windows)]
mod windows_main {
    use crate::stats::{summarize, Summary};
    use clicker_core::clock::{Clock, QpcClock};
    use clicker_core::runtime::EngineHandle;
    use clicker_core::shared::{PositionMode, SharedState};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, RegisterClassExW,
        SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
        HWND_TOPMOST, MSG, PM_REMOVE, SWP_SHOWWINDOW, SW_SHOW, WM_LBUTTONDOWN, WNDCLASSEXW,
        WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW,
    };

    const WIN_X: i32 = 200;
    const WIN_Y: i32 = 200;
    const WIN_W: i32 = 600;
    const WIN_H: i32 = 400;

    /// Timestamps of delivered clicks. Pre-allocated so the wndproc never grows it.
    static DELIVERED: OnceLock<Mutex<Vec<u64>>> = OnceLock::new();
    static DELIVERED_COUNT: AtomicUsize = AtomicUsize::new(0);
    static CLOCK: OnceLock<QpcClock> = OnceLock::new();

    fn delivered() -> &'static Mutex<Vec<u64>> {
        DELIVERED.get_or_init(|| Mutex::new(Vec::with_capacity(4_000_000)))
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if msg == WM_LBUTTONDOWN {
            let now = CLOCK.get().map(|c| c.now_ns()).unwrap_or(0);
            if let Ok(mut v) = delivered().lock() {
                v.push(now);
            }
            DELIVERED_COUNT.fetch_add(1, Ordering::Relaxed);
            return LRESULT(0);
        }
        // SAFETY: forwarding unhandled messages with the exact arguments received.
        unsafe { DefWindowProcW(hwnd, msg, w, l) }
    }

    fn create_receiver() -> (HWND, (i32, i32)) {
        // SAFETY: standard window registration and creation. All string
        // pointers below reference NUL-terminated UTF-16 buffers that outlive
        // the calls, and the class name is registered before it is used.
        unsafe {
            let hinst = GetModuleHandleW(None).expect("GetModuleHandleW failed");
            let class: Vec<u16> = "ClickerBenchReceiver\0".encode_utf16().collect();

            let wc = WNDCLASSEXW {
                cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: hinst.into(),
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            RegisterClassExW(&wc);

            let title: Vec<u16> = "Clicker Bench Receiver\0".encode_utf16().collect();
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST,
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW,
                WIN_X,
                WIN_Y,
                WIN_W,
                WIN_H,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .expect("failed to create receiver window");

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ =
                SetWindowPos(hwnd, Some(HWND_TOPMOST), WIN_X, WIN_Y, WIN_W, WIN_H, SWP_SHOWWINDOW);
            let _ = SetForegroundWindow(hwnd);

            (hwnd, (WIN_X + WIN_W / 2, WIN_Y + WIN_H / 2))
        }
    }

    /// Maximum messages drained per `pump()` call.
    ///
    /// This bound is load-bearing, not defensive. An unbounded
    /// `while PeekMessageW(..) {}` never terminates once the engine injects
    /// faster than this thread can consume — new messages keep arriving before
    /// the queue empties, so the caller's wall-clock deadline is never
    /// re-checked and the harness hangs. That is precisely the regime the
    /// unthrottled cells operate in.
    const MAX_DRAIN_PER_PUMP: usize = 4096;

    /// Drain up to `MAX_DRAIN_PER_PUMP` messages. This thread's consumption
    /// rate is the very bound that makes delivered < emitted.
    fn pump() {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid writable MSG for each call.
        unsafe {
            for _ in 0..MAX_DRAIN_PER_PUMP {
                if !PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    struct Row {
        label: String,
        batch: u16,
        emitted: u64,
        delivered: usize,
        emitted_cps: f64,
        delivered_cps: f64,
        summary: Summary,
    }

    fn run_cell(
        shared: &Arc<SharedState>,
        interval_ns: u64,
        batch: u16,
        label: &str,
        secs: u64,
    ) -> Row {
        DELIVERED_COUNT.store(0, Ordering::Relaxed);
        delivered().lock().unwrap().clear();

        shared.set_interval_ns(interval_ns);
        shared.set_batch_size(batch);
        let before = shared.clicks_emitted();

        let clock = CLOCK.get().unwrap();
        let t0 = clock.now_ns();
        shared.set_running(true);

        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(secs) {
            pump();
            std::thread::sleep(Duration::from_millis(1));
        }

        shared.set_running(false);
        let t1 = clock.now_ns();
        // Drain anything still queued.
        for _ in 0..200 {
            pump();
            std::thread::sleep(Duration::from_millis(1));
        }

        let emitted = shared.clicks_emitted() - before;
        let elapsed_s = (t1 - t0) as f64 / 1e9;

        let mut times = delivered().lock().unwrap().clone();
        times.sort_unstable();
        let mut intervals: Vec<u64> = times.windows(2).map(|w| w[1].saturating_sub(w[0])).collect();
        let summary = summarize(&mut intervals);
        let delivered_n = times.len();

        Row {
            label: label.to_string(),
            batch,
            emitted,
            delivered: delivered_n,
            emitted_cps: emitted as f64 / elapsed_s,
            delivered_cps: delivered_n as f64 / elapsed_s,
            summary,
        }
    }

    pub fn run() {
        CLOCK.get_or_init(QpcClock::new);

        let (_hwnd, centre) = create_receiver();
        // Let the window settle and take focus before measuring.
        for _ in 0..500 {
            pump();
            std::thread::sleep(Duration::from_millis(1));
        }

        let shared = Arc::new(SharedState::new());
        shared.set_position_mode(PositionMode::FixedPoint);
        shared.set_fixed_point(centre.0, centre.1);

        let handle = EngineHandle::start(shared.clone())
            .expect("engine failed to start — the emergency-stop hotkey is required");
        println!("engine pinned to core {:?}", handle.pinned_core());
        println!("receiver at ({WIN_X}, {WIN_Y}), clicking its centre {centre:?}\n");

        let secs = 5u64;
        let mut rows = Vec::new();

        // Throttled sweep: batching is deliberately not applied here.
        for (cps, label) in [
            (50u64, "50 CPS"),
            (100, "100 CPS"),
            (250, "250 CPS"),
            (500, "500 CPS"),
            (1000, "1000 CPS"),
            (2000, "2000 CPS"),
            (5000, "5000 CPS"),
        ] {
            let interval = 1_000_000_000u64 / cps;
            let r = run_cell(&shared, interval, 1, label, secs);
            println!(
                "  {:<12}       emitted {:>9.0} CPS   delivered {:>9.0} CPS",
                r.label, r.emitted_cps, r.delivered_cps
            );
            rows.push(r);
        }

        // Unthrottled sweep across batch sizes. Whether delivered CPS actually
        // rises with K is the empirical question this harness exists to answer.
        for batch in [1u16, 2, 4, 8, 16] {
            let r = run_cell(&shared, 0, batch, "unthrottled", secs);
            println!(
                "  {:<12} K={:<3} emitted {:>9.0} CPS   delivered {:>9.0} CPS",
                r.label, r.batch, r.emitted_cps, r.delivered_cps
            );
            rows.push(r);
        }

        println!("\n| Target | Batch K | Emitted CPS | Delivered CPS | Ratio | mean us | p50 us | p99 us | max us |");
        println!("|---|---|---|---|---|---|---|---|---|");
        for r in &rows {
            let ratio = if r.emitted > 0 { r.delivered as f64 / r.emitted as f64 } else { 0.0 };
            println!(
                "| {} | {} | {:.0} | {:.0} | {:.1}% | {:.1} | {:.1} | {:.1} | {:.1} |",
                r.label,
                r.batch,
                r.emitted_cps,
                r.delivered_cps,
                ratio * 100.0,
                r.summary.mean_ns / 1000.0,
                r.summary.p50_ns as f64 / 1000.0,
                r.summary.p99_ns as f64 / 1000.0,
                r.summary.max_ns as f64 / 1000.0,
            );
        }

        println!(
            "\nDelivered below emitted is expected, not a bug: SendInput serializes\n\
             through the system raw input thread and the receiver consumes on its\n\
             own message loop."
        );
    }
}
