# Click Engine & Measurement Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `clicker-core` — a headless, maximum-throughput Windows click engine — plus the `bench/` harness that measures its real emitted-vs-delivered CPS ceiling.

**Architecture:** A pinned, `TIME_CRITICAL` native thread runs a hot loop that allocates nothing, locks nothing, and logs nothing. It reads configuration from a `SharedState` block of atomics and emits clicks through a `ClickSink`. The engine is generic over `Clock`, `Waiter`, and `ClickSink`, so the entire loop — limits, drift, snap-forward, unthrottled mode — is tested deterministically in virtual time with zero sleeping. Only `sink_win32`, `hotkey`, and `affinity` are `#[cfg(windows)]`; everything else compiles and tests on Linux.

**Tech Stack:** Rust 1.96.0 (`x86_64-pc-windows-msvc`), the `windows` crate, `std::sync::atomic`. No GUI framework. No async runtime. No serde in this spec.

## Global Constraints

- Rust 1.96+, target `x86_64-pc-windows-msvc`. MSVC linker verified — never switch to `gnu`.
- The hot loop **allocates nothing, locks nothing, logs nothing**. Everything is constructed before the loop.
- No `unsafe` block without a comment stating the invariant that makes it sound.
- Every raw handle and every `timeBeginPeriod` gets an RAII guard with a `Drop` impl.
- No performance claim without a measurement backing it.
- `clicker-core` must pass `cargo test -p clicker-core --target x86_64-unknown-linux-gnu`.
- Process priority class stays default. `REALTIME_PRIORITY_CLASS` is never set anywhere.
- `Acquire`/`Release` ordering on `running`, `shutdown`, `engine_state` only. `Relaxed` elsewhere.
- Out of scope: kernel drivers, HID emulation, anti-cheat evasion, signature masking, detection-defeating humanization. The goal is throughput, not concealment.

**On the `windows` crate signatures in this plan:** the Win32 call sites below are written against the `windows` 0.58-era API, where fallible calls return `Result` and optional handle parameters are wrapped in `Some(..)`. `cargo add` will pin whatever version is current, and these conventions have changed between releases. If a call site does not compile, adapt it to the resolved version's signature — check `cargo doc --open -p windows` for the exact shape. Do **not** work around a signature mismatch by adding `mem::transmute`, casting handles, or suppressing the error; the surrounding safety comments assume the documented contract holds.

---

## File Structure

| File | Platform | Responsibility |
|---|---|---|
| `Cargo.toml` | — | Workspace root, members, shared profile |
| `crates/clicker-core/src/lib.rs` | any | Module wiring, public re-exports |
| `crates/clicker-core/src/shared.rs` | any | `SharedState` atomics, `Config`, `Button`, `PositionMode`, `EngineState` |
| `crates/clicker-core/src/schedule.rs` | any | Pure `decide()`. No I/O, no clock, no allocation |
| `crates/clicker-core/src/clock.rs` | any | `Clock` trait, `QpcClock` (win), `VirtualClock` (test) |
| `crates/clicker-core/src/wait.rs` | any | `Waiter` trait, `HybridWaiter` (win), `InstantWaiter` (test) |
| `crates/clicker-core/src/sink.rs` | any | `ClickSink` trait, `SinkError`, `RecordingSink` |
| `crates/clicker-core/src/engine.rs` | any | `Engine<C, W, S>` — the hot loop |
| `crates/clicker-core/src/sink_win32.rs` | win | `SendInput` impl with batching |
| `crates/clicker-core/src/affinity.rs` | win | Thread priority + cpu-set pinning, RAII guards |
| `crates/clicker-core/src/hotkey.rs` | win | Message-only window thread owning the panic hotkey |
| `crates/clicker-core/src/runtime.rs` | win | `EngineHandle` — spawns and joins the engine thread |
| `bench/src/main.rs` | win | Receiver window, sweep driver, statistics |
| `bench/src/stats.rs` | any | Percentile / jitter math (unit tested) |

---

### Task 1: Workspace scaffold and shared control block

**Files:**
- Create: `Cargo.toml`, `crates/clicker-core/Cargo.toml`, `crates/clicker-core/src/lib.rs`, `crates/clicker-core/src/shared.rs`
- Test: inline `#[cfg(test)]` module in `shared.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `Button::{Left,Middle,Right}`, `PositionMode::{FollowCursor,FixedPoint}`, `EngineState::{Idle,Running,StoppedByLimit,Error}`, `Config { interval_ns: u64, button: Button, position: Option<(i32,i32)>, limit_clicks: u64, limit_ns: u64, batch_size: u16 }`, and `SharedState` with methods `new()`, `snapshot() -> Config`, `running() -> bool`, `set_running(bool)`, `shutdown() -> bool`, `request_shutdown()`, `engine_state() -> EngineState`, `set_engine_state(EngineState)`, `clicks_emitted() -> u64`, `store_clicks(u64)`, `set_interval_ns(u64)`, `set_button(Button)`, `set_position_mode(PositionMode)`, `set_fixed_point(i32,i32)`, `set_limit_clicks(u64)`, `set_limit_ns(u64)`, `set_batch_size(u16)`.

- [ ] **Step 1: Create the workspace root**

`Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["crates/clicker-core", "bench"]

[workspace.package]
edition = "2021"
rust-version = "1.96"

[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
```

- [ ] **Step 2: Create the core crate manifest**

`crates/clicker-core/Cargo.toml`:

```toml
[package]
name = "clicker-core"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true

[dependencies]

[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_UI_WindowsAndMessaging",
    "Win32_UI_Input_KeyboardAndMouse",
    "Win32_System_Threading",
    "Win32_System_SystemInformation",
    "Win32_System_LibraryLoader",
    "Win32_Media",
] }
```

- [ ] **Step 3: Verify the dependency version resolves**

Run: `cargo add windows --package clicker-core --target 'cfg(windows)' --features Win32_Foundation,Win32_UI_WindowsAndMessaging,Win32_UI_Input_KeyboardAndMouse,Win32_System_Threading,Win32_System_SystemInformation,Win32_System_LibraryLoader,Win32_Media`

Expected: `cargo add` rewrites the version to the current release and prints the resolved feature list. If `0.58` was already current it reports no change. Accept whatever version `cargo add` pins — do not hand-edit it back.

- [ ] **Step 4: Write the failing test for the control block**

Create `crates/clicker-core/src/shared.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reflects_writes() {
        let s = SharedState::new();
        s.set_interval_ns(1_000_000);
        s.set_button(Button::Right);
        s.set_position_mode(PositionMode::FixedPoint);
        s.set_fixed_point(640, 480);
        s.set_limit_clicks(50);
        s.set_batch_size(8);

        let cfg = s.snapshot();
        assert_eq!(cfg.interval_ns, 1_000_000);
        assert_eq!(cfg.button, Button::Right);
        assert_eq!(cfg.position, Some((640, 480)));
        assert_eq!(cfg.limit_clicks, 50);
        assert_eq!(cfg.batch_size, 8);
    }

    #[test]
    fn follow_cursor_yields_no_position() {
        let s = SharedState::new();
        s.set_fixed_point(100, 100);
        s.set_position_mode(PositionMode::FollowCursor);
        assert_eq!(s.snapshot().position, None);
    }

    #[test]
    fn defaults_are_idle_and_stopped() {
        let s = SharedState::new();
        assert!(!s.running());
        assert!(!s.shutdown());
        assert_eq!(s.engine_state(), EngineState::Idle);
        assert_eq!(s.clicks_emitted(), 0);
        assert_eq!(s.snapshot().batch_size, 1);
    }

    #[test]
    fn unknown_enum_bytes_degrade_to_defaults() {
        assert_eq!(Button::from_u8(200), Button::Left);
        assert_eq!(EngineState::from_u8(200), EngineState::Idle);
    }
}
```

- [ ] **Step 5: Run the test to verify it fails**

Run: `cargo test -p clicker-core`
Expected: FAIL — `cannot find type SharedState in this scope`, plus errors for the other missing items.

- [ ] **Step 6: Implement the control block**

Prepend to `crates/clicker-core/src/shared.rs`, above the test module:

```rust
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU64, AtomicU8, Ordering};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Button {
    Left = 0,
    Middle = 1,
    Right = 2,
}

impl Button {
    /// Saturating conversion. A corrupt byte degrades to `Left` rather than
    /// panicking — this runs in the hot loop and must never fail.
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Button::Middle,
            2 => Button::Right,
            _ => Button::Left,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PositionMode {
    FollowCursor = 0,
    FixedPoint = 1,
}

impl PositionMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => PositionMode::FixedPoint,
            _ => PositionMode::FollowCursor,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EngineState {
    Idle = 0,
    Running = 1,
    StoppedByLimit = 2,
    Error = 3,
}

impl EngineState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => EngineState::Running,
            2 => EngineState::StoppedByLimit,
            3 => EngineState::Error,
            _ => EngineState::Idle,
        }
    }
}

