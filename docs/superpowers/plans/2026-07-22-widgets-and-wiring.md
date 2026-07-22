# Widgets & Engine Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the clicker fully operable from its own neumorphic interface, wired to the `clicker-core` engine through the shared atomics, without perturbing engine timing.

**Architecture:** Each widget is a platform-neutral model (state machine, value math, focus, layout — tested on Linux) plus a thin `#[cfg(windows)]` renderer over `neumorph::render_surface` and a Win32 input adapter. The GUI writes config atomics on widget actions and reads `clicks_emitted` on a ~100 ms timer that invalidates only the readout rect.

**Tech Stack:** Rust 1.96, the `windows` crate 0.62 (+ DirectWrite), `clicker-core`.

## Global Constraints

- Model/value/focus/layout modules carry **no** `windows` dependency; they pass `cargo check -p clicker-gui --target x86_64-unknown-linux-gnu --all-targets`.
- Widget actions are the **only** writers to config atomics. `interval_ns = 1_000_000_000 / cps`.
- Slider range is **1..=2000 CPS** (Spec 1 measured ceiling). Numeric field hard cap 4000.
- The CPS readout timer does one `Relaxed` load of `clicks_emitted` and invalidates only the readout rect. No locks, no channels, no per-click coupling.
- DirectWrite uses **greyscale** antialiasing (not ClearType) and text colour `text_primary`.
- No `unsafe` without an invariant comment. The Spec 1 hot loop stays untouched.
- The completion gate: bench emit-side p99 with the GUI active must not regress vs `bench/results/README.md`.

---

## File Structure

| File | Platform | Responsibility |
|---|---|---|
| `widget/model.rs` | any | `WidgetState`, `WidgetEvent`, `KeyCode`, `Action`, `transition()` |
| `widget/value.rs` | any | slider ↔ value math, numeric parse/clamp, cps↔interval |
| `widget/focus.rs` | any | `FocusRing` — ordered ids, next/prev wrap, skip disabled |
| `widget/layout.rs` | any | `WidgetId`, client size → per-widget DIP rects |
| `widget/mod.rs` | any | re-exports; `#[cfg(windows)]` submodules gated |
| `widget/text.rs` | win | cached `IDWriteTextFormat`, greyscale AA draw helper |
| `widget/render.rs` | win | state→Elevation, per-widget bitmap cache, compose text |
| `app.rs` | win | owns `Arc<SharedState>` + `EngineHandle`; routes events; timer |
| `window.rs` | win | (modify) input adapter → `WidgetEvent`; hosts `app` |
| `main.rs` | win | (modify) construct app, start engine |

---

### Task 1: Widget model — state machine

**Files:** Create `crates/clicker-gui/src/widget/model.rs`, `crates/clicker-gui/src/widget/mod.rs`; Modify `crates/clicker-gui/src/lib.rs`

**Interfaces:**
- Produces: `WidgetState::{Idle,Hover,Pressed,Focused,Disabled}`; `KeyCode::{Space,Enter,Left,Right,Up,Down,Tab,Backspace,Digit(u8),Other}`; `WidgetEvent::{PointerEnter,PointerLeave,PointerDown{x:f32,y:f32},PointerUp{x:f32,y:f32},FocusGained,FocusLost,Key(KeyCode)}`; `Action::{Fire,ValueChanged}`; `transition(WidgetState, WidgetEvent) -> (WidgetState, Option<Action>)`; `WidgetState::elevation(self) -> Elevation`.

- [ ] **Step 1: Write failing tests** in `model.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Elevation;

    #[test]
    fn hover_enter_and_leave() {
        assert_eq!(transition(WidgetState::Idle, WidgetEvent::PointerEnter).0, WidgetState::Hover);
        assert_eq!(transition(WidgetState::Hover, WidgetEvent::PointerLeave).0, WidgetState::Idle);
    }

    #[test]
    fn press_then_release_fires() {
        let (s, a) = transition(WidgetState::Hover, WidgetEvent::PointerDown { x: 1.0, y: 1.0 });
        assert_eq!(s, WidgetState::Pressed);
        assert_eq!(a, None);
        let (s, a) = transition(s, WidgetEvent::PointerUp { x: 1.0, y: 1.0 });
        assert_eq!(s, WidgetState::Hover);
        assert_eq!(a, Some(Action::Fire));
    }

    #[test]
    fn release_after_leaving_does_not_fire() {
        // Press, pointer leaves, release elsewhere: no fire (drag-off cancel).
        let (s, _) = transition(WidgetState::Hover, WidgetEvent::PointerDown { x: 1.0, y: 1.0 });
        let (s, _) = transition(s, WidgetEvent::PointerLeave);
        let (s, a) = transition(s, WidgetEvent::PointerUp { x: 1.0, y: 1.0 });
        assert_eq!(a, None, "releasing after drag-off must not fire");
        assert_eq!(s, WidgetState::Idle);
    }

    #[test]
    fn space_and_enter_fire_when_focused() {
        for k in [KeyCode::Space, KeyCode::Enter] {
            let (s, a) = transition(WidgetState::Focused, WidgetEvent::Key(k));
            assert_eq!(a, Some(Action::Fire));
            assert_eq!(s, WidgetState::Focused);
        }
    }

    #[test]
    fn disabled_ignores_everything() {
        for ev in [
            WidgetEvent::PointerEnter,
            WidgetEvent::PointerDown { x: 0.0, y: 0.0 },
            WidgetEvent::Key(KeyCode::Space),
        ] {
            assert_eq!(transition(WidgetState::Disabled, ev), (WidgetState::Disabled, None));
        }
    }

    #[test]
    fn focus_gained_and_lost() {
        assert_eq!(transition(WidgetState::Idle, WidgetEvent::FocusGained).0, WidgetState::Focused);
        assert_eq!(transition(WidgetState::Focused, WidgetEvent::FocusLost).0, WidgetState::Idle);
    }

    #[test]
    fn state_maps_to_elevation() {
        assert_eq!(WidgetState::Pressed.elevation(), Elevation::Inset);
        assert_eq!(WidgetState::Disabled.elevation(), Elevation::Flat);
        assert_eq!(WidgetState::Idle.elevation(), Elevation::Raised);
        assert_eq!(WidgetState::Hover.elevation(), Elevation::Raised);
        assert_eq!(WidgetState::Focused.elevation(), Elevation::Raised);
    }
}
```

