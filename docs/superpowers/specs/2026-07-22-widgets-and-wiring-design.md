# Spec 3 — Widgets, Caching, and Engine Wiring

**Date:** 2026-07-22
**Scope:** The widget framework, the widgets needed for a working clicker, DirectWrite text, the
bitmap caching layer, and wiring the GUI to `clicker-core` through the shared atomics.
**Predecessors:** [Spec 1](2026-07-21-clicker-engine-design.md) (engine, merged),
[Spec 2](2026-07-21-render-foundation-design.md) (render foundation, merged).

## 1. Gate inherited from Spec 1

The mission's hard rule: **the GUI must never measurably perturb engine timing.** Spec 1's
emit-side interval table in [`bench/results/README.md`](../../../bench/results/README.md) is the
baseline that makes this falsifiable — at 1,000 CPS the engine holds p99 at 1,002.2 µs. Spec 3's
completion gate re-runs that sweep with the GUI open and interacting and compares. Any regression
is an architecture bug, fixed at the coupling.

## 2. Scope

**In:** widget model framework, five widgets (button, toggle, slider, numeric field, CPS
readout), DirectWrite text, per-widget bitmap caching, focus/tab, and wiring to the engine.

**Out (Spec 4):** hotkey-capture field, profile dropdown, `serde_json` persistence, and the
`HIGH_PRIORITY_CLASS` opt-in toggle. The result of Spec 3 is a usable clicker driven entirely
from its own UI; Spec 4 adds persistence and global hotkeys.

## 3. Decisions

1. **Pure model + thin adapters**, mirroring the engine's `Clock`/`Waiter` split and the shell's
   `hit_test`. A widget's behaviour — state machine, hit-test math, value mapping, tab order — is
   platform-neutral and tests on `x86_64-unknown-linux-gnu` with no window. Only rendering
   (`neumorph::render_surface` + DirectWrite) and Win32 message plumbing are `#[cfg(windows)]`.
   This is the pattern that caught real bugs in Specs 1 and 2 before they reached a screen.

2. **The CPS readout is driven by `SetTimer` (~100 ms)**, whose handler does one `Relaxed` load of
   `clicks_emitted`, computes CPS from the delta since the last tick, and `InvalidateRect`s **only
   the readout's rectangle**. The GUI never touches engine memory except that one atomic read.
   Rejected: whole-window redraw each tick (wastes the cache, spins CPU the engine may want) and
   engine-signals-GUI (couples the hot loop to the GUI — forbidden, and at 4,000 CPS would flood
   the message queue).

3. **The slider maximum is 2,000 CPS**, the measured 100%-fidelity ceiling from Spec 1, not a
   round large number. A hard cap of 4,000 is reachable via the numeric field with a "beyond
   guaranteed delivery" affordance. No million-CPS slider.

## 4. Module layout

```
crates/clicker-gui/src/
├─ widget/
│  ├─ mod.rs            re-exports
│  ├─ model.rs          ANY TARGET: WidgetState, WidgetEvent, transitions, WidgetId
│  ├─ value.rs          ANY TARGET: slider/field value math, clamping to the CPS range
│  ├─ focus.rs          ANY TARGET: FocusRing — ordered ids, next/prev with wrap
│  ├─ layout.rs         ANY TARGET: client size -> per-widget DIP rects
│  ├─ render.rs         win: draw a widget via its state->Elevation + DirectWrite; caching
│  └─ text.rs           win: cached IDWriteTextFormat per style, greyscale AA
├─ app.rs               win: owns Arc<SharedState> + EngineHandle; routes events; the timer
└─ window.rs            win: (existing) input adapter feeds WidgetEvents to app
```

`model`, `value`, `focus`, `layout` carry no `windows` dependency and their tests run on Linux.

## 5. The widget model

```rust
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WidgetState { Idle, Hover, Pressed, Focused, Disabled }

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum WidgetEvent {
    PointerEnter,
    PointerLeave,
    PointerDown { x: f32, y: f32 },   // widget-local DIPs
    PointerUp   { x: f32, y: f32 },
    FocusGained,
    FocusLost,
    Key(KeyCode),                     // Tab handled by the focus ring, not here
}

/// Pure transition. Returns the next state and any action the host must apply
/// (e.g. "the button fired", "the value changed").
pub fn transition(state: WidgetState, ev: WidgetEvent) -> (WidgetState, Option<Action>);
```

`WidgetEvent` is the neutral vocabulary the Win32 layer produces and the model consumes: no HWND,
no D2D. A disabled widget ignores pointer and key events and never leaves `Disabled` except by an
explicit host call.

`WidgetState` maps to an `Elevation` for rendering: `Pressed` → `Inset`, `Disabled` → `Flat`,
everything else → `Raised`. Focus is drawn as an accent ring *in addition to* the elevation, so a
focused control still reads as raised.

### Value math (`value.rs`)

