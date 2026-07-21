# Spec 1 — Click Engine & Measurement Harness

**Date:** 2026-07-21
**Scope:** `clicker-core` + engine thread + `bench/`. Ends at the Phase 2 measurement gate.
**Source brief:** `BUILD_PROMPT.md` §2, §3, work items 1–4.

## 1. Context and decomposition

`BUILD_PROMPT.md` describes five subsystems. It is decomposed into four specs along the
brief's own phase gates:

| Spec | Content | Gate |
|---|---|---|
| **1 (this doc)** | `clicker-core`, engine thread, `bench/` | Measured emitted vs. delivered CPS + jitter distribution written down |
| 2 | Win32 window, D3D11/D2D/DirectComposition, DPI, `neumorph::surface` | Raised + inset primitive correct in isolation, golden-image tested |
| 3 | Widgets, bitmap caching, GUI↔engine wiring | GUI does not measurably perturb engine timing |
| 4 | Hotkeys, profiles, README with measured numbers | — |

### Environment (verified 2026-07-21)

- Rust 1.96.0, host `x86_64-pc-windows-msvc`. Target `x86_64-unknown-linux-gnu` also installed.
- **MSVC linker verified**: VS Build Tools 2026 at
  `C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools`, Windows SDK 10.0.26100.0.
  A hello-world compiled, linked, and ran. No `gnu` fallback needed.
- Dev CPU: **AMD Ryzen 5 7600, 6C/12T, homogeneous.** No P/E split, so every cpu set reports
  an identical `EfficiencyClass`. The hybrid-selection branch of §5.3 ships **untested on this
  machine** — this is a known, accepted gap, recorded here so it is not mistaken for
  verified behaviour.

## 2. Decisions taken during brainstorming

1. **Four phase-gated specs** rather than one monolith.
2. **P-core pinning written properly with graceful degradation** — implement
   `GetSystemCpuSetInformation` + `SetThreadSelectedCpuSets`; when all cpu sets report the same
   `EfficiencyClass`, fall back to pinning a single stable core.
3. **Kill switch lives in `clicker-core`**, on its own message-only window thread, so the
   emergency stop exists from Spec 1 onward and does not depend on a GUI that does not yet exist.
4. **`interval_ns == 0` is an explicit unthrottled mode**, not a degenerate case of the deadline
   math. Keeps the ceiling path and the stall-recovery path textually separate.
5. **Engine is generic over `Clock` + `Waiter` + `ClickSink`**, making the entire engine
   deterministically testable in virtual time. This retires the brief's own admission (§6) that
   wall-clock engine tests flake and get muted.

### Tension resolved: core purity vs. owning the kill switch

Decision 3 collides with the brief's requirement that `clicker-core` build and test on a
non-Windows target. Resolved by `cfg`-gating: the timing brain (`shared`, `schedule`, `clock`,
`wait`, `sink` trait, `engine`) is platform-neutral and its tests run under
`--target x86_64-unknown-linux-gnu`; only `sink_win32`, `hotkey`, and `affinity` are
`#[cfg(windows)]`.

## 3. Module layout

```
crates/clicker-core/src/
├─ lib.rs
├─ shared.rs        any    SharedState atomics, Button/PositionMode/EngineState
├─ schedule.rs      any    pure decide() — no I/O, no clock
├─ clock.rs         any    Clock trait; QpcClock (win), VirtualClock (test)
├─ wait.rs          any    Waiter trait; HybridWaiter (win), InstantWaiter (test)
├─ sink.rs          any    ClickSink trait, SinkError, RecordingSink
├─ engine.rs        any    Engine<C: Clock, W: Waiter, S: ClickSink>
├─ sink_win32.rs    win    SendInput impl, batched
├─ hotkey.rs        win    message-only window thread owning the panic hotkey
├─ affinity.rs      win    thread priority + cpu-set pinning, RAII guards
└─ runtime.rs       win    EngineHandle — assembles and owns the engine thread
bench/src/stats.rs          percentile / jitter math (unit tested)
bench/src/main.rs           receiver window + sweep harness
```

`profile.rs` is **deferred to Spec 4**. Profiles are Phase 4 work; nothing in Spec 1 reads them.

## 4. Control block

Per brief §2.1, with two additions.

**`snapshot()`** performs one `Relaxed` read of every config field into a plain `Config` struct at
the top of each iteration. The loop body then reasons about a single consistent snapshot rather
than re-reading fields that may change between reads.

**Per-run vs. monotonic counters.** `clicks_emitted` is documented monotonic (the GUI reads it for
the lifetime of the process), but `limit_clicks` is naturally per-run. The engine therefore keeps
local `run_start_clicks` and `run_start_ns`, captured on the idle→running edge, and evaluates
limits against the delta. Without this, the second run of a session trips its click limit
immediately.