- [ ] **Step 2: Run to verify fail:** `cargo test -p clicker-gui model` → FAIL (undefined).

- [ ] **Step 3: Implement** — prepend to `model.rs`:

```rust
use crate::render::Elevation;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WidgetState { Idle, Hover, Pressed, Focused, Disabled }

impl WidgetState {
    /// Rendering elevation for this state. Focus is drawn as an accent ring on
    /// top, so a focused control still reads as raised.
    pub fn elevation(self) -> Elevation {
        match self {
            WidgetState::Pressed => Elevation::Inset,
            WidgetState::Disabled => Elevation::Flat,
            _ => Elevation::Raised,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeyCode { Space, Enter, Left, Right, Up, Down, Tab, Backspace, Digit(u8), Other }

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum WidgetEvent {
    PointerEnter,
    PointerLeave,
    PointerDown { x: f32, y: f32 },
    PointerUp { x: f32, y: f32 },
    FocusGained,
    FocusLost,
    Key(KeyCode),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Action { Fire, ValueChanged }

/// Pure state transition shared by push-button-like widgets. Widgets with
/// richer behaviour (slider drag, field editing) layer their value math
/// (`value.rs`) on top of this and interpret `Action` themselves.
pub fn transition(state: WidgetState, ev: WidgetEvent) -> (WidgetState, Option<Action>) {
    use WidgetEvent::*;
    if state == WidgetState::Disabled {
        return (WidgetState::Disabled, None); // only a host call re-enables
    }
    match (state, ev) {
        (WidgetState::Idle, PointerEnter) => (WidgetState::Hover, None),
        (WidgetState::Hover, PointerLeave) => (WidgetState::Idle, None),
        (WidgetState::Hover, PointerDown { .. }) => (WidgetState::Pressed, None),
        (WidgetState::Pressed, PointerUp { .. }) => (WidgetState::Hover, Some(Action::Fire)),
        // Drag-off: pointer left while pressed. A later release must not fire.
        (WidgetState::Pressed, PointerLeave) => (WidgetState::Idle, None),
        (WidgetState::Idle, FocusGained) | (WidgetState::Hover, FocusGained) => {
            (WidgetState::Focused, None)
        }
        (WidgetState::Focused, FocusLost) => (WidgetState::Idle, None),
        (WidgetState::Focused, Key(KeyCode::Space)) | (WidgetState::Focused, Key(KeyCode::Enter)) => {
            (WidgetState::Focused, Some(Action::Fire))
        }
        _ => (state, None),
    }
}
```

- [ ] **Step 4:** Create `widget/mod.rs`:

```rust
pub mod focus;
pub mod layout;
pub mod model;
pub mod value;

pub use focus::FocusRing;
pub use layout::{layout, WidgetId, WidgetRects};
pub use model::{transition, Action, KeyCode, WidgetEvent, WidgetState};

#[cfg(windows)]
pub mod render;
#[cfg(windows)]
pub mod text;
```

Add `pub mod widget;` to `crates/clicker-gui/src/lib.rs`. (`value.rs`, `focus.rs`, `layout.rs` are created in Tasks 2–4; if compiling Task 1 alone, temporarily comment their `mod`/`pub use` lines, or create empty stubs. Since this plan executes in order, create the stubs now: `echo "" > value.rs` etc., filled by later tasks.)

- [ ] **Step 5:** `cargo test -p clicker-gui model` → PASS. Then `cargo check -p clicker-gui --target x86_64-unknown-linux-gnu --all-targets` → Finished.

- [ ] **Step 6: Commit** `feat(gui): widget state-machine model`.

---

### Task 2: Value math

**Files:** Modify `crates/clicker-gui/src/widget/value.rs`

**Interfaces:**
- Produces: `cps_to_interval_ns(cps: u32) -> u64`; `slider_value(px: f32, groove_min: f32, groove_max: f32, vmin: u32, vmax: u32) -> u32`; `slider_pixel(value: u32, groove_min: f32, groove_max: f32, vmin: u32, vmax: u32) -> f32`; `parse_and_clamp(text: &str, min: u32, max: u32) -> u32`; constants `CPS_MIN: u32 = 1`, `CPS_SLIDER_MAX: u32 = 2000`, `CPS_HARD_MAX: u32 = 4000`.