Pure functions, boundary-tested without a pointer:

- `slider_value(px: f32, groove: Range, min: u32, max: u32) -> u32` and its inverse
  `slider_pixel(value, ...)`, clamped to `1..=2000`.
- `parse_and_clamp(text: &str, min, max) -> u32` for numeric fields, rejecting non-digits and
  saturating rather than overflowing.
- The toggle is a boolean flip; still a pure function so its action mapping is tested.

### Focus (`focus.rs`)

```rust
pub struct FocusRing { order: Vec<WidgetId>, current: Option<usize> }
impl FocusRing { pub fn next(&mut self); pub fn prev(&mut self); pub fn focused(&self) -> Option<WidgetId>; }
```

Tab advances, Shift-Tab retreats, both wrapping. Skips disabled ids. Keyboard reachability is not
optional: with neumorphism's low contrast the accent focus ring is doing real accessibility work.

## 6. Widgets

| Widget | Surface | Model owns | Writes |
|---|---|---|---|
| Button (Start/Stop) | Raised → Inset on press | pressed edge, fire action | `running` |
| Toggle (follow/fixed) | Inset track, Raised knob | on/off, knob position | `position_mode` |
| Slider (CPS 1–2000) | Inset groove, Raised thumb | value math, drag, clamp | `interval_ns` |
| Numeric field | Inset well | parse, clamp, caret | `interval_ns` / limits |
| CPS readout | Inset well, large numerals | display only | — (reads `clicks_emitted`) |

Widget actions are the **only** writers to the config atomics. `interval_ns` is derived from CPS:
`interval_ns = 1_000_000_000 / cps`, with `cps == 0` reserved (not exposed on the slider).

## 7. Caching and text

**Caching.** Each widget holds a cache keyed by `(w_px, h_px, dpi_bucket, WidgetState)` mapping to
a `RenderedSurface`. A normal repaint blits the cached bitmap; a state transition re-renders that
one widget; resize/DPI change clears the cache. `render_surface` already returns exactly this
bitmap, so caching is a `HashMap` in front of it, not new rendering. Cross-fade between two cached
bitmaps on hover/press is optional polish, deferred if it complicates the first cut.

**Text (`text.rs`).** DirectWrite with a cached `IDWriteTextFormat` per style. Segoe UI Variable
if available, else Segoe UI. **Greyscale antialiasing, not ClearType** — subpixel AA against the
transparent composited surface produces colour fringing. Text draws on top of the blitted surface
each paint; the numerals are not separately cached (text is cheap; the invalidation bookkeeping
is not worth it). Text colour is `text_primary`, which the Spec 2 contrast test already pins at
≥ 4.5:1 on the base.

## 8. Wiring (`app.rs`)

`app` owns `Arc<SharedState>` and the `EngineHandle`. Flow:

- Win32 message → `window.rs` input adapter → `WidgetEvent` → routed to the hovered/focused
  widget's model → optional `Action` → `app` applies it to a config atomic and invalidates that
  widget's rect.
- `WM_TIMER` (~100 ms) → one `Relaxed` load of `clicks_emitted` → CPS from the delta →
  `InvalidateRect` only the readout rect.
- The engine thread is untouched by any of this; it reads config atomics at the top of its loop
  exactly as it did headless.

This is the whole GUI↔engine contract: config atomics written by widget actions, `clicks_emitted`
read by the timer. No locks, no channels, no per-click coupling.

## 9. Testing

**Platform-neutral (Linux):** every state transition; slider value math at the range boundaries
(1, 2000, below, above); numeric parse/clamp including junk input and overflow; focus ring
wrap/skip-disabled; layout rects for representative client sizes.

**Golden (WARP, per S2-7):** assert state → elevation — a pressed button renders inset, a disabled
one flat, a focused one shows the accent ring. Reuses the offscreen-render-and-read-pixels harness.

**The gate (manual + bench):** re-run `bench/results` with the GUI open and interacting; compare
emit-side p99 per rate against the Spec 1 baseline. No regression permitted.

**Manual checklist:** Tab through every control and confirm the focus ring is visible on each;
drive the engine entirely from the UI (set rate, start, stop); confirm the CPS readout tracks the
delivered rate; 100/150/200% DPI; light and dark base.

## 10. Gate

Spec 3 is complete when the clicker is fully operable from its own interface, the widget and
value-math tests pass, the golden state→elevation checks pass, and the bench sweep run with the
GUI active shows no emit-side p99 regression against Spec 1. Spec 4 (hotkeys, profiles) does not
begin before that.

## 11. Standing rules (inherited)

- No `unsafe` block without a comment stating the invariant that makes it sound.
- The hot loop is Spec 1's and stays untouched: no allocation, no locking, no logging.
- No performance claim without a measurement backing it.
- If the GUI ever measurably perturbs engine timing, fix the coupling, not the symptom.