**Counter writes use `store`, not `fetch_add`.** The engine is the sole writer and the GUI is a
pure reader, so a locked read-modify-write per click is unnecessary. The engine keeps a local
`u64` and issues a `Relaxed` store. This matters at ceiling rates.

Ordering: `Acquire`/`Release` on `running`, `shutdown`, `engine_state`. `Relaxed` everywhere else.

## 5. Engine

### 5.1 `schedule::decide()` — pure

```rust
pub enum Decision {
    Fire { next_deadline_ns: u64 },
    Wait { until_ns: u64 },
    Stop(StopReason),          // ClickLimit | TimeLimit
}
```

Limits are evaluated **before** firing, so `limit_clicks = N` emits exactly N clicks.

Three rules:

1. **Unthrottled** (`interval_ns == 0`) — check limits, then always
   `Fire { next_deadline_ns: now }`. Never enters deadline arithmetic.
2. **Normal** — `next = deadline + interval`. Absolute advancement; drift-free by construction.
   Never `now + interval`.
3. **Snap-forward** — when `now - deadline > max(interval * 4, 2ms)`, set `next = now + interval`
   and discard the backlog rather than firing catch-up clicks.

The `2ms` floor is load-bearing: at 10,000 CPS a bare `interval * 4` is 400 µs, so ordinary
scheduler noise would trip snap-forward on nearly every iteration and it would stop meaning
"recovered from a stall."

### 5.2 Loop structure

```
loop {
    if shutdown.load(Acquire) { break }
    let cfg = shared.snapshot();
    if !running.load(Acquire) { idle(); continue }   // 1ms poll; not a hot path
    // on idle→running edge: capture run_start_ns / run_start_clicks, deadline = now
    match schedule::decide(clock.now_ns(), deadline, cfg, clicks_this_run, run_start_ns) {
        Fire { next_deadline_ns } => { sink.emit(..)?; local += 1;
                                       clicks_emitted.store(local, Relaxed);
                                       deadline = next_deadline_ns }
        Wait { until_ns }         => waiter.wait_until(until_ns, &clock),
        Stop(reason)              => { running.store(false, Release);
                                       engine_state.store(StoppedByLimit, Release) }
    }
}
```

Allocates nothing, locks nothing, logs nothing. Everything is constructed before the loop.

### 5.3 Thread configuration

- `SetThreadPriority(THREAD_PRIORITY_TIME_CRITICAL)`, restored by RAII guard on exit.
- Process priority class stays default. `HIGH_PRIORITY_CLASS` is a Spec 3 opt-in toggle;
  `REALTIME_PRIORITY_CLASS` is never set — it can starve the input stack and produce a machine
  the user cannot regain control of.
- Pinning: enumerate `GetSystemCpuSetInformation`, prefer the highest `EfficiencyClass`, pin via
  `SetThreadSelectedCpuSets`. When all classes are equal (this dev machine), pin one stable
  non-zero core.

### 5.4 Wait tiers

Per brief §2.2, behind the `Waiter` trait:

| Remaining | Mechanism |
|---|---|
| > 2 ms | `CreateWaitableTimerExW` + `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` |
| 0.05–2 ms | `SwitchToThread()` / `Sleep(0)` yield loop, re-checking QPC |
| < 0.05 ms | `spin_loop()` on QPC |

`QueryPerformanceFrequency` cached once at `QpcClock` construction. If the high-resolution
waitable timer is unavailable, fall back to `timeBeginPeriod(1)` behind a `Drop` guard that
pairs `timeEndPeriod(1)` on every exit path including unwind.

## 6. Click emission

```rust
pub trait ClickSink {
    fn emit(&mut self, button: Button, pos: Option<(i32, i32)>) -> Result<(), SinkError>;
    fn emit_batch(&mut self, button: Button, pos: Option<(i32,i32)>, count: u16)
        -> Result<u16, SinkError>;
}
```

### 6.1 Batching — the throughput decision

`SendInput` accepts an array of N events, so `[INPUT; 2*K]` delivers **K clicks in one syscall**.
The brief's "one syscall per click, not two" is correct but incomplete.

> **MEASURED 2026-07-21 — this hypothesis was wrong.** This section originally claimed batching
> was "the largest remaining user-mode win." The Phase 2 sweep falsified it: batching raises
> *emitted* CPS by up to 42% while *delivered* CPS falls and the delivery ratio collapses from
> 99.4% (K=1) to 56.5% (K=16). It is a vanity metric — a bigger counter for less real work.
> `K = 1` is the correct default. See [`bench/results/README.md`](../../../bench/results/README.md).
> The text below is retained as the original design rationale; the sweep axis did its job.

- Applies **only** in unthrottled mode. Batched events carry no temporal spacing, so batching a
  throttled rate would destroy the requested interval.
- Batch size `K` is configurable and is a **sweep axis in the bench**, not an assumption. Whether
  *delivered* CPS actually rises with `K` — or whether the receiving application coalesces the
  events — is an empirical question Phase 2 exists to answer.
