# Build Prompt: Maximum-Throughput Windows Auto Clicker with Neumorphic Direct2D GUI

> Self-directed brief. Read fully before writing code. Follow the phase order — the click
> engine is proven with measurements before any pixel of GUI is drawn.

---

## 0. Mission

Build a Windows auto clicker that reaches the genuine **user-mode throughput ceiling**, wrapped
in a hand-rendered **neumorphic** interface built on Direct2D. Two hard requirements that pull in
opposite directions and must not be allowed to compromise each other:

1. The click engine's timing must be unaffected by the GUI. Ever.
2. The GUI must look deliberately designed, not like a hobby Win32 dialog.

The resolution is strict separation: the engine is a pinned, high-priority native thread that
never allocates, never locks, and never knows the GUI exists. The GUI is an event-driven,
render-on-demand surface that communicates with the engine exclusively through atomics.

**Success is measured, not claimed.** "Fastest possible" is a falsifiable statement. Phase 2
exists to falsify it.

### Explicitly out of scope

- Kernel-mode drivers, filter drivers, or HID emulation via unsigned drivers.
- Anti-cheat evasion, injection-signature masking, or timing humanization intended to defeat
  detection. (Humanization was considered and cut from v1.)
- Anything requiring the user to disable Driver Signature Enforcement or Secure Boot.

If a design idea's primary value is "harder to detect," it does not belong in this project.
The performance goal is throughput, not concealment.

---

## 1. Stack & Prerequisites

- **Language:** Rust 1.96+, `x86_64-pc-windows-msvc`.
- **Win32 bindings:** the `windows` crate. Enable only the feature groups actually needed —
  `Win32_Graphics_Direct2D`, `Win32_Graphics_Direct3D11`, `Win32_Graphics_DirectWrite`,
  `Win32_Graphics_DirectComposition`, `Win32_Graphics_Dxgi`, `Win32_UI_WindowsAndMessaging`,
  `Win32_UI_Input_KeyboardAndMouse`, `Win32_System_Threading`, `Win32_Media`. Feature bloat here
  costs real compile time.
- **Serialization:** `serde` + `serde_json` for profiles. Nothing else.
- **No GUI framework.** No egui, no Tauri, no winit. Raw `CreateWindowExW` and a hand-written
  window procedure.

### Step 0 — verify the linker before anything else