- [ ] **Step 1: Write failing tests** in `value.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cps_maps_to_interval() {
        assert_eq!(cps_to_interval_ns(1000), 1_000_000);
        assert_eq!(cps_to_interval_ns(100), 10_000_000);
        assert_eq!(cps_to_interval_ns(1), 1_000_000_000);
    }

    #[test]
    fn cps_zero_is_treated_as_one_not_unthrottled() {
        // The slider must never hand the engine interval_ns == 0 by accident.
        assert_eq!(cps_to_interval_ns(0), 1_000_000_000);
    }

    #[test]
    fn slider_endpoints_map_to_range_ends() {
        assert_eq!(slider_value(0.0, 0.0, 200.0, 1, 2000), 1);
        assert_eq!(slider_value(200.0, 0.0, 200.0, 1, 2000), 2000);
    }

    #[test]
    fn slider_clamps_outside_the_groove() {
        assert_eq!(slider_value(-50.0, 0.0, 200.0, 1, 2000), 1);
        assert_eq!(slider_value(9999.0, 0.0, 200.0, 1, 2000), 2000);
    }

    #[test]
    fn slider_value_and_pixel_are_inverse_at_midpoint() {
        let px = slider_pixel(1000, 0.0, 200.0, 1, 2000);
        let v = slider_value(px, 0.0, 200.0, 1, 2000);
        assert!((v as i64 - 1000).abs() <= 6, "round-trip drifted: {v}");
    }

    #[test]
    fn parse_rejects_junk_and_clamps() {
        assert_eq!(parse_and_clamp("500", 1, 4000), 500);
        assert_eq!(parse_and_clamp("", 1, 4000), 1);            // empty -> min
        assert_eq!(parse_and_clamp("12x9", 1, 4000), 1);        // non-digit -> min
        assert_eq!(parse_and_clamp("99999", 1, 4000), 4000);    // over -> max
        assert_eq!(parse_and_clamp("0", 1, 4000), 1);           // under -> min
    }

    #[test]
    fn parse_saturates_rather_than_overflowing() {
        let huge = "999999999999999999999999";
        assert_eq!(parse_and_clamp(huge, 1, 4000), 4000);
    }
}
```

- [ ] **Step 2:** `cargo test -p clicker-gui value` → FAIL.

- [ ] **Step 3: Implement** — prepend:

```rust
pub const CPS_MIN: u32 = 1;
pub const CPS_SLIDER_MAX: u32 = 2000;
pub const CPS_HARD_MAX: u32 = 4000;

/// Clicks per second to the engine's per-click period. `cps == 0` is coerced to
/// 1: the slider must never accidentally hand the engine unthrottled mode.
pub fn cps_to_interval_ns(cps: u32) -> u64 {
    let cps = cps.max(1) as u64;
    1_000_000_000 / cps
}

/// Pointer pixel within a horizontal groove to a value, clamped to the range.
pub fn slider_value(px: f32, groove_min: f32, groove_max: f32, vmin: u32, vmax: u32) -> u32 {
    let span = (groove_max - groove_min).max(1.0);
    let t = ((px - groove_min) / span).clamp(0.0, 1.0);
    let v = vmin as f32 + t * (vmax - vmin) as f32;
    v.round().clamp(vmin as f32, vmax as f32) as u32
}

/// The inverse: a value's thumb-centre pixel within the groove.
pub fn slider_pixel(value: u32, groove_min: f32, groove_max: f32, vmin: u32, vmax: u32) -> f32 {
    let denom = (vmax - vmin).max(1) as f32;
    let t = ((value.clamp(vmin, vmax) - vmin) as f32) / denom;
    groove_min + t * (groove_max - groove_min)
}

/// Parse a decimal field, coercing junk/empty to `min` and saturating to `max`.
pub fn parse_and_clamp(text: &str, min: u32, max: u32) -> u32 {
    let t = text.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return min;
    }
    match t.parse::<u64>() {
        Ok(v) => (v.max(min as u64).min(max as u64)) as u32,
        Err(_) => max, // only reachable on overflow, since all bytes are digits
    }
}
```

- [ ] **Step 4:** `cargo test -p clicker-gui value` → PASS; cross-target check → Finished.

- [ ] **Step 5: Commit** `feat(gui): widget value math with clamped CPS range`.

---

### Task 3: Focus ring

**Files:** Modify `crates/clicker-gui/src/widget/focus.rs` (depends on `WidgetId` from Task 4 — define `WidgetId` in `layout.rs` first, or accept a generic id. To avoid a cycle, `FocusRing` is generic over `T: Copy + PartialEq`.)

**Interfaces:**
- Produces: `FocusRing<T>` with `new(order: Vec<(T, bool)>) -> Self` (bool = enabled), `focused() -> Option<T>`, `next()`, `prev()`, `focus(T)`, `set_enabled(T, bool)`. Wrapping; skips disabled.

- [ ] **Step 1: Write failing tests** in `focus.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> FocusRing<u32> {
        FocusRing::new(vec![(0, true), (1, true), (2, true)])
    }

    #[test]
    fn starts_unfocused() {
        assert_eq!(ring().focused(), None);
    }

    #[test]
    fn next_advances_and_wraps() {
        let mut r = ring();
        r.next();
        assert_eq!(r.focused(), Some(0));
        r.next();
        assert_eq!(r.focused(), Some(1));
        r.next();
        r.next();
        assert_eq!(r.focused(), Some(0), "should wrap");
    }

    #[test]
    fn prev_retreats_and_wraps() {
        let mut r = ring();
        r.next();
        r.prev();
        assert_eq!(r.focused(), Some(2), "prev from first wraps to last");
    }

    #[test]
    fn skips_disabled() {
        let mut r = FocusRing::new(vec![(0, true), (1, false), (2, true)]);
        r.next();
        assert_eq!(r.focused(), Some(0));
        r.next();
        assert_eq!(r.focused(), Some(2), "must skip the disabled one");
    }

    #[test]
    fn all_disabled_focuses_nothing() {
        let mut r = FocusRing::new(vec![(0, false), (1, false)]);
        r.next();
        assert_eq!(r.focused(), None);
    }
}
```

- [ ] **Step 2:** `cargo test -p clicker-gui focus` → FAIL.

- [ ] **Step 3: Implement** — prepend:

```rust
/// Tab-order ring over widget ids. `next`/`prev` wrap and skip disabled ids;
/// the accent focus ring the renderer draws on `focused()` is doing real
/// accessibility work under neumorphism's low contrast, so this is not optional.
pub struct FocusRing<T> {
    order: Vec<(T, bool)>,
    current: Option<usize>,
}

impl<T: Copy + PartialEq> FocusRing<T> {
    pub fn new(order: Vec<(T, bool)>) -> Self {
        Self { order, current: None }
    }

    pub fn focused(&self) -> Option<T> {
        self.current.map(|i| self.order[i].0)
    }

    fn step(&mut self, forward: bool) {
        let n = self.order.len();
        if n == 0 || !self.order.iter().any(|(_, en)| *en) {
            self.current = None;
            return;
        }
        let start = self.current.unwrap_or(if forward { n - 1 } else { 0 });
        for k in 1..=n {
            let i = if forward {
                (start + k) % n
            } else {
                (start + n - k) % n
            };
            if self.order[i].1 {
                self.current = Some(i);
                return;
            }
        }
    }

    pub fn next(&mut self) {
        self.step(true);
    }
    pub fn prev(&mut self) {
        self.step(false);
    }

    pub fn focus(&mut self, id: T) {
        if let Some(i) = self.order.iter().position(|(t, en)| *t == id && *en) {
            self.current = Some(i);
        }
    }

    pub fn set_enabled(&mut self, id: T, enabled: bool) {
        if let Some(e) = self.order.iter_mut().find(|(t, _)| *t == id) {
            e.1 = enabled;
        }
    }
}
```

- [ ] **Step 4:** `cargo test -p clicker-gui focus` → PASS; cross-target → Finished.

- [ ] **Step 5: Commit** `feat(gui): focus ring with wrap and disabled-skip`.

---

### Task 4: Layout

**Files:** Modify `crates/clicker-gui/src/widget/layout.rs`

**Interfaces:**
- Produces: `WidgetId::{StartStop,ModeToggle,RateSlider,IntervalField,CpsReadout}` (Copy, PartialEq, Eq, Hash); `Rect { x: f32, y: f32, w: f32, h: f32 }` with `contains(px,py)->bool` and `local(px,py)->(f32,f32)`; `WidgetRects` (a struct with one `Rect` per id) with `get(WidgetId)->Rect` and `hit(px,py)->Option<WidgetId>`; `layout(client_w_dip: f32, client_h_dip: f32) -> WidgetRects`.

- [ ] **Step 1: Write failing tests** in `layout.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains_and_local() {
        let r = Rect { x: 10.0, y: 20.0, w: 100.0, h: 40.0 };
        assert!(r.contains(15.0, 25.0));
        assert!(!r.contains(5.0, 25.0));
        assert_eq!(r.local(15.0, 25.0), (5.0, 5.0));
    }

    #[test]
    fn every_widget_gets_a_nonempty_rect() {
        let l = layout(520.0, 680.0);
        for id in [
            WidgetId::StartStop,
            WidgetId::ModeToggle,
            WidgetId::RateSlider,
            WidgetId::IntervalField,
            WidgetId::CpsReadout,
        ] {
            let r = l.get(id);
            assert!(r.w > 0.0 && r.h > 0.0, "{id:?} has an empty rect");
        }
    }

    #[test]
    fn widgets_do_not_overlap() {
        let l = layout(520.0, 680.0);
        let ids = [
            WidgetId::StartStop,
            WidgetId::ModeToggle,
            WidgetId::RateSlider,
            WidgetId::IntervalField,
            WidgetId::CpsReadout,
        ];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                let (ra, rb) = (l.get(*a), l.get(*b));
                let disjoint = ra.x + ra.w <= rb.x
                    || rb.x + rb.w <= ra.x
                    || ra.y + ra.h <= rb.y
                    || rb.y + rb.h <= ra.y;
                assert!(disjoint, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn hit_finds_the_widget_under_a_point() {
        let l = layout(520.0, 680.0);
        let r = l.get(WidgetId::StartStop);
        assert_eq!(l.hit(r.x + 1.0, r.y + 1.0), Some(WidgetId::StartStop));
        assert_eq!(l.hit(-10.0, -10.0), None);
    }

    #[test]
    fn everything_stays_within_the_client_area() {
        let (w, h) = (520.0, 680.0);
        let l = layout(w, h);
        for id in [WidgetId::StartStop, WidgetId::RateSlider, WidgetId::CpsReadout] {
            let r = l.get(id);
            assert!(r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= w && r.y + r.h <= h);
        }
    }
}
```

- [ ] **Step 2:** `cargo test -p clicker-gui layout` → FAIL.

- [ ] **Step 3: Implement** — prepend (a fixed vertical stack with a title-bar gap at top; margins in DIPs):

```rust
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum WidgetId { StartStop, ModeToggle, RateSlider, IntervalField, CpsReadout }

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }

impl Rect {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn local(&self, px: f32, py: f32) -> (f32, f32) {
        (px - self.x, py - self.y)
    }
}

pub struct WidgetRects {
    start_stop: Rect,
    mode_toggle: Rect,
    rate_slider: Rect,
    interval_field: Rect,
    cps_readout: Rect,
}

impl WidgetRects {
    pub fn get(&self, id: WidgetId) -> Rect {
        match id {
            WidgetId::StartStop => self.start_stop,
            WidgetId::ModeToggle => self.mode_toggle,
            WidgetId::RateSlider => self.rate_slider,
            WidgetId::IntervalField => self.interval_field,
            WidgetId::CpsReadout => self.cps_readout,
        }
    }
    pub fn hit(&self, px: f32, py: f32) -> Option<WidgetId> {
        for id in [
            WidgetId::StartStop,
            WidgetId::ModeToggle,
            WidgetId::RateSlider,
            WidgetId::IntervalField,
            WidgetId::CpsReadout,
        ] {
            if self.get(id).contains(px, py) {
                return Some(id);
            }
        }
        None
    }
}

/// Fixed vertical stack. The control set is small and static, so this is
/// positioned rects rather than a layout engine. All values in DIPs.
pub fn layout(client_w: f32, client_h: f32) -> WidgetRects {
    let m = 28.0; // side margin
    let w = (client_w - 2.0 * m).max(40.0);
    let caption = 44.0; // title-bar height reserved by the shell
    let mut y = caption + 24.0;

    let readout_h = 120.0;
    let cps_readout = Rect { x: m, y, w, h: readout_h };
    y += readout_h + 28.0;

    let field_h = 52.0;
    let interval_field = Rect { x: m, y, w, h: field_h };
    y += field_h + 24.0;

    let slider_h = 48.0;
    let rate_slider = Rect { x: m, y, w, h: slider_h };
    y += slider_h + 24.0;

    let toggle_w = 96.0;
    let toggle_h = 44.0;
    let mode_toggle = Rect { x: m, y, w: toggle_w, h: toggle_h };
    y += toggle_h + 24.0;

    let button_h = 64.0;
    let start_stop = Rect { x: m, y, w, h: button_h };

    WidgetRects { start_stop, mode_toggle, rate_slider, interval_field, cps_readout }
}
```

