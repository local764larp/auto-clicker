//! Unthrottled soak for verifying the emergency stop under saturation.
//!
//! WARNING: this emits real mouse clicks as fast as the machine allows,
//! wherever the cursor is sitting. Park the cursor over a scratch text editor
//! or an empty desktop area — never over anything destructive — and be ready
//! to press F8.
//!
//! Run: cargo run -p clicker-core --release --example soak

#[cfg(windows)]
fn main() {
    use clicker_core::runtime::EngineHandle;
    use clicker_core::shared::SharedState;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let shared = Arc::new(SharedState::new());
    shared.set_interval_ns(0); // unthrottled
    shared.set_batch_size(1);
    shared.set_limit_ns(15_000_000_000); // 15s backstop if F8 fails

    let handle = match EngineHandle::start(shared.clone()) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("engine did not start: {e}");
            std::process::exit(1);
        }
    };
    println!("engine pinned to core {:?}", handle.pinned_core());

    println!("\n*** This will click as fast as possible wherever the cursor is. ***");
    println!("Park the cursor somewhere harmless now. Press F8 at any time to stop.");
    for n in (1..=5).rev() {
        println!("  starting in {n}...");
        std::thread::sleep(Duration::from_secs(1));
    }

    shared.set_running(true);
    let start = Instant::now();
    let mut last = 0u64;
    while shared.running() && start.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(200));
        let now = shared.clicks_emitted();
        println!(
            "  clicks: {now:>9}  (+{:>7} in 200ms => {:.0} CPS)",
            now - last,
            (now - last) as f64 * 5.0
        );
        last = now;
    }

    println!(
        "\nstopped after {:?} with {} clicks, state {:?}",
        start.elapsed(),
        shared.clicks_emitted(),
        shared.engine_state()
    );
    println!("If you pressed F8, state should be Idle (not StoppedByLimit).");
}

#[cfg(not(windows))]
fn main() {
    eprintln!("windows only");
}