- The `[INPUT; 2*K_MAX]` buffer is allocated once at sink construction; per-call work mutates
  flags and coordinates only.

### 6.2 Details

- Follow-cursor is the fast path: no move event, no `GetCursorPos`. 2 events per click.
- Fixed-position: OR `MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` onto the
  down event with normalized coords — still one syscall, no extra event.
- Normalization uses **virtual-screen** metrics (`SM_XVIRTUALSCREEN`, `SM_CXVIRTUALSCREEN`, …)
  because `VIRTUALDESK` is set. Metrics cached; refreshed on display change in Spec 3.
- `SendInput` returns the count inserted. **Check it.** A short return means blocked input —
  UIPI refusing injection into a higher-integrity target, or a locked workstation / secure
  desktop. Map to `SinkError::Blocked(u32)` via `GetLastError`, set `EngineState::Error`, clear
  `running`. Never a silent no-op.
- UIPI blockage surfaces a message explaining elevation may be required. No admin manifest is
  forced.

## 7. Emergency stop

A dedicated thread in `clicker-core` creates an `HWND_MESSAGE` window, calls
`RegisterHotKey(hwnd, id, MOD_NOREPEAT, VK_F8)`, and runs a `GetMessage` loop. On `WM_HOTKEY` it
stores `running = false` with `Release` directly on the shared atomic — no queue, no channel, no
round-trip through any other subsystem.

- RAII: `UnregisterHotKey` + `DestroyWindow` on drop; thread terminated via `PostMessage(WM_QUIT)`.
- **If `RegisterHotKey` fails** (F8 already claimed by another process), this is a hard error
  surfaced to the caller — the engine must not start with no working kill switch.
- Verified deliberately while the engine saturates a core (§9 checklist).

## 8. Bench harness

A `bench/` binary, built before any GUI work.

- A topmost receiver window with a minimal message pump counts genuinely delivered
  `WM_LBUTTONDOWN` and timestamps each with QPC into a pre-allocated buffer.
- The engine drives it in fixed-point mode targeting the window's centre.
- A timestamping wrapper `ClickSink` records the emitted-side intervals in the same run.

**Sweep axes:** target rate `[50, 100, 250, 500, 1000, 2000, 5000, unthrottled]` × batch size
`K ∈ [1, 2, 4, 8, 16]` (K > 1 only meaningful unthrottled), fixed duration per cell.

**Reported per cell:** emitted CPS, delivered CPS, delivered/emitted ratio, and interval
distribution — mean, p50, p99, max jitter. **The tail matters far more than the mean.**

Output is a markdown table to stdout and to a results file feeding the Spec 4 README.

Delivered is expected to fall well below emitted at high rates. That gap is the honest finding,
not a bug: `SendInput` serializes through the system's raw input thread and the receiver consumes
on its own message loop. The measured ceiling sets the UI maximum in Spec 3 — no meaningless
million-CPS slider.

## 9. Testing

**Deterministic (the substance).** `VirtualClock` + `InstantWaiter` + `RecordingSink` make the
whole engine testable with no sleeping and no tolerance windows:

- drift-free advancement over many periods
- snap-forward after a long stall, and *absence* of snap-forward under small jitter
  (the `2ms` floor)
- click limit at exactly N-1 / N / N+1
- time limit at the boundary
- interval change mid-run
- unthrottled mode never enters deadline arithmetic
- per-run limit reset across stop→start cycles
- `SinkError::Blocked` transitions to `EngineState::Error` and clears `running`

**Cross-target.** `cargo test -p clicker-core --target x86_64-unknown-linux-gnu` must pass,
enforcing the platform-neutrality of the timing brain.

**Wall-clock.** Real `QpcClock` + `HybridWaiter` accuracy tests are `#[ignore]`d, local-only,
with generous tolerances. They inform; they do not gate.

**Manual checklist:** emergency stop while the engine saturates a core; UIPI-blocked target
(elevated window) produces a clear message rather than silence; unplugging the high-resolution
timer path exercises the `timeBeginPeriod` guard.

## 10. Standing rules (inherited)

- The hot loop allocates nothing, locks nothing, logs nothing.
- No `unsafe` block without a comment stating the invariant that makes it sound.
- Every raw handle and every `timeBeginPeriod` gets an RAII guard.
- No performance claim without a measurement backing it.

## 11. Explicitly out of scope

Kernel-mode drivers or HID emulation. Anti-cheat evasion, injection-signature masking, or timing
humanization intended to defeat detection. Anything requiring Driver Signature Enforcement or
Secure Boot to be disabled. The performance goal is **throughput, not concealment**.

## 12. Gate

Spec 1 is complete when emitted vs. delivered CPS and the jitter distribution are measured and
written down. Spec 2 does not begin before that.