- [ ] **Step 4:** `cargo test -p clicker-gui layout` → PASS; cross-target → Finished. Also `cargo test -p clicker-gui` (all model/value/focus/layout) → PASS.

- [ ] **Step 5: Commit** `feat(gui): static widget layout`.

---

### Task 5: DirectWrite text

**Files:** Create `crates/clicker-gui/src/widget/text.rs`; add `Win32_Graphics_DirectWrite` feature.

**Interfaces:**
- Produces: `TextStyle::{Body,Large}`; `TextRenderer::new() -> Result<Self, RenderError>` (creates `IDWriteFactory`); `TextRenderer::format(&self, TextStyle) -> &IDWriteTextFormat` (cached); `draw_text(ctx, text, rect_dip, format, color, ctx_brush_cache)` helper drawing greyscale-AA text.

- [ ] **Step 1:** `cargo add windows --package clicker-gui --target 'cfg(windows)' --features Win32_Graphics_DirectWrite`

- [ ] **Step 2: Implement** `text.rs` (no pure test — this is device code; it is exercised by the golden text test in Task 11 and by manual run):

```rust
use crate::render::color::Rgb;
use crate::render::device::RenderError;
use windows::core::PCWSTR;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_DRAW_TEXT_OPTIONS_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TextStyle { Body, Large }

pub struct TextRenderer {
    _factory: IDWriteFactory,
    body: IDWriteTextFormat,
    large: IDWriteTextFormat,
}

fn make_format(f: &IDWriteFactory, size: f32, weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT)
    -> Result<IDWriteTextFormat, RenderError>
{
    // Prefer Segoe UI Variable; DirectWrite falls back to Segoe UI if absent.
    let family: Vec<u16> = "Segoe UI Variable\0".encode_utf16().collect();
    let locale: Vec<u16> = "en-us\0".encode_utf16().collect();
    // SAFETY: both string pointers are NUL-terminated UTF-16 that outlive the
    // call; all enum arguments are valid.
    let fmt = unsafe {
        f.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            PCWSTR(locale.as_ptr()),
        )?
    };
    // SAFETY: `fmt` is a live text format.
    unsafe {
        let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
    }
    Ok(fmt)
}

impl TextRenderer {
    pub fn new() -> Result<Self, RenderError> {
        // SAFETY: creates a shared DWrite factory; the out-cast matches the type.
        let factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let body = make_format(&factory, 15.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        let large = make_format(&factory, 44.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        Ok(Self { _factory: factory, body, large })
    }

    pub fn format(&self, style: TextStyle) -> &IDWriteTextFormat {
        match style {
            TextStyle::Body => &self.body,
            TextStyle::Large => &self.large,
        }
    }
}

/// Draw greyscale-AA text centred in `rect`. Greyscale (not ClearType) because
/// subpixel AA against the transparent composited surface fringes.
///
/// # Safety
/// `ctx` must be a live device context inside a `BeginDraw`.
pub unsafe fn draw_text(
    ctx: &ID2D1DeviceContext,
    text: &str,
    rect: D2D_RECT_F,
    format: &IDWriteTextFormat,
    color: Rgb,
) -> Result<(), RenderError> {
    use windows::Win32::Graphics::Direct2D::D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE;
    let utf16: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: ctx is live; the brush and the utf16 buffer outlive the call.
    unsafe {
        ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let brush = ctx.CreateSolidColorBrush(
            &D2D1_COLOR_F { r: color.r, g: color.g, b: color.b, a: 1.0 },
            None,
        )?;
        ctx.DrawText(
            &utf16,
            format,
            &rect,
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            windows::Win32::Graphics::Direct2D::Common::DWRITE_MEASURING_MODE_NATURAL,
        );
    }
    Ok(())
}
```