`rustc` is present with an MSVC host triple, but a probe for `link.exe` found only Git's
unrelated binary. **Confirm the MSVC toolchain links a hello-world before writing a line of real
code.** Check for VS Build Tools under `C:\Program Files (x86)\Microsoft Visual Studio\` and
`C:\Program Files\Microsoft Visual Studio\`, or a standalone Windows SDK. If linking fails, stop
and report it — do not silently switch to the `gnu` toolchain, which would change the Direct2D
binding story.

### Workspace layout

```
Auto Clicker/
├─ Cargo.toml                  # workspace
├─ crates/
│  ├─ clicker-core/            # engine, timing, config. ZERO GUI dependencies.
│  │  ├─ src/engine.rs         # the hot loop
│  │  ├─ src/schedule.rs       # pure timing math — heavily unit tested
│  │  ├─ src/sink.rs           # ClickSink trait + SendInput impl + test recorder
│  │  ├─ src/shared.rs         # the atomic control block
│  │  └─ src/profile.rs        # serde types, load/save
│  └─ clicker-gui/             # Win32 + Direct2D. Depends on core; core never depends on it.
│     ├─ src/main.rs
│     ├─ src/window.rs         # wndproc, DPI, message pump
│     ├─ src/render/           # D2D device, swapchain, neumorphic primitives
│     ├─ src/widgets/          # button, toggle, slider, numeric field
│     └─ src/hotkey.rs
└─ bench/                      # Phase 2 measurement harness
```

`clicker-core` must build and its tests must pass on a non-Windows target where possible, or at
minimum without constructing a window. That constraint is what keeps the engine testable.

---

## 2. Phase 1 — The Click Engine

Build this first. It is the product; the GUI is a control surface for it.

### 2.1 The control block

A single `SharedState` struct of atomics, allocated once, shared by `Arc` between GUI and engine.
No `Mutex`, no channel, no allocation touches the hot loop.

```rust
pub struct SharedState {
    running:        AtomicBool,   // engine actively clicking
    shutdown:       AtomicBool,   // terminate the thread
    interval_ns:    AtomicU64,    // target period between clicks
    button:         AtomicU8,     // Left / Middle / Right
    position_mode:  AtomicU8,     // FollowCursor / FixedPoint
    fixed_x:        AtomicI32,
    fixed_y:        AtomicI32,
    limit_clicks:   AtomicU64,    // 0 = unlimited
    limit_ns:       AtomicU64,    // 0 = unlimited
    clicks_emitted: AtomicU64,    // engine → GUI, monotonic
    engine_state:   AtomicU8,     // Idle / Running / StoppedByLimit / Error
}
```

Engine reads config with `Ordering::Relaxed` at the top of each iteration — these are hints that
may be one click stale, which is fine and cheaper than acquiring. Use `Acquire`/`Release` only on
`running`, `shutdown`, and `engine_state`, where ordering carries meaning.

### 2.2 Timing — the actual hard part

Naive `thread::sleep` is useless here: the default Windows timer tick is ~15.6 ms, which caps you
near 64 CPS. Use a **three-tier hybrid wait**, choosing the tier by remaining time to deadline:

| Remaining | Mechanism | Why |
|---|---|---|
| > 2 ms | `CreateWaitableTimerExW` with `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` | ~0.5 ms granularity, Win10 1803+, no system-wide penalty. Preferred over `timeBeginPeriod`. |
| 0.05–2 ms | `Sleep(0)` / `SwitchToThread()` yield loop, re-checking QPC | Cheap yields without burning a full core |
| < 0.05 ms | Busy spin on QPC with `std::hint::spin_loop()` | Only path to sub-millisecond precision. `spin_loop` emits `PAUSE` — meaningfully better for SMT siblings and power than a bare loop. |

Clock is `QueryPerformanceCounter` / `QueryPerformanceFrequency`. **Cache the frequency once** —
it is fixed for the boot session; querying it in the loop is a wasted call.

Schedule against an **absolute deadline that advances by `interval_ns`**, never
`now + interval`. The latter accumulates drift proportional to per-iteration overhead. If the
engine falls behind by more than a few periods (scheduler preemption, laptop suspend), **snap the
deadline forward to now** rather than firing a burst of catch-up clicks — a burst is never what
the user wanted.

If the high-resolution waitable timer is unavailable, fall back to `timeBeginPeriod(1)` and
**pair it with `timeEndPeriod(1)` on every exit path, including panic**. Leaking a raised global
timer resolution measurably degrades system battery life. Use a guard struct with a `Drop` impl —
do not rely on reaching the end of `main`.

### 2.3 Thread configuration

- `SetThreadPriority(THREAD_PRIORITY_TIME_CRITICAL)` on the engine thread.
- **Do not** set `REALTIME_PRIORITY_CLASS` on the process. It can starve the input stack itself,
  producing a machine the user cannot regain control of — precisely the worst failure mode for
  this application. `HIGH_PRIORITY_CLASS` is the ceiling, and it should be an opt-in toggle with
  the tradeoff stated in the UI, not the default.
- Pin the thread to one core to avoid migration and cold caches. On hybrid CPUs (Intel 12th gen+),
  **select a P-core**: enumerate with `GetSystemCpuSetInformation` and pin via
  `SetThreadSelectedCpuSets`, preferring cpu sets with the highest `EfficiencyClass`. Landing the
  hot loop on an E-core is a large, silent, and easily-missed regression.
- Pre-allocate everything before the loop. No `Vec` growth, no formatting, no logging inside it.

### 2.4 Emitting clicks

Define a trait so the engine is testable without moving a real cursor:

```rust
pub trait ClickSink {
    fn emit(&mut self, button: Button, pos: Option<(i32, i32)>) -> Result<(), SinkError>;
}
```

Production impl uses `SendInput`. Critical details:

- Build the `[INPUT; 2]` array (down + up) **once**, outside the loop; mutate only the flags/coords
  fields per click. `SendInput` accepts both events in a single call — **one syscall per click,
  not two.** This is the single largest throughput win available.
- Fixed-position mode: `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE`, coordinates normalized to
  0..65535. Include `MOUSEEVENTF_VIRTUALDESK` so multi-monitor setups map correctly — omitting it
  is a classic bug that confines clicks to the primary display.
- Follow-cursor mode: omit the move entirely. Do not call `GetCursorPos` per click; the click
  lands wherever the cursor already is, and the extra syscall is pure loss.
- `SendInput` returns the number of events inserted. **Check it.** A short return means the input
  was blocked — most often UIPI refusing to inject into a higher-integrity target, or a locked
  workstation / secure desktop. Surface this as a distinct engine error state, not a silent no-op.
- On UIPI blockage, tell the user elevation is required and let them choose. Do not force an
  admin manifest; a tool that unconditionally demands elevation is worse than one that explains
  why it might need it.

### 2.5 Limits and the kill switch

Check `limit_clicks` and `limit_ns` against the counter and elapsed time each iteration; on trip,
clear `running` and set `engine_state = StoppedByLimit`.

**A global emergency stop is a safety requirement, not a feature.** At several thousand CPS the
machine becomes difficult to operate, so stopping must not depend on the GUI being responsive:

- Register a dedicated panic hotkey (default `F8`) via `RegisterHotKey`, separate from the normal
  toggle.
- The hotkey handler must clear `running` directly on the shared atomic — no queue, no channel, no
  round-trip through render or layout code.
- Verify the stop path works while the engine is saturating a core. Test this deliberately.

---

## 3. Phase 2 — Prove the Speed (do not skip)

Build `bench/` before the GUI. Without it, every performance claim in this project is unfounded.

A receiver window with a minimal message pump counts genuinely delivered `WM_LBUTTONDOWN`
messages and timestamps each with QPC. Report:

- **Emitted CPS** — what the engine attempted.
- **Delivered CPS** — what a real message pump actually consumed.
- **Interval distribution** — mean, p50, p99, max jitter. The tail matters far more than the mean.

Expect delivered to fall well below emitted at high rates. That gap is the honest finding, not a
bug: `SendInput` serializes through the system's raw input thread, and the receiving application
consumes on its own message loop, typically bounded by its frame rate. **Document the measured
numbers in the README, including the ceiling.** A tool that states its real limit is more useful
than one that advertises an unreachable number.

Use these results to set a sane maximum in the UI rather than exposing a meaningless
million-CPS slider.

---

## 4. Phase 3 — The Neumorphic GUI

### 4.1 Design constraints

Neumorphism is defined by one rule: **surface and background share the same base color, and all
depth comes from paired light and dark shadows.** Break that rule and it stops being neumorphism.

- Base: a single mid-tone. Light `#E0E5EC` / dark `#2E3239`. Elements are that exact color.
- Light source fixed at **top-left**. Every raised element casts a light highlight up-left and a
  dark shadow down-right. Pressed/inset elements invert both. Consistency across every widget is
  what sells the effect — one inconsistent element breaks the whole illusion.
