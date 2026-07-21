# Spec 2 — Render Foundation & Neumorphic Surface Primitive

**Date:** 2026-07-21
**Scope:** `clicker-gui` window shell, D3D11 → D2D → DirectComposition stack, Per-Monitor DPI v2,
and the `neumorph::surface` primitive. Work items 5–6 of `BUILD_PROMPT.md`.
**Predecessor:** [Spec 1](2026-07-21-clicker-engine-design.md) — merged, gate satisfied.

## 1. Gate status from Spec 1

Spec 2 was gated behind measured throughput. That gate is met — see
[`bench/results/README.md`](../../../bench/results/README.md):

- Ceiling **~4,000 delivered CPS**; 100% delivery through 2,000 CPS.
- Scheduler accurate to **well under a microsecond** at every throttled rate
  (50 CPS: mean/p50/p99 all 20,000.0 µs against a 20,000 µs target).
- The emit-side interval table is the **baseline that makes this spec's successor testable**:
  Spec 3 must not degrade p99 at any rate.

## 2. Scope

**In:** window creation and borderless shell, the device stack, DPI handling, device-lost
recovery, the colour system, and `neumorph::surface` with `Raised | Inset | Flat`.

**Out:** widgets, hit-testing, focus, caching, and any wiring to the engine — all Spec 3.
`clicker-gui` does **not** depend on `clicker-core` in this spec; nothing here needs it, and
adding the dependency early would invite premature coupling.

## 3. Decisions

1. **Golden-image tests render on WARP.** `D3D_DRIVER_TYPE_WARP` is Microsoft's software
   rasterizer and produces bit-identical output regardless of GPU, driver version, or vendor.
   Hardware rendering with a pixel tolerance was rejected: the tolerance required to absorb
   driver variation is precisely wide enough to hide the subtly-wrong blur radius the tests
   exist to catch. The shipping app still uses `D3D_DRIVER_TYPE_HARDWARE`; only the test path
   forces WARP, via one parameter on device creation.

   This follows directly from Spec 1's experience: a flaky test gets muted, which is worse than
   no test.

2. **A standalone primitive playground.** The brief requires the primitive be correct in
   isolation before anything depends on it. An example binary renders raised/inset/flat surfaces
   across sizes, radii, DPI scales, and both bases on one screen, with parameters adjustable at
   runtime. It is also the source from which golden images are generated.

3. **Colour tokens are platform-neutral and contrast-tested.** The brief is explicit that
   accessibility must not lose to aesthetics. Rather than asserting compliance in prose, the
   tokens live in a `cfg`-free module and a unit test computes WCAG relative luminance and fails
   the build if any text-on-base pair drops below 4.5:1. This runs on the Linux target with the
   rest of the platform-neutral code.

## 4. Module layout

```
crates/clicker-gui/src/
├─ main.rs               entry, DPI awareness, message pump
├─ window.rs             CreateWindowExW, wndproc, WM_DPICHANGED, borderless shell
├─ render/
│  ├─ mod.rs             re-exports
│  ├─ device.rs          D3D11 -> DXGI -> D2D -> DComp; WARP switch; device-lost recovery
│  ├─ color.rs           ANY TARGET: palette tokens, sRGB math, WCAG contrast
│  └─ neumorph.rs        surface(ctx, rect, radius, Elevation) — the primitive
└─ examples/playground.rs   the isolation harness
```

`color.rs` is deliberately platform-neutral so its contrast tests run everywhere.

## 5. Device stack

`ID3D11Device` → `IDXGIDevice` → `ID2D1Device` → `ID2D1DeviceContext`, presented through a
DirectComposition visual tree with a flip-model swapchain
(`DXGI_SWAP_EFFECT_FLIP_DISCARD`, `DXGI_ALPHA_MODE_PREMULTIPLIED`).

DirectComposition is required, not decorative: a borderless rounded window needs true per-pixel
transparency, which a plain HWND swapchain cannot provide.

```rust
pub enum DriverKind { Hardware, Warp }
pub struct RenderDevice { /* d3d, dxgi, d2d device+context, dcomp device+target+visual, swapchain */ }
impl RenderDevice {
    pub fn new(hwnd: HWND, kind: DriverKind) -> Result<Self, RenderError>;
    pub fn resize(&mut self, w: u32, h: u32) -> Result<(), RenderError>;
    pub fn recreate(&mut self) -> Result<(), RenderError>;
}
```

**Device-lost is handled, not ignored.** `DXGI_ERROR_DEVICE_REMOVED`, `DXGI_ERROR_DEVICE_RESET`,
and `D2DERR_RECREATE_TARGET` tear down and rebuild the stack. Driver updates and GPU resets
trigger this in ordinary use; skipping it yields a permanently black window that reads as a hang.

**DPI:** `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` before any window exists.
`WM_DPICHANGED` resizes to the suggested rect and rebuilds DPI-dependent resources. All primitive
geometry takes DIPs and scales at draw time, so nothing stores pixel constants.