- [ ] **Step 3:** `cargo build -p clicker-gui` → Finished. Adapt any 0.62 signature mismatches per the note (e.g. `DrawText`'s measuring-mode enum path, `SetTextAntialiasMode` location). Verify against `cargo doc -p windows`.

- [ ] **Step 4: Commit** `feat(gui): DirectWrite text with greyscale AA`.

---

### Task 6: Widget renderer with cache

**Files:** Create `crates/clicker-gui/src/widget/render.rs`

**Interfaces:**
- Consumes: `neumorph::render_surface`/`draw_surface`, `RenderedSurface`, `WidgetState`, `TextRenderer`, `Palette`, `Rect`.
- Produces: `SurfaceCache` with `new()`, `get(ctx, w_dip, h_dip, radius, Elevation, &Palette, dpi_scale) -> Result<&RenderedSurface, RenderError>` (renders on miss, else returns cached), `clear()`. Keyed by `(w_px:u32, h_px:u32, dpi_centi:u32, elevation:u8, theme:u8)`.

- [ ] **Step 1: Implement** (the cache is device code; correctness verified by the "no re-render when key unchanged" behaviour test using a `RefCell` counter, plus golden tests in Task 11):

```rust
use crate::render::color::{Palette, Theme};
use crate::render::device::RenderError;
use crate::render::neumorph::{render_surface, RenderedSurface};
use crate::render::shadow::Elevation;
use std::collections::HashMap;
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;

type Key = (u32, u32, u32, u8, u8);

fn elev_tag(e: Elevation) -> u8 {
    match e {
        Elevation::Raised => 0,
        Elevation::Inset => 1,
        Elevation::Flat => 2,
    }
}
fn theme_tag(t: Theme) -> u8 {
    match t {
        Theme::Light => 0,
        Theme::Dark => 1,
    }
}

/// Per-widget appearance cache. `render_surface` is expensive (Gaussian shadow
/// effects); its inputs rarely change, so a normal repaint blits the cached
/// bitmap and only a size/DPI/state/theme change re-renders.
#[derive(Default)]
pub struct SurfaceCache {
    map: HashMap<Key, RenderedSurface>,
}

impl SurfaceCache {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn get(
        &mut self,
        ctx: &ID2D1DeviceContext,
        w_dip: f32,
        h_dip: f32,
        radius: f32,
        elevation: Elevation,
        palette: &Palette,
        dpi_scale: f32,
    ) -> Result<&RenderedSurface, RenderError> {
        let key: Key = (
            (w_dip * dpi_scale).round() as u32,
            (h_dip * dpi_scale).round() as u32,
            (dpi_scale * 100.0).round() as u32,
            elev_tag(elevation),
            theme_tag(palette.theme),
        );
        if !self.map.contains_key(&key) {
            let surf = render_surface(ctx, w_dip, h_dip, radius, elevation, palette, dpi_scale)?;
            self.map.insert(key, surf);
        }
        Ok(&self.map[&key])
    }
}
```

- [ ] **Step 2:** Add to `widget/mod.rs`: `#[cfg(windows)] pub use render::SurfaceCache;` (the `pub mod render;` already added in Task 1). `cargo build -p clicker-gui` → Finished.

- [ ] **Step 3: Commit** `feat(gui): per-widget surface cache`.

---

### Task 7: App state and engine ownership

**Files:** Create `crates/clicker-gui/src/app.rs`; add `clicker-core` dependency.

**Interfaces:**
- Consumes: `clicker_core::{SharedState, EngineHandle, PositionMode}`, `widget::{FocusRing, WidgetId, WidgetState, layout, value}`.
- Produces: `App` holding `Arc<SharedState>`, `EngineHandle`, per-widget `WidgetState`, `FocusRing<WidgetId>`, current cps, `interval_field_text`, `mode`, `last_clicks`/`last_cps`, `Palette`, `dpi_scale`, `SurfaceCache`, `TextRenderer`; `App::new(dpi_scale) -> Result<App, String>`; `App::set_cps(u32)`, `App::toggle_running()`, `App::set_mode(PositionMode)`, `App::sample_cps() -> u32` (called by timer), `App::is_running() -> bool`.

- [ ] **Step 1:** `cargo add clicker-core --package clicker-gui --path crates/clicker-core` (from workspace root the path is `../clicker-core`; use `cargo add clicker-core --package clicker-gui` since it is a workspace member).

- [ ] **Step 2: Write a failing pure test** for the cps→config side effect (the parts of `App` that don't need a window are tested; construction that needs the engine is `#[ignore]` or behind `#[cfg(windows)]` manual). Put the pure logic in a helper:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_cps_writes_the_derived_interval() {
        let shared = std::sync::Arc::new(clicker_core::SharedState::new());
        apply_cps(&shared, 500);
        assert_eq!(shared.snapshot().interval_ns, 2_000_000);
    }

    #[test]
    fn sample_cps_uses_the_delta_since_last_tick() {
        // 100 clicks in a 100ms window => 1000 CPS.
        assert_eq!(cps_from_delta(100, 0.100), 1000);
        assert_eq!(cps_from_delta(0, 0.100), 0);
    }
}
```

- [ ] **Step 3: Implement** `app.rs` — pure helpers first, then the `#[cfg(windows)]` `App`:

```rust
use clicker_core::SharedState;
use std::sync::Arc;

/// Write the engine's period for a target CPS. The only path that sets
/// `interval_ns`, so the derivation lives in one tested place.
pub fn apply_cps(shared: &Arc<SharedState>, cps: u32) {
    shared.set_interval_ns(crate::widget::value::cps_to_interval_ns(cps));
}

/// CPS from a click-count delta over an elapsed window.
pub fn cps_from_delta(delta_clicks: u64, elapsed_s: f64) -> u32 {
    if elapsed_s <= 0.0 {
        return 0;
    }
    (delta_clicks as f64 / elapsed_s).round() as u32
}

#[cfg(windows)]
pub use win::App;

#[cfg(windows)]
mod win {
    use super::*;
    use crate::render::color::{Palette, Theme};
    use crate::widget::render::SurfaceCache;
    use crate::widget::text::TextRenderer;
    use crate::widget::{layout, value, FocusRing, WidgetId, WidgetState};
    use clicker_core::{EngineHandle, PositionMode};
    use std::collections::HashMap;

    pub struct App {
        pub shared: Arc<SharedState>,
        _engine: EngineHandle,
        pub states: HashMap<WidgetId, WidgetState>,
        pub focus: FocusRing<WidgetId>,
        pub cps: u32,
        pub field_text: String,
        pub mode: PositionMode,
        pub palette: Palette,
        pub dpi_scale: f32,
        pub cache: SurfaceCache,
        pub text: TextRenderer,
        last_clicks: u64,
        pub last_cps: u32,
    }

    impl App {
        pub fn new(dpi_scale: f32) -> Result<App, String> {
            let shared = Arc::new(SharedState::new());
            let cps = 100u32;
            apply_cps(&shared, cps);
            let engine = EngineHandle::start(shared.clone()).map_err(|e| e.to_string())?;

            let mut states = HashMap::new();
            for id in [
                WidgetId::StartStop,
                WidgetId::ModeToggle,
                WidgetId::RateSlider,
                WidgetId::IntervalField,
                WidgetId::CpsReadout,
            ] {
                states.insert(id, WidgetState::Idle);
            }
            states.insert(WidgetId::CpsReadout, WidgetState::Disabled); // display only

            let focus = FocusRing::new(vec![
                (WidgetId::StartStop, true),
                (WidgetId::ModeToggle, true),
                (WidgetId::RateSlider, true),
                (WidgetId::IntervalField, true),
            ]);

            Ok(App {
                shared,
                _engine: engine,
                states,
                focus,
                cps,
                field_text: cps.to_string(),
                mode: PositionMode::FollowCursor,
                palette: Palette::light(),
                dpi_scale,
                cache: SurfaceCache::new(),
                text: TextRenderer::new().map_err(|e| e.to_string())?,
                last_clicks: 0,
                last_cps: 0,
            })
        }

        pub fn set_cps(&mut self, cps: u32) {
            self.cps = cps.clamp(value::CPS_MIN, value::CPS_HARD_MAX);
            self.field_text = self.cps.to_string();
            apply_cps(&self.shared, self.cps);
        }

        pub fn is_running(&self) -> bool {
            self.shared.running()
        }

        pub fn toggle_running(&mut self) {
            let now = !self.shared.running();
            if now {
                self.shared.set_engine_state(clicker_core::EngineState::Idle);
            }
            self.shared.set_running(now);
        }

        pub fn set_mode(&mut self, mode: PositionMode) {
            self.mode = mode;
            self.shared.set_position_mode(mode);
        }

        /// Sample the click counter; returns the current CPS for the readout.
        pub fn sample_cps(&mut self, elapsed_s: f64) -> u32 {
            let now = self.shared.clicks_emitted();
            let delta = now.saturating_sub(self.last_clicks);
            self.last_clicks = now;
            self.last_cps = cps_from_delta(delta, elapsed_s);
            self.last_cps
        }

        pub fn toggle_theme(&mut self) {
            self.palette = match self.palette.theme {
                Theme::Light => Palette::dark(),
                Theme::Dark => Palette::light(),
            };
            self.cache.clear();
        }
    }
}
```

- [ ] **Step 4:** Add `pub mod app;` to `lib.rs` (gate the `App` re-export windows-only; the pure helpers compile anywhere). `cargo test -p clicker-gui app` → PASS; `cargo check --target x86_64-unknown-linux-gnu --all-targets` → Finished (pure helpers only on Linux).

- [ ] **Step 5: Commit** `feat(gui): app state owning the engine handle`.

---

### Task 8: Input adapter — Win32 messages to WidgetEvents

**Files:** Modify `crates/clicker-gui/src/window.rs`

**Interfaces:**
- Consumes: `App`, `layout`, `widget::{transition, WidgetEvent, KeyCode, Action, WidgetId}`, `value`.
- Produces: message handlers for `WM_MOUSEMOVE`, `WM_LBUTTONDOWN`, `WM_LBUTTONUP`, `WM_KEYDOWN`, `WM_TIMER`, driving `App` and invalidating affected rects.