- Corner radii large and uniform (14–20 px at 100% DPI). Small radii read as flat.
- A subtle diagonal linear gradient across raised surfaces (a few percent lighter toward the light
  source) is what separates crisp neumorphism from a blurry gray blob. Include it.
- **Confront the accessibility weakness directly.** Same-color-on-same-color means near-zero
  contrast for interactive affordances. Non-negotiable mitigations: text and icons must clear
  WCAG AA (4.5:1) against the base — so text is markedly darker/lighter than the surface, not
  tinted; every focusable control gets a visible accent-colored focus ring; and the single accent
  color (used for the active/running state) must be distinguishable independent of the shadow
  treatment. Do not let aesthetics win this argument.

### 4.2 Rendering pipeline

`ID3D11Device` → `IDXGIDevice` → `ID2D1Device` → `ID2D1DeviceContext`, presented through a
**DirectComposition** visual tree with a flip-model DXGI swapchain
(`DXGI_SWAP_EFFECT_FLIP_DISCARD`, `DXGI_ALPHA_MODE_PREMULTIPLIED`). DirectComposition gives real
per-pixel transparency, which a borderless rounded neumorphic window needs.

- **Per-Monitor DPI v2**: `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`, handle
  `WM_DPICHANGED` by resizing to the suggested rect and rebuilding DPI-dependent resources. Test on
  a 150% display — DPI bugs are invisible at 100% and glaring everywhere else.