/// One consistent read of every config field, taken once per loop iteration.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub interval_ns: u64,
    pub button: Button,
    pub position: Option<(i32, i32)>,
    pub limit_clicks: u64,
    pub limit_ns: u64,
    pub batch_size: u16,
}

/// Shared between the GUI/host thread and the engine thread. Every field is an
/// atomic; there is no lock anywhere in this type by design.
#[derive(Debug)]
pub struct SharedState {
    running: AtomicBool,
    shutdown: AtomicBool,
    interval_ns: AtomicU64,
    button: AtomicU8,
    position_mode: AtomicU8,
    fixed_x: AtomicI32,
    fixed_y: AtomicI32,
    limit_clicks: AtomicU64,
    limit_ns: AtomicU64,
    clicks_emitted: AtomicU64,
    engine_state: AtomicU8,
    batch_size: AtomicU16,
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedState {
    pub const fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            interval_ns: AtomicU64::new(10_000_000), // 100 CPS
            button: AtomicU8::new(Button::Left as u8),
            position_mode: AtomicU8::new(PositionMode::FollowCursor as u8),
            fixed_x: AtomicI32::new(0),
            fixed_y: AtomicI32::new(0),
            limit_clicks: AtomicU64::new(0),
            limit_ns: AtomicU64::new(0),
            clicks_emitted: AtomicU64::new(0),
            engine_state: AtomicU8::new(EngineState::Idle as u8),
            batch_size: AtomicU16::new(1),
        }
    }

    /// Single `Relaxed` read of every config field. These are hints that may be
    /// one click stale, which is acceptable and cheaper than acquiring.
    pub fn snapshot(&self) -> Config {
        let mode = PositionMode::from_u8(self.position_mode.load(Ordering::Relaxed));
        let position = match mode {
            PositionMode::FollowCursor => None,
            PositionMode::FixedPoint => Some((
                self.fixed_x.load(Ordering::Relaxed),
                self.fixed_y.load(Ordering::Relaxed),
            )),
        };
        Config {
            interval_ns: self.interval_ns.load(Ordering::Relaxed),
            button: Button::from_u8(self.button.load(Ordering::Relaxed)),
            position,
            limit_clicks: self.limit_clicks.load(Ordering::Relaxed),
            limit_ns: self.limit_ns.load(Ordering::Relaxed),
            batch_size: self.batch_size.load(Ordering::Relaxed),
        }
    }

    // --- ordered flags: Acquire/Release carries meaning here ---

    pub fn running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
    pub fn set_running(&self, v: bool) {
        self.running.store(v, Ordering::Release);
    }
    pub fn shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }
    pub fn engine_state(&self) -> EngineState {
        EngineState::from_u8(self.engine_state.load(Ordering::Acquire))
    }
    pub fn set_engine_state(&self, s: EngineState) {
        self.engine_state.store(s as u8, Ordering::Release);
    }

    // --- counter: engine is the sole writer, GUI is a pure reader ---

    pub fn clicks_emitted(&self) -> u64 {
        self.clicks_emitted.load(Ordering::Relaxed)
    }
    /// Plain store, never `fetch_add`. The engine owns this value; a locked
    /// read-modify-write per click is wasted work at ceiling rates.
    pub fn store_clicks(&self, v: u64) {
        self.clicks_emitted.store(v, Ordering::Relaxed);
    }

    // --- config setters ---

    pub fn set_interval_ns(&self, v: u64) {
        self.interval_ns.store(v, Ordering::Relaxed);
    }
    pub fn set_button(&self, b: Button) {
        self.button.store(b as u8, Ordering::Relaxed);
    }
    pub fn set_position_mode(&self, m: PositionMode) {
        self.position_mode.store(m as u8, Ordering::Relaxed);
    }
    pub fn set_fixed_point(&self, x: i32, y: i32) {
        self.fixed_x.store(x, Ordering::Relaxed);
        self.fixed_y.store(y, Ordering::Relaxed);
    }
    pub fn set_limit_clicks(&self, v: u64) {
        self.limit_clicks.store(v, Ordering::Relaxed);
    }
    pub fn set_limit_ns(&self, v: u64) {
        self.limit_ns.store(v, Ordering::Relaxed);
    }
    pub fn set_batch_size(&self, v: u16) {
        self.batch_size.store(v.max(1), Ordering::Relaxed);
    }
}
```

- [ ] **Step 7: Create the lib root**

`crates/clicker-core/src/lib.rs`:

```rust
//! Headless maximum-throughput click engine.
//!
//! The timing brain (`shared`, `schedule`, `clock`, `wait`, `sink`, `engine`)
//! is platform-neutral and tests on any target. Only `sink_win32`, `affinity`,
//! `hotkey`, and `runtime` are Windows-specific.

pub mod shared;

pub use shared::{Button, Config, EngineState, PositionMode, SharedState};
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p clicker-core`
Expected: PASS, 4 tests.

- [ ] **Step 9: Verify the cross-target constraint holds from day one**

Run: `cargo test -p clicker-core --target x86_64-unknown-linux-gnu`
Expected: PASS, 4 tests. If this fails, a Windows-only item leaked into a platform-neutral module — fix it now, not later.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml crates/clicker-core
git commit -m "feat(core): workspace scaffold and atomic control block"
```

---

### Task 2: Pure scheduling decision

**Files:**
- Create: `crates/clicker-core/src/schedule.rs`
- Modify: `crates/clicker-core/src/lib.rs`
- Test: inline `#[cfg(test)]` module in `schedule.rs`

**Interfaces:**
- Consumes: `Config` from Task 1.
- Produces: `StopReason::{ClickLimit,TimeLimit}`, `Decision::{Fire{next_deadline_ns:u64}, Wait{until_ns:u64}, Stop(StopReason)}`, `RunState { now_ns: u64, deadline_ns: u64, clicks_this_run: u64, elapsed_ns: u64 }`, `decide(run: RunState, cfg: &Config) -> Decision`, and the constants `SNAP_PERIODS: u64 = 4`, `SNAP_FLOOR_NS: u64 = 2_000_000`.

This is the most important test suite in the project. Every timing behaviour the engine has is decided here, in a function with no clock and no I/O.

- [ ] **Step 1: Write the failing tests**

Create `crates/clicker-core/src/schedule.rs` containing only this test module for now:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p clicker-core schedule`
Expected: FAIL — `cannot find function decide in this scope`, plus errors for `Decision`, `RunState`, `StopReason`.

- [ ] **Step 3: Implement the decision function**

Prepend to `crates/clicker-core/src/schedule.rs`, above the test module:

```rust
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
```

- [ ] **Step 4: Wire the module in**

In `crates/clicker-core/src/lib.rs`, add after `pub mod shared;`:

```rust
pub mod schedule;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p clicker-core schedule`
Expected: PASS, 14 tests.

- [ ] **Step 6: Verify cross-target**

Run: `cargo test -p clicker-core --target x86_64-unknown-linux-gnu`
Expected: PASS, 18 tests total.

- [ ] **Step 7: Commit**

```bash
git add crates/clicker-core/src/schedule.rs crates/clicker-core/src/lib.rs
git commit -m "feat(core): pure scheduling decision with drift-free advancement"
```

---

### Task 3: Clock and Waiter abstractions

**Files:**
- Create: `crates/clicker-core/src/clock.rs`, `crates/clicker-core/src/wait.rs`
- Modify: `crates/clicker-core/src/lib.rs`
- Test: inline `#[cfg(test)]` modules in both files

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `trait Clock { fn now_ns(&self) -> u64; }` with a blanket `impl<T: Clock + ?Sized> Clock for std::sync::Arc<T>`; `VirtualClock` with `new(start_ns: u64)`, `advance(&self, ns: u64)`, `set_at_least(&self, ns: u64)`; `QpcClock::new()` (Windows); `trait Waiter { fn wait_until<C: Clock>(&mut self, target_ns: u64, clock: &C); fn idle(&mut self); }`; `InstantWaiter::new(clock: Arc<VirtualClock>)`; `HybridWaiter::new()` (Windows).

The `Waiter` trait is what makes the engine testable: in tests, "waiting" means jumping the virtual clock forward instantly.

- [ ] **Step 1: Write the failing clock test**

Create `crates/clicker-core/src/clock.rs` with only this test module:

```rust
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
        fn takes_clock<C: Clock>(c: &C) -> u64 { c.now_ns() }
        assert_eq!(takes_clock(&c), 42);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p clicker-core clock`
Expected: FAIL — `cannot find type VirtualClock in this scope`.

- [ ] **Step 3: Implement the clock**

Prepend to `crates/clicker-core/src/clock.rs`:

```rust
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
```

- [ ] **Step 4: Add the `Win32_System_Performance` feature**

Run: `cargo add windows --package clicker-core --target 'cfg(windows)' --features Win32_System_Performance`

Expected: the feature is appended to the existing `windows` dependency in `crates/clicker-core/Cargo.toml`.

- [ ] **Step 5: Write the failing waiter test**

Create `crates/clicker-core/src/wait.rs` with only this test module:

```rust
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
```

- [ ] **Step 6: Run to verify it fails**

Run: `cargo test -p clicker-core wait`
Expected: FAIL — `cannot find type InstantWaiter in this scope`.

- [ ] **Step 7: Implement the waiter**

Prepend to `crates/clicker-core/src/wait.rs`:

```rust
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
                            SwitchToThread();
                        }
                    }
                } else if remaining > TIER_YIELD_NS {
                    // SAFETY: as above — no pointer arguments.
                    unsafe {
                        SwitchToThread();
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
                    SwitchToThread();
                }
            }
        }
    }
}

#[cfg(windows)]
pub use win::{HybridWaiter, TimerResolutionGuard};
```

- [ ] **Step 8: Wire the modules in**

In `crates/clicker-core/src/lib.rs`, add:

```rust
pub mod clock;
pub mod wait;
```

- [ ] **Step 9: Run all tests**

Run: `cargo test -p clicker-core`
Expected: PASS, 24 tests.

Run: `cargo test -p clicker-core --target x86_64-unknown-linux-gnu`
Expected: PASS, 24 tests. The Windows modules are `cfg`-gated out and the rest still builds.

- [ ] **Step 10: Commit**

```bash
git add crates/clicker-core
git commit -m "feat(core): Clock and Waiter abstractions with three-tier hybrid wait"
```

---

### Task 4: ClickSink trait and recording sink

**Files:**
- Create: `crates/clicker-core/src/sink.rs`
- Modify: `crates/clicker-core/src/lib.rs`
- Test: inline `#[cfg(test)]` module in `sink.rs`

**Interfaces:**
- Consumes: `Button` from Task 1, `Clock`/`VirtualClock` from Task 3.
- Produces: `SinkError::Blocked { inserted: u32, expected: u32, last_error: u32 }`; `trait ClickSink { fn emit_batch(&mut self, button: Button, pos: Option<(i32,i32)>, count: u16) -> Result<u16, SinkError>; fn emit(&mut self, button: Button, pos: Option<(i32,i32)>) -> Result<(), SinkError>; }`; `RecordedClick { button: Button, pos: Option<(i32,i32)>, at_ns: u64 }`; `RecordingSink::new(clock: Arc<VirtualClock>)`, `::with_cost(clock, cost_ns)`, `::fail_after(&mut self, n: u64)`, `::clicks(&self) -> &[RecordedClick]`, `::total(&self) -> u64`.

`RecordingSink` models per-emit syscall cost by advancing the virtual clock, so engine tests exercise realistic behaviour without a real syscall.

- [ ] **Step 1: Write the failing tests**

Create `crates/clicker-core/src/sink.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::VirtualClock;
    use std::sync::Arc;

    #[test]
    fn records_button_position_and_time() {
        let clock = Arc::new(VirtualClock::new(500));
        let mut s = RecordingSink::new(clock.clone());
        s.emit(Button::Right, Some((10, 20))).unwrap();
        assert_eq!(s.clicks().len(), 1);
        assert_eq!(s.clicks()[0].button, Button::Right);
        assert_eq!(s.clicks()[0].pos, Some((10, 20)));
        assert_eq!(s.clicks()[0].at_ns, 500);
    }

    #[test]
    fn batch_records_each_click_and_returns_the_count() {
        let clock = Arc::new(VirtualClock::new(0));
        let mut s = RecordingSink::new(clock.clone());
        assert_eq!(s.emit_batch(Button::Left, None, 4).unwrap(), 4);
        assert_eq!(s.clicks().len(), 4);
        assert_eq!(s.total(), 4);
    }

    #[test]
    fn per_emit_cost_advances_the_clock() {
        let clock = Arc::new(VirtualClock::new(0));
        let mut s = RecordingSink::with_cost(clock.clone(), 1_000);
        s.emit_batch(Button::Left, None, 3).unwrap();
        assert_eq!(clock.now_ns(), 3_000);
    }

    #[test]
    fn fail_after_reports_a_blocked_error() {
        let clock = Arc::new(VirtualClock::new(0));
        let mut s = RecordingSink::new(clock.clone());
        s.fail_after(2);
        assert!(s.emit(Button::Left, None).is_ok());
        assert!(s.emit(Button::Left, None).is_ok());
        let err = s.emit(Button::Left, None).unwrap_err();
        assert!(matches!(err, SinkError::Blocked { .. }));
    }

    #[test]
    fn zero_count_batch_is_a_noop() {
        let clock = Arc::new(VirtualClock::new(0));
        let mut s = RecordingSink::new(clock.clone());
        assert_eq!(s.emit_batch(Button::Left, None, 0).unwrap(), 0);
        assert_eq!(s.clicks().len(), 0);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p clicker-core sink`
Expected: FAIL — `cannot find type RecordingSink in this scope`.

- [ ] **Step 3: Implement the sink**

Prepend to `crates/clicker-core/src/sink.rs`:

```rust
use crate::shared::Button;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SinkError {
    /// Fewer events were accepted than submitted. Most often UIPI refusing to
    /// inject into a higher-integrity target, or a locked workstation /
    /// secure desktop. Never treat this as a silent no-op.
    Blocked { inserted: u32, expected: u32, last_error: u32 },
}

impl core::fmt::Display for SinkError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SinkError::Blocked { inserted, expected, last_error } => write!(
                f,
                "input blocked: {inserted}/{expected} events accepted (GetLastError={last_error}). \
                 The target window may run at a higher integrity level; running this tool \
                 elevated would be required to click it."
            ),
        }
    }
}

impl std::error::Error for SinkError {}

pub trait ClickSink {
    /// Emit `count` complete clicks. Returns how many were emitted.
    fn emit_batch(
        &mut self,
        button: Button,
        pos: Option<(i32, i32)>,
        count: u16,
    ) -> Result<u16, SinkError>;

    fn emit(&mut self, button: Button, pos: Option<(i32, i32)>) -> Result<(), SinkError> {
        self.emit_batch(button, pos, 1).map(|_| ())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RecordedClick {
    pub button: Button,
    pub pos: Option<(i32, i32)>,
    pub at_ns: u64,
}

/// Test sink. Timestamps every call against a `VirtualClock` and can model
/// per-emit syscall cost, so engine tests see realistic time advancement.
#[derive(Debug)]
pub struct RecordingSink {
    clock: std::sync::Arc<crate::clock::VirtualClock>,
    cost_ns: u64,
    clicks: Vec<RecordedClick>,
    fail_after: Option<u64>,
    total: u64,
}

impl RecordingSink {
    pub fn new(clock: std::sync::Arc<crate::clock::VirtualClock>) -> Self {
        Self::with_cost(clock, 0)
    }

    pub fn with_cost(clock: std::sync::Arc<crate::clock::VirtualClock>, cost_ns: u64) -> Self {
        Self { clock, cost_ns, clicks: Vec::new(), fail_after: None, total: 0 }
    }

    /// After `n` successful clicks, every subsequent emit returns `Blocked`.
    pub fn fail_after(&mut self, n: u64) {
        self.fail_after = Some(n);
    }

    pub fn clicks(&self) -> &[RecordedClick] {
        &self.clicks
    }

    pub fn total(&self) -> u64 {
        self.total
    }
}

impl ClickSink for RecordingSink {
    fn emit_batch(
        &mut self,
        button: Button,
        pos: Option<(i32, i32)>,
        count: u16,
    ) -> Result<u16, SinkError> {
        use crate::clock::Clock;
        for _ in 0..count {
            if let Some(limit) = self.fail_after {
                if self.total >= limit {
                    return Err(SinkError::Blocked {
                        inserted: 0,
                        expected: count as u32 * 2,
                        last_error: 5, // ERROR_ACCESS_DENIED
                    });
                }
            }
            self.clicks.push(RecordedClick { button, pos, at_ns: self.clock.now_ns() });
            self.total += 1;
            if self.cost_ns > 0 {
                self.clock.advance(self.cost_ns);
            }
        }
        Ok(count)
    }
}
```