**Render on demand.** Paint on `WM_PAINT` and explicit invalidation only. No display-refresh loop —
Spec 1 measured what the engine costs, and a spinning GUI would steal from it for nothing.

## 6. Colour system

Base is a single mid-tone; surfaces are that exact colour, and all depth comes from paired
light/dark shadows. Breaking that rule stops it being neumorphism.

| Token | Light | Dark |
|---|---|---|
| `base` | `#E0E5EC` | `#2E3239` |
| `shadow_dark` | derived, ~12% darker | derived, ~40% darker |
| `shadow_light` | derived, near-white | derived, ~18% lighter |
| `text_primary` | must clear 4.5:1 on `base` | must clear 4.5:1 on `base` |
| `accent` | single hue, running state | same |

Light source is fixed **top-left**: raised elements cast light up-left and dark down-right; inset
inverts both. Consistency across every element is what sells the effect.

Raised surfaces carry a subtle diagonal gradient, a few percent lighter toward the light source.
Without it the result is a blurry grey blob rather than crisp neumorphism.

**Contrast is enforced by test**, not by intent: `color.rs` computes WCAG relative luminance and
the suite fails if `text_primary` on `base` drops below 4.5:1 in either theme. The accent must
also be distinguishable independently of the shadow treatment, since shadows convey no
information to a low-vision user.

## 7. The surface primitive

```rust
pub enum Elevation { Raised, Inset, Flat }
pub fn surface(
    ctx: &ID2D1DeviceContext,
    rect: D2D_RECT_F,
    radius: f32,
    elevation: Elevation,
    palette: &Palette,
) -> Result<(), RenderError>;
```

Every widget in Spec 3 builds on this one function. Duplicating shadow construction across
widgets is what makes the design drift, so it is deliberately the only way to draw a surface.

**Raised** — outer dual shadow:
1. Render the rounded-rect silhouette to an intermediate `ID2D1Bitmap1`.
2. Two `CLSID_D2D1Shadow` effects: dark offset `(+dx, +dy)`, light offset `(-dx, -dy)`, blur
   radius ≈ offset magnitude.
3. Composite both beneath the filled surface, then draw the surface with its gradient on top.

**Inset** — no built-in effect exists, so it is constructed:
1. Take the shape's alpha mask and invert it, so the region outside the shape is opaque.
2. Gaussian-blur the inverted mask (`CLSID_D2D1GaussianBlur`).
3. Composite back with `D2D1_COMPOSITE_MODE_SOURCE_IN` against the original mask, clipping the
   blur to the shape's interior.
4. Twice — dark tinted down-right, light tinted up-left.

**Flat** — fill plus gradient, no shadow. Exists so callers never hand-roll the "no elevation"
case inconsistently.

Corner radii are large and uniform (14–20 DIP at 100%); small radii read as flat.

This is the most intricate rendering in the project, which is exactly why it is one function with
a golden-image suite behind it.

## 8. Window shell

Borderless with a custom title bar: rounded corners, drag via `WM_NCHITTEST` returning
`HTCAPTION` over the header, custom minimize/close. Snap Layouts and system window-management
gestures must keep working — **verified by manual checklist, not assumed**, since a borderless
shell is the classic way to break them.

## 9. Testing

**Platform-neutral (runs on the Linux target):**
- WCAG contrast ratios for every text-on-base pair, both themes.
- sRGB ↔ linear round-trips.
- Shadow parameter derivation: offsets, blur radii, and gradient stops as pure functions of
  elevation and radius.

**Golden checks (WARP, deterministic):**
- Raised, inset, and flat, both light and dark bases.
- Implemented as **pixel-property assertions** rather than stored PNGs: each
  surface renders offscreen on WARP over an opaque base, the pixels are read
  back, and the neumorphic invariants are asserted directly — light comes from
  the top-left, raised casts an outer halo, inset a recessed inner shadow, flat
  none. Rationale: property assertions need no bless step, give a legible
  failure message, and encode the design rules that actually regress (a flipped
  offset, a missing blur). The inset direction bug found by eye during S2-6 is
  exactly what these now catch automatically. A separate test confirms WARP's
  output is bit-reproducible, which was the premise for choosing it.
- Deferred: multi-DPI-scale rendering is exercised at runtime (the window
  refreshes surfaces on DPI change) but not yet asserted in the golden suite.

**Manual checklist:** 100%/150%/200% DPI; multi-monitor with mixed DPI; light and dark base;
Snap Layouts still functional; device-lost recovery (trigger via driver restart or
`dxcap`/TDR) repaints rather than blacking out.

## 10. Gate

Spec 2 is complete when the playground renders raised, inset, and flat surfaces that are visually
correct at all three DPI scales in both themes, the golden suite passes on WARP, and the contrast
tests pass. Spec 3 (widgets) does not begin before that.

## 11. Standing rules (inherited)

- No `unsafe` block without a comment stating the invariant that makes it sound.
- Every raw handle gets an RAII guard; COM interfaces are refcounted by the `windows` crate.
- No performance claim without a measurement backing it.
- The GUI must never measurably perturb engine timing — Spec 1's emit-side table is the baseline.