- **Render on demand only.** Paint in response to `WM_PAINT` and explicit invalidation. A GUI
  spinning at display refresh permanently steals CPU from the engine for no benefit. The only
  recurring work is a ~100 ms timer sampling the click counter for the live CPS readout, and it
  should invalidate just the readout's rect.
- Handle device-lost (`DXGI_ERROR_DEVICE_REMOVED` / `D2DERR_RECREATE_TARGET`) by tearing down and
  rebuilding the device stack. Driver updates and GPU resets trigger this in normal use; skipping
  it produces a permanently black window that looks like a hang.

### 4.3 Constructing the shadows

Direct2D has no neumorphism primitive. Build both directions from effects:

**Raised (outer dual shadow):**
1. Render the rounded-rect silhouette to an intermediate `ID2D1Bitmap1`.
2. Two `CLSID_D2D1Shadow` effects — one dark, offset `(+dx, +dy)`; one light/white, offset
   `(-dx, -dy)`. Blur radius roughly equal to the offset magnitude.
3. Composite both **beneath** the filled surface, then draw the surface with its gradient on top.

**Inset (pressed) — no built-in effect exists; construct it:**
1. Take the shape's alpha mask and **invert** it, so the region outside the shape is opaque.
2. Gaussian-blur the inverted mask (`CLSID_D2D1GaussianBlur`).
3. Composite the blurred result back with `D2D1_COMPOSITE_MODE_SOURCE_IN` against the original
   shape mask, clipping the blur to the shape's interior.
4. Do this twice — dark tint offset down-right, light tint offset up-left — for the inverted pair.

This is the single most intricate piece of rendering in the project. **Isolate it as a
`neumorph::surface(ctx, rect, radius, Elevation)` primitive with `Raised | Inset | Flat`, and
build every widget on top of it.** If shadow construction is duplicated across widgets, the design
will drift and the code becomes unmaintainable.

### 4.4 Cache aggressively

Gaussian shadow effects are expensive and the inputs rarely change. Cache each widget's rendered
appearance per `(size, dpi, state)` into an `ID2D1Bitmap1`; on a normal repaint, blit cached
bitmaps. Re-render only on resize, DPI change, or state transition. Hover and press transitions
can cross-fade between two cached bitmaps — cheap, and it reads as polish.

Text via **DirectWrite** with a cached `IDWriteTextFormat` per style. Use `Segoe UI Variable` if
available, falling back to `Segoe UI`. Enable greyscale antialiasing rather than ClearType on the
translucent composited surface — subpixel AA against transparency produces color fringing.

### 4.5 Widgets required

Hand-build each on the shared surface primitive: neumorphic **push button** (raised → inset on
press), **toggle switch** (inset track, raised knob), **slider** (inset groove, raised thumb),
**numeric entry** (inset well), **hotkey capture field**, **profile dropdown**, and a **live CPS
readout** (inset well, large DirectWrite numerals).

Each needs a hit-test rect, a state machine (`Idle / Hover / Pressed / Focused / Disabled`), and
keyboard reachability via Tab. Keyboard navigation is not optional — with neumorphism's low
contrast, the focus ring is doing real accessibility work.