- [ ] **Step 4: Wire the module in**

In `crates/clicker-core/src/lib.rs`, add:

```rust
pub mod sink;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p clicker-core sink`
Expected: PASS, 5 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/clicker-core
git commit -m "feat(core): ClickSink trait and recording test sink"
```

---

### Task 5: The engine loop

**Files:**
- Create: `crates/clicker-core/src/engine.rs`
- Modify: `crates/clicker-core/src/lib.rs`
- Test: inline `#[cfg(test)]` module in `engine.rs`

**Interfaces:**
- Consumes: everything from Tasks 1–4.
- Produces: `Engine<C: Clock, W: Waiter, S: ClickSink>` with `new(clock: C, waiter: W, sink: S) -> Self`, `run(&mut self, shared: &SharedState)`, `sink(&self) -> &S`, `into_sink(self) -> S`.

The loop allocates nothing, locks nothing, and logs nothing. Because it is generic over `Clock` and `Waiter`, these tests run in virtual time with no sleeping and no tolerance windows.

- [ ] **Step 1: Write the failing tests**

Create `crates/clicker-core/src/engine.rs` with only this test module:

```rust
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
    fn harness(cost_ns: u64) -> (Arc<VirtualClock>, Engine<Arc<VirtualClock>, InstantWaiter, RecordingSink>) {
        let clock = Arc::new(VirtualClock::new(0));
        let waiter = InstantWaiter::new(clock.clone());
        let sink = RecordingSink::with_cost(clock.clone(), cost_ns);
        (clock.clone(), Engine::new(clock, waiter, sink))
    }

    #[test]
    fn emits_exactly_the_click_limit() {
        let (_clock, mut eng) = harness(0);
        let shared = SharedState::new();
        shared.set_interval_ns(1_000_000);
        shared.set_limit_clicks(10);
        shared.set_running(true);

        // Shutdown once the limit stops the run, so run() terminates.
        std::thread::scope(|s| {
            s.spawn(|| {
                while shared.engine_state() != EngineState::StoppedByLimit {
                    std::hint::spin_loop();
                }
                shared.request_shutdown();
            });
            eng.run(&shared);
        });

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

        std::thread::scope(|s| {
            s.spawn(|| {
                while shared.engine_state() != EngineState::StoppedByLimit {
                    std::hint::spin_loop();
                }
                shared.request_shutdown();
            });
            eng.run(&shared);
        });

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

        std::thread::scope(|s| {
            s.spawn(|| {
                while shared.engine_state() != EngineState::StoppedByLimit {
                    std::hint::spin_loop();
                }
                shared.request_shutdown();
            });
            eng.run(&shared);
        });

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

        std::thread::scope(|s| {
            s.spawn(|| {
                while shared.engine_state() != EngineState::StoppedByLimit {
                    std::hint::spin_loop();
                }
                shared.request_shutdown();
            });
            eng.run(&shared);
        });

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

        std::thread::scope(|s| {
            s.spawn(|| {
                while shared.engine_state() != EngineState::StoppedByLimit {
                    std::hint::spin_loop();
                }
                shared.request_shutdown();
            });
            eng.run(&shared);
        });

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

        std::thread::scope(|s| {
            s.spawn(|| {
                while shared.engine_state() != EngineState::StoppedByLimit {
                    std::hint::spin_loop();
                }
                shared.request_shutdown();
            });
            eng.run(&shared);
        });

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

        std::thread::scope(|s| {
            s.spawn(|| {
                while shared.engine_state() != EngineState::Error {
                    std::hint::spin_loop();
                }
                shared.request_shutdown();
            });
            eng.run(&shared);
        });

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

        std::thread::scope(|s| {
            s.spawn(|| {
                while shared.engine_state() != EngineState::StoppedByLimit {
                    std::hint::spin_loop();
                }
                shared.request_shutdown();
            });
            eng.run(&shared);
        });

        assert_eq!(eng.sink().clicks()[0].pos, Some((300, 400)));
        assert_eq!(eng.sink().clicks()[0].button, Button::Middle);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p clicker-core engine`
Expected: FAIL — `cannot find type Engine in this scope`.

- [ ] **Step 3: Implement the engine**

Prepend to `crates/clicker-core/src/engine.rs`:

```rust
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
```

- [ ] **Step 4: Wire the module in**

In `crates/clicker-core/src/lib.rs`, add:

```rust
pub mod engine;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p clicker-core engine`
Expected: PASS, 10 tests. Every one completes in microseconds — there is no sleeping anywhere in this suite.

- [ ] **Step 6: Verify cross-target**

Run: `cargo test -p clicker-core --target x86_64-unknown-linux-gnu`
Expected: PASS, 39 tests.

- [ ] **Step 7: Commit**

```bash
git add crates/clicker-core
git commit -m "feat(core): engine hot loop, deterministically tested in virtual time"
```

---

### Task 6: SendInput sink with batching

**Files:**
- Create: `crates/clicker-core/src/sink_win32.rs`
- Modify: `crates/clicker-core/src/lib.rs`
- Test: inline `#[cfg(all(test, windows))]` module in `sink_win32.rs`

**Interfaces:**
- Consumes: `Button`, `ClickSink`, `SinkError`.
- Produces: `SendInputSink::new() -> Self`, `SendInputSink::refresh_virtual_screen(&mut self)`, `MAX_BATCH: usize = 32`, and `normalize_abs(x: i32, y: i32, vs: VirtualScreen) -> (i32, i32)` plus `VirtualScreen { left: i32, top: i32, width: i32, height: i32 }` for testing.

This is where the throughput win lives: `[INPUT; 2*K]` puts K complete clicks into a single `SendInput` syscall.

- [ ] **Step 1: Write the failing test for coordinate normalization**

The syscall itself cannot be unit tested, but the normalization arithmetic — the classic source of multi-monitor bugs — can be. Create `crates/clicker-core/src/sink_win32.rs` with only this test module:

```rust
#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn primary_monitor_origin_maps_to_zero() {
        let vs = VirtualScreen { left: 0, top: 0, width: 1920, height: 1080 };
        assert_eq!(normalize_abs(0, 0, vs), (0, 0));
    }

    #[test]
    fn bottom_right_maps_to_full_scale() {
        let vs = VirtualScreen { left: 0, top: 0, width: 1920, height: 1080 };
        assert_eq!(normalize_abs(1919, 1079, vs), (65535, 65535));
    }

    #[test]
    fn negative_origin_left_monitor_maps_correctly() {
        // A second monitor to the left gives a negative virtual-screen origin.
        // Getting this wrong confines all clicks to the primary display.
        let vs = VirtualScreen { left: -1920, top: 0, width: 3840, height: 1080 };
        assert_eq!(normalize_abs(-1920, 0, vs), (0, 0));
        let (mid_x, _) = normalize_abs(0, 0, vs);
        assert!((32750..=32790).contains(&mid_x), "midpoint was {mid_x}");
    }

    #[test]
    fn degenerate_virtual_screen_does_not_divide_by_zero() {
        let vs = VirtualScreen { left: 0, top: 0, width: 1, height: 1 };
        let (x, y) = normalize_abs(0, 0, vs);
        assert_eq!((x, y), (0, 0));
    }

    #[test]
    fn batch_buffer_is_preallocated_to_the_maximum() {
        let s = SendInputSink::new();
        assert_eq!(s.buffer_capacity(), MAX_BATCH * 2);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p clicker-core sink_win32`
Expected: FAIL — `cannot find type VirtualScreen in this scope`.

- [ ] **Step 3: Implement the sink**

Prepend to `crates/clicker-core/src/sink_win32.rs`:

```rust
use crate::shared::Button;
use crate::sink::{ClickSink, SinkError};
use windows::Win32::Foundation::GetLastError;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
    MOUSE_EVENT_FLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

/// Maximum clicks per syscall. The buffer is `2 * MAX_BATCH` INPUT structs,
/// allocated once at construction and never grown.
pub const MAX_BATCH: usize = 32;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VirtualScreen {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

/// Map a virtual-desktop pixel to the 0..=65535 absolute range `SendInput`
/// expects when `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` is set.
pub fn normalize_abs(x: i32, y: i32, vs: VirtualScreen) -> (i32, i32) {
    let denom_x = (vs.width - 1).max(1) as i64;
    let denom_y = (vs.height - 1).max(1) as i64;
    let nx = ((x - vs.left) as i64 * 65535 / denom_x).clamp(0, 65535) as i32;
    let ny = ((y - vs.top) as i64 * 65535 / denom_y).clamp(0, 65535) as i32;
    (nx, ny)
}

fn down_up_flags(b: Button) -> (MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS) {
    match b {
        Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
    }
}

/// Production sink. One `SendInput` call carries K complete clicks as
/// `2*K` events — the largest remaining user-mode throughput win.
pub struct SendInputSink {
    buf: Vec<INPUT>,
    vs: VirtualScreen,
}

impl SendInputSink {
    pub fn new() -> Self {
        let mut s = Self {
            buf: vec![INPUT::default(); MAX_BATCH * 2],
            vs: VirtualScreen { left: 0, top: 0, width: 1, height: 1 },
        };
        s.refresh_virtual_screen();
        s
    }

    pub fn buffer_capacity(&self) -> usize {
        self.buf.len()
    }

    /// Re-read virtual-screen metrics. Call on display change, never per click.
    pub fn refresh_virtual_screen(&mut self) {
        // SAFETY: GetSystemMetrics takes a plain enum index and returns an i32.
        // No pointers are involved and it cannot fail in a memory-unsafe way.
        unsafe {
            self.vs = VirtualScreen {
                left: GetSystemMetrics(SM_XVIRTUALSCREEN),
                top: GetSystemMetrics(SM_YVIRTUALSCREEN),
                width: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
                height: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
            };
        }
    }
}

impl Default for SendInputSink {
    fn default() -> Self {
        Self::new()
    }
}

impl ClickSink for SendInputSink {
    fn emit_batch(
        &mut self,
        button: Button,
        pos: Option<(i32, i32)>,
        count: u16,
    ) -> Result<u16, SinkError> {
        let count = (count as usize).min(MAX_BATCH);
        if count == 0 {
            return Ok(0);
        }

        let (down, up) = down_up_flags(button);

        // Fixed-position mode ORs the move onto the DOWN event rather than
        // adding a third event, so a positioned click is still one syscall and
        // still two events. Follow-cursor omits the move entirely and never
        // calls GetCursorPos — the click lands where the cursor already is.
        let (move_flags, dx, dy) = match pos {
            Some((x, y)) => {
                let (nx, ny) = normalize_abs(x, y, self.vs);
                (
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    nx,
                    ny,
                )
            }
            None => (MOUSE_EVENT_FLAGS(0), 0, 0),
        };

        for i in 0..count {
            self.buf[i * 2] = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx,
                        dy,
                        mouseData: 0,
                        dwFlags: down | move_flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            self.buf[i * 2 + 1] = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: up,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
        }

        let events = count * 2;
        // SAFETY: `self.buf` holds at least `events` initialised INPUT structs
        // (the loop above wrote every one of them), the slice is valid for the
        // duration of the call, and the size argument matches the element type
        // exactly. SendInput does not retain the pointer past return.
        let inserted = unsafe {
            SendInput(&self.buf[..events], core::mem::size_of::<INPUT>() as i32)
        };

        if inserted as usize != events {
            // SAFETY: GetLastError reads thread-local error state and takes no
            // arguments.
            let last_error = unsafe { GetLastError() }.0;
            return Err(SinkError::Blocked {
                inserted,
                expected: events as u32,
                last_error,
            });
        }

        Ok(count as u16)
    }
}
```

- [ ] **Step 4: Wire the module in**

In `crates/clicker-core/src/lib.rs`, add:

```rust
#[cfg(windows)]
pub mod sink_win32;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p clicker-core sink_win32`
Expected: PASS, 5 tests.

- [ ] **Step 6: Confirm the cross-target build is unaffected**

Run: `cargo test -p clicker-core --target x86_64-unknown-linux-gnu`
Expected: PASS, 39 tests. The new module is gated out.

- [ ] **Step 7: Commit**

```bash
git add crates/clicker-core
git commit -m "feat(core): SendInput sink with batched multi-click syscalls"
```

---

### Task 7: Thread priority and core pinning

**Files:**
- Create: `crates/clicker-core/src/affinity.rs`
- Modify: `crates/clicker-core/src/lib.rs`
- Test: inline `#[cfg(all(test, windows))]` module in `affinity.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `ThreadPriorityGuard::time_critical() -> Self` (restores the previous priority on drop); `CpuSet { id: u32, efficiency_class: u8, group: u16, logical_index: u8 }`; `select_best_cpu_set(sets: &[CpuSet]) -> Option<u32>`; `pin_current_thread_to_best() -> Option<u32>`; `enumerate_cpu_sets() -> Vec<CpuSet>`.

`select_best_cpu_set` is pure and unit tested, so the P-core preference logic is verified even though this dev machine (homogeneous Ryzen 5 7600) cannot exercise the hybrid branch at runtime.

- [ ] **Step 1: Write the failing tests**

Create `crates/clicker-core/src/affinity.rs` with only this test module:

```rust
#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn set(id: u32, eff: u8) -> CpuSet {
        CpuSet { id, efficiency_class: eff, group: 0, logical_index: id as u8 }
    }

    #[test]
    fn prefers_the_highest_efficiency_class() {
        // Intel hybrid: E-cores report a LOWER EfficiencyClass than P-cores.
        // Landing the hot loop on an E-core is a large, silent regression.
        let sets = vec![set(0, 0), set(1, 0), set(2, 1), set(3, 1)];
        let chosen = select_best_cpu_set(&sets).unwrap();
        assert!(chosen == 2 || chosen == 3, "chose {chosen}, expected a P-core");
    }

    #[test]
    fn homogeneous_cpu_picks_a_stable_non_zero_core() {
        // This dev machine: every set reports the same class.
        let sets: Vec<CpuSet> = (0..12).map(|i| set(i, 0)).collect();
        let a = select_best_cpu_set(&sets).unwrap();
        let b = select_best_cpu_set(&sets).unwrap();
        assert_eq!(a, b, "selection must be deterministic");
        assert_ne!(a, 0, "core 0 handles more interrupt and DPC work; avoid it");
    }

    #[test]
    fn single_core_machine_falls_back_to_core_zero() {
        let sets = vec![set(0, 0)];
        assert_eq!(select_best_cpu_set(&sets), Some(0));
    }

    #[test]
    fn empty_enumeration_selects_nothing() {
        assert_eq!(select_best_cpu_set(&[]), None);
    }

    #[test]
    fn enumeration_returns_at_least_one_cpu_set_on_this_machine() {
        let sets = enumerate_cpu_sets();
        assert!(!sets.is_empty(), "GetSystemCpuSetInformation returned nothing");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p clicker-core affinity`
Expected: FAIL — `cannot find type CpuSet in this scope`.

- [ ] **Step 3: Implement priority and pinning**

Prepend to `crates/clicker-core/src/affinity.rs`:

```rust
use windows::Win32::System::SystemInformation::GetSystemCpuSetInformation;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, GetThreadPriority, SetThreadPriority,
    SetThreadSelectedCpuSets, THREAD_PRIORITY, THREAD_PRIORITY_TIME_CRITICAL,
};

/// Restores the thread's previous priority on drop.
#[derive(Debug)]
pub struct ThreadPriorityGuard {
    previous: i32,
}

impl ThreadPriorityGuard {
    pub fn time_critical() -> Self {
        // SAFETY: GetCurrentThread returns a pseudo-handle that needs no
        // closing and is always valid on the calling thread. Both calls take
        // that handle plus a plain integer.
        let previous = unsafe { GetThreadPriority(GetCurrentThread()) };
        unsafe {
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        }
        Self { previous }
    }
}