- [ ] **Step 1: Implement** — replace the `WindowState` demo fields with an `App`, and add handlers. Key points (full code):
  - `WM_CREATE`: build `App::new(dpi_scale)`; on error, `eprintln!` and continue with a null app (window still shows base). Start a `SetTimer(hwnd, 1, 100, None)`.
  - Track `hovered: Option<WidgetId>` and a `pointer_down_in: Option<WidgetId>` in `WindowState`.
  - `WM_MOUSEMOVE`: compute DIP point; `layout(...).hit(...)`; if hover changed, send `PointerLeave` to old and `PointerEnter` to new via `transition`, update states, invalidate both rects.
  - `WM_LBUTTONDOWN`: hit → focus it (`app.focus.focus(id)`), send `PointerDown{local}`; for the slider, additionally `app.set_cps(value::slider_value(local_x, ...))`; invalidate.
  - `WM_LBUTTONUP`: send `PointerUp`; if `Action::Fire` on StartStop → `app.toggle_running()`; on ModeToggle → flip mode; invalidate.
  - `WM_KEYDOWN`: map VK to `KeyCode`; `VK_TAB` → `app.focus.next()`/`prev()` (Shift), invalidate old+new focused; arrows on a focused slider → `set_cps(cps ± step)`; Space/Enter on focused StartStop → toggle.
  - `WM_TIMER`: `let cps = app.sample_cps(0.100);` then `InvalidateRect(hwnd, Some(&readout_rect_px), false)` — only the readout.
  - `WM_DPICHANGED`/`WM_SIZE`: `app.cache.clear()` and update `app.dpi_scale`.

  (This step's code is long; write it directly against the existing `window.rs` wndproc, following the existing SAFETY-comment conventions. Each `unsafe` Win32 call gets its invariant comment. The DIP conversion is `screen_or_client_px / dpi_scale`.)

- [ ] **Step 2:** `cargo build -p clicker-gui --release` → Finished.

- [ ] **Step 3: Commit** `feat(gui): Win32 input adapter feeding widget events`.

---

### Task 9: Draw the widgets

**Files:** Modify `crates/clicker-gui/src/window.rs` (the `paint` fn), extend `widget/render.rs` with per-widget draw functions.

**Interfaces:**
- Produces in `render.rs`: `draw_button`, `draw_toggle`, `draw_slider`, `draw_field`, `draw_readout` — each takes `(ctx, &mut SurfaceCache, &TextRenderer, rect, state, &App-derived data, &Palette, dpi_scale)` and composes a cached surface + text/accent. Also `accent_ring(ctx, rect, &Palette)` drawing the focus ring.

- [ ] **Step 1: Implement** the five draw functions + focus ring. Each: `cache.get(...)` for the surface at the widget's elevation, `draw_surface` it, then overlay — button: centred label ("Start"/"Stop"); toggle: knob surface at on/off position + label; slider: groove (inset) with a raised thumb surface at `slider_pixel(cps,...)`; field: the `field_text` in an inset well; readout: `last_cps` large numerals + "CPS" caption. When a widget is the focused id, draw `accent_ring` around it. The running state tints the Start/Stop button's label or draws an accent bar using `palette.accent`.

- [ ] **Step 2:** `paint()` iterates the widget ids in layout order, calling the right draw fn with the widget's current `WidgetState` and the `App` data. Pre-render pass (surface cache fills) happens before `begin_draw`; text/accent overlays happen inside it (text needs the frame's `BeginDraw`; surfaces are pre-rendered bitmaps blitted inside it — consistent with S2-5's discipline).

- [ ] **Step 3:** `cargo build -p clicker-gui --release` → Finished. Run it; screenshot; verify all five widgets render, the focus ring appears on Tab, and the accent shows when running.

- [ ] **Step 4: Commit** `feat(gui): render all five widgets with focus ring`.

---

### Task 10: Wire interactions end to end

**Files:** Modify `window.rs` / `app.rs` as needed.

- [ ] **Step 1:** Manually verify the full loop with the app running (screenshot + observation):
  - Slider drag changes the readout target and the engine's rate (watch `clicks_emitted` climb faster).
  - Start button toggles `running`; label flips Start↔Stop; accent shows.
  - Mode toggle flips follow/fixed.
  - Numeric field: type digits, Enter commits via `parse_and_clamp`, slider thumb moves to match.
  - CPS readout tracks the delivered rate (~target up to 2000).
  - F8 still stops the engine (the Spec 1 kill switch is registered by `EngineHandle::start`).

- [ ] **Step 2:** Fix any wiring gaps found. Re-run.

- [ ] **Step 3: Commit** `feat(gui): end-to-end interaction wiring`.

---

### Task 11: Golden state→elevation checks

**Files:** Modify `widget/render.rs` with a `#[cfg(all(test, windows))]` golden module (reuse the S2-7 WARP readback helper — extract it to a shared test util or duplicate the small readback fn).

**Interfaces:** Consumes `create_context(DriverKind::Warp)`, `render_surface`.

- [ ] **Step 1: Write tests** asserting state→elevation renders correctly by rendering a widget surface at each state's elevation and checking the neumorphic invariant (pressed=inset recesses dark top-left; disabled=flat uniform; idle=raised outer halo). This is the S2-7 assertion set applied via `WidgetState::elevation()`:

```rust
#[cfg(all(test, windows))]
mod golden {
    use super::*;
    use crate::render::color::Palette;
    use crate::render::device::{create_context, DriverKind};
    use crate::widget::WidgetState;
    // ... reuse render_over_base-style readback from neumorph::golden ...

    #[test]
    fn pressed_button_renders_inset() {
        let p = Palette::light();
        assert_eq!(WidgetState::Pressed.elevation(), crate::render::shadow::Elevation::Inset);
        // render at that elevation and assert the interior top-left is darker
        // than bottom-right (inner shadow), matching neumorph::golden.
    }

    #[test]
    fn disabled_widget_renders_flat() {
        assert_eq!(WidgetState::Disabled.elevation(), crate::render::shadow::Elevation::Flat);
    }
}
```

  (Where practical, call the actual `neumorph::render_surface` at `state.elevation()` and reuse the pixel-invariant assertions from `neumorph::golden`. The point is that a future change to `WidgetState::elevation()` that broke the mapping fails a test.)

- [ ] **Step 2:** `cargo test -p clicker-gui golden` → PASS.

- [ ] **Step 3: Commit** `test(gui): golden state-to-elevation checks`.

---

### Task 12: The gate — GUI must not perturb engine timing

**Files:** none (measurement); update `bench/results/README.md`.

- [ ] **Step 1:** With the GUI **built and running** (engine driven from the UI at a fixed rate), run the bench sweep in a second process:
  `cargo run -p bench --release > bench/results/2026-07-22-gui-active.md 2>&1`
  (The bench starts its own engine + receiver; the point is the machine is under the GUI's load — GUI open, timer firing, a widget being interacted with — while the bench measures. Note: bench and GUI each register F8; run the GUI without starting its engine, or temporarily have the GUI use a different panic key, to avoid the 1409 collision. Simplest: close the GUI's engine by not pressing Start, but keep the window painting and the timer running.)

- [ ] **Step 2:** Compare the emit-side p99 per rate against `bench/results/README.md` (Spec 1 baseline). Assert no material regression (allow run-to-run variance ~±150 CPS on the ceiling; p99 at ≤2000 CPS should stay within a few percent).

- [ ] **Step 3:** Write the finding into `bench/results/README.md` under a new "GUI-active timing" section: the compared p99 numbers and the verdict. If a regression appears, STOP and fix the coupling (per the standing rule) rather than proceeding.

- [ ] **Step 4: Commit** `test(bench): GUI-active timing shows no engine regression`.

---

## Gate

Spec 3 is complete when the clicker is fully operable from its interface, all pure + golden tests pass, and `bench/results/README.md` records the GUI-active sweep with no emit-side p99 regression against Spec 1.