### 4.6 Window shell

Borderless with a custom title bar: rounded corners, drag via `WM_NCHITTEST` returning `HTCAPTION`
over the header, and custom minimize/close. Snap Layouts and system window-management gestures
should keep working — verify rather than assuming.

---

## 5. Phase 4 — Profiles, Hotkeys, Persistence

**Hotkeys.** Toggle mode uses `RegisterHotKey` — no hook, no system-wide cost. Hold-to-click needs
raw input: prefer `RegisterRawInputDevices` with `RIDEV_INPUTSINK` over a `WH_MOUSE_LL` /
`WH_KEYBOARD_LL` hook. A low-level hook adds latency to *every* input event system-wide and gets
silently unhooked if the callback exceeds `LowLevelHooksTimeout` — a poor trade for this feature.

If a low-level hook proves unavoidable, it must live on the GUI thread with a live message pump,
must never block, and **must discard events flagged `LLMHF_INJECTED`.** Without that check the
clicker's own synthetic clicks re-enter the hook and retrigger it — an unbounded feedback loop
that is genuinely unpleasant to debug at 3000 CPS.

**Profiles.** `serde_json` at `%APPDATA%\AutoClicker\profiles.json`. Write atomically: temp file
in the same directory, then `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`. Include a schema
version field from day one. A corrupt or unreadable profile file must fall back to defaults with a
non-fatal notice — never a startup crash.

---

## 6. Testing

**`clicker-core` — the substance of the test suite.**
- `schedule.rs` is written as **pure functions** (`next_deadline(now, last, interval) -> Decision`)
  precisely so timing logic is testable without sleeping. Cover: drift-free advancement, snap-forward
  after a long stall, limit tripping at exact boundaries, interval changes mid-run.
- Engine tests run against a recording `ClickSink` that timestamps calls. Assert ordering, counts,
  and limit enforcement.
- Wall-clock assertions must use generous tolerances or be marked `#[ignore]` for local-only runs.
  A test asserting sub-millisecond precision on a shared CI runner will flake, and a flaky test
  gets muted, which is worse than no test.

**GUI.** Render widgets to a WIC bitmap off-screen and compare against golden PNGs with a small
per-pixel tolerance. This catches shadow-construction regressions — exactly the failure mode most
likely to slip through review, since a subtly wrong blur radius still "looks fine" in isolation.

**Manual checklist:** 100%/150%/200% DPI; multi-monitor with mixed DPI; light and dark base;
emergency stop while the engine saturates a core; UIPI-blocked target produces a clear message;
profile file deleted / corrupted / from a future schema version.

---

## 7. Order of Work

1. Verify MSVC linker. Scaffold workspace.
2. `clicker-core`: shared state, schedule math + tests, `ClickSink` trait, `SendInput` impl.
3. Engine thread: priority, P-core pinning, hybrid wait, limits.
4. **`bench/` harness. Measure. Record real numbers.** Gate: do not proceed until emitted vs.
   delivered CPS and the jitter distribution are known and written down.
5. Win32 window + D3D11/D2D/DirectComposition stack + DPI handling. Clear to a flat base color.
6. `neumorph::surface` primitive — raised and inset. Get this exactly right in isolation before
   building anything on it.
7. Widgets on the primitive. Caching layer.
8. Wire GUI to engine through the atomics. Live CPS readout.
9. Hotkeys (toggle, hold, emergency stop). Profiles.
10. README with **measured** performance numbers, stated ceiling, and the explanation of why
    delivered < emitted.

---

## 8. Standing Rules

- The hot loop allocates nothing, locks nothing, and logs nothing.
- No `unsafe` block without a comment stating the invariant that makes it sound. Win32 interop
  means a lot of `unsafe`; undocumented `unsafe` is where the real bugs will live.
- Every raw handle and every `timeBeginPeriod` gets an RAII guard.
- No performance claim without a measurement backing it.
- If the GUI ever measurably perturbs engine timing, the architecture is wrong — fix the coupling
  rather than tuning around it.