impl Drop for ThreadPriorityGuard {
    fn drop(&mut self) {
        // SAFETY: as above. `previous` is whatever GetThreadPriority returned,
        // so it is a value the API already produced for this thread.
        unsafe {
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY(self.previous));
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CpuSet {
    pub id: u32,
    /// Higher is more performant. On Intel hybrid parts P-cores report a
    /// higher class than E-cores; on homogeneous parts every set reports 0.
    pub efficiency_class: u8,
    pub group: u16,
    pub logical_index: u8,
}

/// Pure selection policy, unit tested independently of the OS.
///
/// Prefers the highest `efficiency_class`. Among equals, prefers the
/// lowest-numbered set that is not the first, because core 0 absorbs more
/// interrupt and DPC work than its siblings. Deterministic by construction.
pub fn select_best_cpu_set(sets: &[CpuSet]) -> Option<u32> {
    if sets.is_empty() {
        return None;
    }
    let best_class = sets.iter().map(|s| s.efficiency_class).max()?;
    let mut candidates: Vec<&CpuSet> =
        sets.iter().filter(|s| s.efficiency_class == best_class).collect();
    candidates.sort_by_key(|s| (s.group, s.logical_index, s.id));

    if candidates.len() > 1 {
        Some(candidates[1].id)
    } else {
        Some(candidates[0].id)
    }
}

/// Enumerate the system's cpu sets. Returns an empty vec if the API fails.
pub fn enumerate_cpu_sets() -> Vec<CpuSet> {
    let mut needed: u32 = 0;

    // SAFETY: passing a null buffer with a zero length is the documented way to
    // query the required size; the call writes only to `needed`.
    unsafe {
        let _ = GetSystemCpuSetInformation(None, 0, &mut needed, GetCurrentProcess(), 0);
    }
    if needed == 0 {
        return Vec::new();
    }

    let mut buf = vec![0u8; needed as usize];
    let mut written: u32 = 0;
    // SAFETY: `buf` is `needed` bytes long, which is exactly the size the
    // previous call reported. The pointer is valid for that length and the
    // call writes at most that many bytes.
    let ok = unsafe {
        GetSystemCpuSetInformation(
            Some(buf.as_mut_ptr() as *mut _),
            needed,
            &mut written,
            GetCurrentProcess(),
            0,
        )
    };
    if ok.is_err() || written == 0 {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + core::mem::size_of::<u32>() * 2 <= written as usize {
        // SYSTEM_CPU_SET_INFORMATION layout:
        //   0: DWORD Size
        //   4: CPU_SET_INFORMATION_TYPE Type (DWORD; 0 == CpuSetInformation)
        //   8: DWORD Id
        //  12: WORD  Group
        //  14: BYTE  LogicalProcessorIndex
        //  15: BYTE  CoreIndex
        //  16: BYTE  LastLevelCacheIndex
        //  17: BYTE  NumaNodeIndex
        //  18: BYTE  EfficiencyClass
        // SAFETY: `offset` stays within `written` bytes of an initialised
        // buffer, and every read below is a plain integer at a fixed offset
        // inside one record whose declared Size we have already bounds-checked.
        let size = u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap()) as usize;
        if size == 0 || offset + size > written as usize || size < 19 {
            break;
        }
        let ty = u32::from_le_bytes(buf[offset + 4..offset + 8].try_into().unwrap());
        if ty == 0 {
            out.push(CpuSet {
                id: u32::from_le_bytes(buf[offset + 8..offset + 12].try_into().unwrap()),
                group: u16::from_le_bytes(buf[offset + 12..offset + 14].try_into().unwrap()),
                logical_index: buf[offset + 14],
                efficiency_class: buf[offset + 18],
            });
        }
        offset += size;
    }
    out
}

/// Pin the calling thread to the best available cpu set. Returns the chosen id.
///
/// Pinning avoids migration and the cold caches that come with it. On hybrid
/// parts this is also what keeps the hot loop off an E-core.
pub fn pin_current_thread_to_best() -> Option<u32> {
    let sets = enumerate_cpu_sets();
    let chosen = select_best_cpu_set(&sets)?;
    let ids = [chosen];
    // SAFETY: GetCurrentThread returns a pseudo-handle valid on this thread and
    // needing no close. `ids` is a live slice of one u32 read only during the call.
    let ok = unsafe { SetThreadSelectedCpuSets(GetCurrentThread(), &ids) };
    if ok.is_ok() {
        Some(chosen)
    } else {
        None
    }
}
```

- [ ] **Step 4: Wire the module in**

In `crates/clicker-core/src/lib.rs`, add:

```rust
#[cfg(windows)]
pub mod affinity;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p clicker-core affinity`
Expected: PASS, 5 tests.

- [ ] **Step 6: Record the untested branch**

Add to `crates/clicker-core/src/affinity.rs`, directly under the `use` block:

```rust
//! NOTE: the hybrid (P-core vs E-core) selection branch of
//! `select_best_cpu_set` is exercised by unit tests but has never been
//! validated against real hybrid silicon — the development machine is a
//! homogeneous AMD Ryzen 5 7600 where every cpu set reports
//! `efficiency_class == 0`. Treat the hybrid path as unverified on hardware.
```

- [ ] **Step 7: Commit**

```bash
git add crates/clicker-core
git commit -m "feat(core): thread priority guard and cpu-set pinning with P-core preference"
```

---

### Task 8: Emergency stop hotkey

**Files:**
- Create: `crates/clicker-core/src/hotkey.rs`
- Modify: `crates/clicker-core/src/lib.rs`
- Test: inline `#[cfg(all(test, windows))]` module in `hotkey.rs`

**Interfaces:**
- Consumes: `SharedState`, `EngineState`.
- Produces: `HotkeyError::{Register(u32), WindowCreation(u32)}`; `PanicHotkey::register(shared: Arc<SharedState>, vk: u32) -> Result<PanicHotkey, HotkeyError>` with `Drop` that unregisters and stops the thread; `DEFAULT_PANIC_VK: u32` (VK_F8).

The stop path must not depend on the GUI being responsive — at several thousand CPS the machine becomes hard to operate. The handler writes the atomic directly: no queue, no channel, no round-trip through any other subsystem.

- [ ] **Step 1: Write the failing tests**

Create `crates/clicker-core/src/hotkey.rs` with only this test module:

```rust
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::shared::SharedState;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn registers_and_unregisters_cleanly() {
        let shared = Arc::new(SharedState::new());
        let hk = PanicHotkey::register(shared.clone(), DEFAULT_PANIC_VK)
            .expect("F8 should be available on a test machine");
        drop(hk);

        // A second registration only succeeds if the first fully released F8.
        let hk2 = PanicHotkey::register(shared, DEFAULT_PANIC_VK)
            .expect("hotkey was not released on drop");
        drop(hk2);
    }

    #[test]
    fn stop_now_clears_running_immediately() {
        let shared = Arc::new(SharedState::new());
        shared.set_running(true);
        PanicHotkey::stop_now(&shared);
        assert!(!shared.running());
        assert_eq!(shared.engine_state(), EngineState::Idle);
    }

    #[test]
    fn message_thread_becomes_ready_promptly() {
        let shared = Arc::new(SharedState::new());
        let start = Instant::now();
        let hk = PanicHotkey::register(shared, DEFAULT_PANIC_VK).unwrap();
        assert!(hk.is_ready(), "thread did not signal readiness");
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p clicker-core hotkey`
Expected: FAIL — `cannot find type PanicHotkey in this scope`.

- [ ] **Step 3: Implement the hotkey thread**

Prepend to `crates/clicker-core/src/hotkey.rs`:

```rust
use crate::shared::{EngineState, SharedState};
use std::sync::mpsc;
use std::sync::Arc;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT, VK_F8,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetMessageW, PostThreadMessageW, TranslateMessage,
    DispatchMessageW, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_HOTKEY, WM_QUIT,
};

pub const DEFAULT_PANIC_VK: u32 = VK_F8.0 as u32;
const HOTKEY_ID: i32 = 0xC10C;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HotkeyError {
    /// The key is already claimed by another process.
    Register(u32),
    WindowCreation(u32),
}

impl core::fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HotkeyError::Register(e) => write!(
                f,
                "could not register the emergency-stop hotkey (GetLastError={e}); \
                 another application already owns that key. The engine must not \
                 start without a working kill switch."
            ),
            HotkeyError::WindowCreation(e) => {
                write!(f, "could not create the message-only window (GetLastError={e})")
            }
        }
    }
}

impl std::error::Error for HotkeyError {}

/// Owns a message-only window on its own thread, registers the panic hotkey,
/// and clears `running` the instant the key is pressed.
#[derive(Debug)]
pub struct PanicHotkey {
    thread_id: u32,
    handle: Option<std::thread::JoinHandle<()>>,
    ready: bool,
}

impl PanicHotkey {
    /// Clear `running` directly on the shared atomic. No queue, no channel, no
    /// round-trip through render or layout code.
    pub fn stop_now(shared: &SharedState) {
        shared.set_running(false);
        shared.set_engine_state(EngineState::Idle);
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn register(shared: Arc<SharedState>, vk: u32) -> Result<Self, HotkeyError> {
        let (tx, rx) = mpsc::channel::<Result<u32, HotkeyError>>();

        let handle = std::thread::Builder::new()
            .name("clicker-panic-hotkey".into())
            .spawn(move || {
                // SAFETY: HWND_MESSAGE creates a message-only window. All string
                // pointers point at NUL-terminated static UTF-16 below, and the
                // window is destroyed before this thread returns.
                let hwnd = unsafe {
                    let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
                    CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        PCWSTR(class.as_ptr()),
                        PCWSTR::null(),
                        WINDOW_STYLE(0),
                        0,
                        0,
                        0,
                        0,
                        Some(HWND_MESSAGE),
                        None,
                        None,
                        None,
                    )
                };

                let hwnd = match hwnd {
                    Ok(h) => h,
                    Err(_) => {
                        // SAFETY: no arguments; reads thread-local error state.
                        let e = unsafe { GetLastError() }.0;
                        let _ = tx.send(Err(HotkeyError::WindowCreation(e)));
                        return;
                    }
                };

                // SAFETY: `hwnd` is the live window created immediately above.
                // MOD_NOREPEAT suppresses auto-repeat storms while held.
                let reg = unsafe {
                    RegisterHotKey(
                        Some(hwnd),
                        HOTKEY_ID,
                        HOT_KEY_MODIFIERS(MOD_NOREPEAT.0),
                        vk,
                    )
                };

                if reg.is_err() {
                    // SAFETY: no arguments.
                    let e = unsafe { GetLastError() }.0;
                    // SAFETY: `hwnd` is live and destroyed exactly once here.
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                    let _ = tx.send(Err(HotkeyError::Register(e)));
                    return;
                }

                // SAFETY: no arguments; returns this thread's id.
                let tid = unsafe {
                    windows::Win32::System::Threading::GetCurrentThreadId()
                };
                let _ = tx.send(Ok(tid));

                let mut msg = MSG::default();
                loop {
                    // SAFETY: `msg` is a valid writable MSG. Passing a null HWND
                    // retrieves messages for any window on this thread, which is
                    // what receives both WM_HOTKEY and our WM_QUIT.
                    let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                    if got.0 <= 0 {
                        break; // WM_QUIT or error
                    }
                    if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == HOTKEY_ID {
                        Self::stop_now(&shared);
                    }
                    // SAFETY: `msg` was just filled by GetMessageW.
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }

                // SAFETY: `hwnd` is still live; UnregisterHotKey then
                // DestroyWindow each run exactly once on this path.
                unsafe {
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
                    let _ = DestroyWindow(hwnd);
                }
            })
            .expect("failed to spawn hotkey thread");

        match rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self { thread_id, handle: Some(handle), ready: true }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                Err(HotkeyError::WindowCreation(0))
            }
        }
    }
}

impl Drop for PanicHotkey {
    fn drop(&mut self) {
        // SAFETY: posting WM_QUIT to our own thread id is safe; the worst case
        // is the thread has already exited and the post fails harmlessly.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
```

- [ ] **Step 4: Wire the module in**

In `crates/clicker-core/src/lib.rs`, add:

```rust
#[cfg(windows)]
pub mod hotkey;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p clicker-core hotkey -- --test-threads=1`

Expected: PASS, 3 tests. Single-threaded because two tests both register F8 and would otherwise collide with each other.

- [ ] **Step 6: Commit**

```bash
git add crates/clicker-core
git commit -m "feat(core): emergency-stop hotkey on a message-only window thread"
```

---

### Task 9: Engine runtime handle

**Files:**
- Create: `crates/clicker-core/src/runtime.rs`
- Modify: `crates/clicker-core/src/lib.rs`
- Test: inline `#[cfg(all(test, windows))]` module in `runtime.rs`

**Interfaces:**
- Consumes: `SharedState`, `Engine`, `QpcClock`, `HybridWaiter`, `SendInputSink`, `ThreadPriorityGuard`, `pin_current_thread_to_best`, `PanicHotkey`.
- Produces: `EngineHandle::start(shared: Arc<SharedState>) -> Result<EngineHandle, HotkeyError>`, `EngineHandle::pinned_core(&self) -> Option<u32>`, and `Drop` that requests shutdown and joins.

This assembles the production engine: pinned, `TIME_CRITICAL`, with the kill switch registered *before* the first click can be emitted.

- [ ] **Step 1: Write the failing test**

Create `crates/clicker-core/src/runtime.rs` with only this test module:

```rust
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::shared::SharedState;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn starts_idle_and_shuts_down_cleanly() {
        let shared = Arc::new(SharedState::new());
        let h = EngineHandle::start(shared.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(shared.clicks_emitted(), 0, "an idle engine must emit nothing");
        drop(h);
        assert!(shared.shutdown());
    }

    /// Emits real clicks. Ignored by default so it never fires during an
    /// ordinary `cargo test` run and starts clicking the developer's desktop.
    #[test]
    #[ignore = "moves the real mouse; run explicitly"]
    fn emits_a_bounded_burst_and_stops_at_the_limit() {
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p clicker-core runtime`
Expected: FAIL — `cannot find type EngineHandle in this scope`.

- [ ] **Step 3: Implement the runtime handle**

Prepend to `crates/clicker-core/src/runtime.rs`:

```rust
use crate::affinity::{pin_current_thread_to_best, ThreadPriorityGuard};
use crate::clock::QpcClock;
use crate::engine::Engine;
use crate::hotkey::{HotkeyError, PanicHotkey, DEFAULT_PANIC_VK};
use crate::shared::SharedState;
use crate::sink_win32::SendInputSink;
use crate::wait::HybridWaiter;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Owns the engine thread and the emergency-stop hotkey.
#[derive(Debug)]
pub struct EngineHandle {
    shared: Arc<SharedState>,
    thread: Option<std::thread::JoinHandle<()>>,
    pinned_core: Arc<AtomicU32>,
    _hotkey: PanicHotkey,
}

const NOT_PINNED: u32 = u32::MAX;

impl EngineHandle {
    pub fn start(shared: Arc<SharedState>) -> Result<Self, HotkeyError> {
        // The kill switch is registered BEFORE the engine thread exists, so
        // there is never a window in which clicks can be emitted with no way
        // to stop them.
        let hotkey = PanicHotkey::register(shared.clone(), DEFAULT_PANIC_VK)?;

        let pinned_core = Arc::new(AtomicU32::new(NOT_PINNED));
        let pinned_for_thread = pinned_core.clone();
        let shared_for_thread = shared.clone();

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
                let sink = SendInputSink::new();
                let mut engine = Engine::new(clock, waiter, sink);

                engine.run(&shared_for_thread);
            })
            .expect("failed to spawn engine thread");

        Ok(Self { shared, thread: Some(thread), pinned_core, _hotkey: hotkey })
    }

    /// The cpu set the engine thread was pinned to, if pinning succeeded.
    pub fn pinned_core(&self) -> Option<u32> {
        match self.pinned_core.load(Ordering::Relaxed) {
            NOT_PINNED => None,
            c => Some(c),
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.shared.set_running(false);
        self.shared.request_shutdown();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
```

- [ ] **Step 4: Wire the module in**

In `crates/clicker-core/src/lib.rs`, add:

```rust
#[cfg(windows)]
pub mod runtime;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p clicker-core runtime`
Expected: PASS, 1 test run and 1 ignored.

- [ ] **Step 6: Verify the emergency stop under saturation — manual, required**

The brief requires this be tested deliberately rather than assumed.

Run: `cargo test -p clicker-core --release runtime -- --ignored --nocapture`

Then separately, with a scratch text editor focused, run the unthrottled soak below and press **F8** while it saturates a core. Confirm clicking stops within a few milliseconds and the machine stays usable.

Create `crates/clicker-core/examples/soak.rs`:

```rust
//! Unthrottled soak for verifying the emergency stop under saturation.
//! Focus a scratch text editor before running, then press F8 to stop.
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
    let h = EngineHandle::start(shared.clone()).unwrap();
    println!("pinned to core {:?}; press F8 to stop", h.pinned_core());

    shared.set_running(true);
    let start = Instant::now();
    while shared.running() && start.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(200));
        println!("clicks: {}", shared.clicks_emitted());
    }
    println!(
        "stopped after {:?} with {} clicks, state {:?}",
        start.elapsed(),
        shared.clicks_emitted(),
        shared.engine_state()
    );
}

#[cfg(not(windows))]
fn main() {
    eprintln!("windows only");
}
```

Run: `cargo run -p clicker-core --release --example soak`
Expected: click count climbs rapidly; pressing F8 stops it promptly and the process reports `state Idle`. The 15-second time limit is a backstop, not the expected exit.

- [ ] **Step 7: Verify the UIPI-blocked path produces a clear message — manual, required**

Start an elevated application (e.g. an administrator Command Prompt) and give it focus. With `soak` running unelevated in follow-cursor mode, move the cursor over the elevated window.

Expected: `SendInput` inserts fewer events than submitted, the sink returns `SinkError::Blocked`, and the engine reports `state Error` rather than silently emitting nothing. Confirm the `Display` text names elevation as the likely cause. A silent no-op here is a defect — the whole point of checking `SendInput`'s return value is to make this case legible.

To exercise the fallback timer path, temporarily force `HybridWaiter::new` to take the `timer = None` branch (comment out the `CreateWaitableTimerExW` result and substitute `None`), run `soak`, and confirm `TimerResolutionGuard` is constructed and dropped without leaving the system timer resolution raised. Revert the edit afterwards — do not commit it.

- [ ] **Step 8: Commit**

```bash
git add crates/clicker-core
git commit -m "feat(core): engine runtime handle with pinning, priority, and kill switch"
```

---

### Task 10: Statistics for the bench

**Files:**
- Create: `bench/Cargo.toml`, `bench/src/stats.rs`
- Test: inline `#[cfg(test)]` module in `stats.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `Summary { count: usize, mean_ns: f64, p50_ns: u64, p99_ns: u64, max_ns: u64 }` and `summarize(intervals: &mut [u64]) -> Summary`.

The tail matters far more than the mean, so p99 and max are first-class outputs, not afterthoughts.

- [ ] **Step 1: Write the failing tests**

Create `bench/src/stats.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_a_uniform_series() {
        let mut v = vec![1000u64; 100];
        let s = summarize(&mut v);
        assert_eq!(s.count, 100);
        assert_eq!(s.p50_ns, 1000);
        assert_eq!(s.p99_ns, 1000);
        assert_eq!(s.max_ns, 1000);
        assert!((s.mean_ns - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn exposes_the_tail() {
        // 99 fast samples and one 100x outlier: the mean hides it, p99 and max
        // must not.
        let mut v = vec![1000u64; 99];
        v.push(100_000);
        let s = summarize(&mut v);
        assert_eq!(s.max_ns, 100_000);
        assert_eq!(s.p50_ns, 1000);
        assert!(s.mean_ns < 2000.0, "mean alone would hide the outlier");
    }

    #[test]
    fn empty_input_is_all_zero() {
        let s = summarize(&mut []);
        assert_eq!(s.count, 0);
        assert_eq!(s.p50_ns, 0);
        assert_eq!(s.max_ns, 0);
    }

    #[test]
    fn percentiles_are_order_independent() {
        let mut a = vec![5u64, 1, 4, 2, 3];
        let mut b = vec![1u64, 2, 3, 4, 5];
        assert_eq!(summarize(&mut a).p50_ns, summarize(&mut b).p50_ns);
    }
}
```

- [ ] **Step 2: Create the bench manifest**

`bench/Cargo.toml`:

```toml
[package]
name = "bench"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true

[dependencies]
clicker-core = { path = "../crates/clicker-core" }

[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_UI_WindowsAndMessaging",
    "Win32_System_LibraryLoader",
    "Win32_System_Performance",
    "Win32_Graphics_Gdi",
] }
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p bench`
Expected: FAIL — `cannot find function summarize in this scope`.

- [ ] **Step 4: Implement the statistics**

Prepend to `bench/src/stats.rs`:

```rust
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub mean_ns: f64,
    pub p50_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
}

/// Sorts `intervals` in place. The tail matters far more than the mean, so
/// p99 and max are reported alongside it and never collapsed into it.
pub fn summarize(intervals: &mut [u64]) -> Summary {
    if intervals.is_empty() {
        return Summary::default();
    }
    intervals.sort_unstable();
    let n = intervals.len();
    let sum: u128 = intervals.iter().map(|v| *v as u128).sum();

    let idx = |q: f64| -> usize {
        let i = (q * (n - 1) as f64).round() as usize;
        i.min(n - 1)
    };

    Summary {
        count: n,
        mean_ns: sum as f64 / n as f64,
        p50_ns: intervals[idx(0.50)],
        p99_ns: intervals[idx(0.99)],
        max_ns: intervals[n - 1],
    }
}
```

- [ ] **Step 5: Add the binary stub so the crate compiles**

Create `bench/src/main.rs`:

```rust
mod stats;

fn main() {
    println!("bench harness — see Task 11");
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p bench`
Expected: PASS, 4 tests.

- [ ] **Step 7: Commit**

```bash
git add bench Cargo.toml
git commit -m "feat(bench): percentile and jitter statistics"
```

---

### Task 11: Receiver window and measurement sweep

**Files:**
- Modify: `bench/src/main.rs`
- Create: `bench/results/.gitkeep`

**Interfaces:**
- Consumes: `stats::{summarize, Summary}`, `clicker-core`'s `SharedState`, `EngineHandle`, `QpcClock`, `Clock`.
- Produces: the measured numbers that gate Spec 2. No downstream code depends on this module's types.

A topmost receiver window counts genuinely delivered `WM_LBUTTONDOWN` and timestamps each with QPC. Delivered is expected to fall well below emitted at high rates — that gap is the honest finding, not a bug.

- [ ] **Step 1: Implement the harness**

Replace `bench/src/main.rs` entirely:

```rust
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
        SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST,
        MSG, PM_REMOVE, SWP_NOACTIVATE, SW_SHOW, WM_LBUTTONDOWN, WNDCLASSEXW,
        WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW,
    };

    /// Timestamps of delivered clicks. Pre-allocated so the wndproc never grows it.
    static DELIVERED: OnceLock<Mutex<Vec<u64>>> = OnceLock::new();
    static DELIVERED_COUNT: AtomicUsize = AtomicUsize::new(0);
    static CLOCK: OnceLock<QpcClock> = OnceLock::new();

    fn delivered() -> &'static Mutex<Vec<u64>> {
        DELIVERED.get_or_init(|| Mutex::new(Vec::with_capacity(4_000_000)))
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        w: WPARAM,
        l: LPARAM,
    ) -> LRESULT {
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
            let hinst = GetModuleHandleW(None).unwrap();
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
                200,
                200,
                600,
                400,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .expect("failed to create receiver window");

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 200, 200, 600, 400, SWP_NOACTIVATE);

            (hwnd, (200 + 300, 200 + 200)) // centre of the window
        }
    }

    /// Drain the message queue. This is the receiver's own consumption rate —
    /// the very bound that makes delivered < emitted.
    fn pump() {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid writable MSG for each call.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
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
        let mut intervals: Vec<u64> =
            times.windows(2).map(|w| w[1].saturating_sub(w[0])).collect();
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
        println!("engine pinned to core {:?}\n", handle.pinned_core());

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
            rows.push(run_cell(&shared, interval, 1, label, secs));
        }

        // Unthrottled sweep across batch sizes. Whether delivered CPS actually
        // rises with K is the empirical question this harness exists to answer.
        for batch in [1u16, 2, 4, 8, 16] {
            rows.push(run_cell(&shared, 0, batch, "unthrottled", secs));
        }

        println!("\n| Target | Batch K | Emitted CPS | Delivered CPS | Ratio | mean us | p50 us | p99 us | max us |");
        println!("|---|---|---|---|---|---|---|---|---|");
        for r in &rows {
            let ratio = if r.emitted > 0 {
                r.delivered as f64 / r.emitted as f64
            } else {
                0.0
            };
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
```

- [ ] **Step 2: Create the results directory**

```bash
mkdir -p bench/results && touch bench/results/.gitkeep
```

- [ ] **Step 3: Build the harness**

Run: `cargo build -p bench --release`
Expected: compiles with no errors.

- [ ] **Step 4: Run the measurement and capture the output**

This moves the real mouse and clicks a real window for roughly a minute. Close other applications first, and keep F8 free.

Run: `cargo run -p bench --release > bench/results/2026-07-21-baseline.md 2>&1`

Expected: `bench/results/2026-07-21-baseline.md` contains the pinned core, the full sweep table, and the closing note. Every row must have a non-zero delivered count — an all-zero column means the receiver never had focus, which invalidates the run.

- [ ] **Step 5: Verify the results are real before trusting them**

Read `bench/results/2026-07-21-baseline.md` and confirm:
- Emitted CPS tracks the target within a few percent at 50–1000 CPS.
- Delivered CPS diverges from emitted as the rate climbs.
- p99 exceeds p50 (a distribution with zero spread means timestamps were not captured).

If emitted CPS falls far short of target at low rates, the wait tiers are wrong — fix that before recording numbers.

- [ ] **Step 6: Write the findings summary**

Create `bench/results/README.md` with the measured table pasted in, plus a short section stating: the measured ceiling, the batch size at which delivered CPS stopped improving, and the recommended UI maximum for Spec 3. Write actual numbers from your run — this file is the Spec 2 gate artifact and the source for the Spec 4 README.

- [ ] **Step 7: Commit**

```bash
git add bench
git commit -m "feat(bench): receiver window and measurement sweep with recorded baseline"
```

---

## Gate

Spec 1 is complete when `bench/results/README.md` contains measured emitted vs. delivered CPS and the jitter distribution. **Spec 2 does not begin before that.** The recommended UI maximum recorded there — not a guess — sets the slider range in Spec 3.
