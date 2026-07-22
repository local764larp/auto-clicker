//! Per-widget appearance cache.
//!
//! `render_surface` is expensive (Gaussian shadow effects) and its inputs
//! rarely change, so a normal repaint blits the cached bitmap and only a
//! size / DPI / state / theme change re-renders. This is the caching layer the
//! brief calls for; `render_surface` already produces exactly the bitmap to
//! cache, so this is a map in front of it, not new rendering.

use crate::render::color::{Palette, Rgb, Theme};
use crate::render::device::RenderError;
use crate::render::neumorph::{draw_surface, render_surface, RenderedSurface};
use crate::render::shadow::Elevation;
use crate::widget::layout::Rect;
use crate::widget::value;
use crate::widget::WidgetState;
use std::collections::HashMap;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ROUNDED_RECT};

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

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Return the cached surface for these parameters, rendering it on a miss.
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

/// One neumorphic surface a widget draws, positioned relative to its rect.
#[derive(Copy, Clone, Debug)]
pub struct SurfaceSpec {
    pub dx: f32,
    pub dy: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub elevation: Elevation,
}

fn spec(dx: f32, dy: f32, w: f32, h: f32, radius: f32, elevation: Elevation) -> SurfaceSpec {
    SurfaceSpec { dx, dy, w, h, radius, elevation }
}

/// Groove inset (thumb half-width) so the thumb stays inside the slider rect.
pub const SLIDER_THUMB: f32 = 26.0;

/// The surfaces a widget needs, in draw order. Pure so the prepare pass and the
/// blit pass compute identical parameters and never disagree — a disagreement
/// would render inside the main BeginDraw and fail.
pub fn surface_specs(
    id: crate::widget::WidgetId,
    rect: Rect,
    state: WidgetState,
    cps: u32,
    fixed_mode: bool,
) -> Vec<SurfaceSpec> {
    use crate::widget::WidgetId::*;
    match id {
        StartStop => vec![spec(0.0, 0.0, rect.w, rect.h, 16.0, state.elevation())],
        IntervalField => vec![spec(0.0, 0.0, rect.w, rect.h, 14.0, Elevation::Inset)],
        CpsReadout => vec![spec(0.0, 0.0, rect.w, rect.h, 16.0, Elevation::Inset)],
        ModeToggle => {
            let track = spec(0.0, 0.0, rect.w, rect.h, rect.h / 2.0, Elevation::Inset);
            let k = rect.h - 8.0;
            let kx = if fixed_mode { rect.w - 4.0 - k } else { 4.0 };
            let knob = spec(kx, 4.0, k, k, k / 2.0, Elevation::Raised);
            vec![track, knob]
        }
        RateSlider => {
            let groove_y = rect.h / 2.0 - 6.0;
            let groove = spec(0.0, groove_y, rect.w, 12.0, 6.0, Elevation::Inset);
            let gmin = SLIDER_THUMB / 2.0;
            let gmax = rect.w - SLIDER_THUMB / 2.0;
            let cx = value::slider_pixel(cps, gmin, gmax, value::CPS_MIN, value::CPS_SLIDER_MAX);
            let thumb = spec(
                cx - SLIDER_THUMB / 2.0,
                rect.h / 2.0 - SLIDER_THUMB / 2.0,
                SLIDER_THUMB,
                SLIDER_THUMB,
                SLIDER_THUMB / 2.0,
                Elevation::Raised,
            );
            vec![groove, thumb]
        }
    }
}

fn color_f(c: Rgb, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a }
}

/// Draw the accent focus ring around a widget rect (DIP). The one affordance
/// that survives neumorphism's low contrast, so it is always accent-coloured.
///
/// # Safety
/// `ctx` must be a live device context inside a `BeginDraw`.
pub unsafe fn accent_ring(ctx: &ID2D1DeviceContext, rect: Rect, palette: &Palette) {
    // SAFETY: ctx is live and mid-draw; the brush outlives the stroke call.
    unsafe {
        let Ok(brush) = ctx.CreateSolidColorBrush(&color_f(palette.accent, 1.0), None) else {
            return;
        };
        let inset = 2.0;
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: rect.x - inset,
                top: rect.y - inset,
                right: rect.x + rect.w + inset,
                bottom: rect.y + rect.h + inset,
            },
            radiusX: 10.0,
            radiusY: 10.0,
        };
        ctx.DrawRoundedRectangle(&rr, &brush, 2.0, None);
    }
}

/// Populate the cache with every surface each widget needs, outside the main
/// BeginDraw (render_surface opens its own draw sessions).
pub fn prepare(
    cache: &mut SurfaceCache,
    ctx: &ID2D1DeviceContext,
    specs: &[(Rect, Vec<SurfaceSpec>)],
    palette: &Palette,
    dpi_scale: f32,
) -> Result<(), RenderError> {
    for (_rect, list) in specs {
        for s in list {
            let _ = cache.get(ctx, s.w, s.h, s.radius, s.elevation, palette, dpi_scale)?;
        }
    }
    Ok(())
}

/// Blit a widget's cached surfaces at its rect. Cache hits only (prepare ran
/// first), so no rendering happens inside the caller's BeginDraw.
///
/// # Safety
/// `ctx` must be a live device context inside a `BeginDraw`.
pub unsafe fn blit_specs(
    cache: &mut SurfaceCache,
    ctx: &ID2D1DeviceContext,
    rect: Rect,
    list: &[SurfaceSpec],
    palette: &Palette,
    dpi_scale: f32,
) -> Result<(), RenderError> {
    for s in list {
        let surf = cache.get(ctx, s.w, s.h, s.radius, s.elevation, palette, dpi_scale)?;
        // SAFETY: caller contract — ctx is mid-draw.
        unsafe {
            draw_surface(ctx, surf, rect.x + s.dx, rect.y + s.dy);
        }
    }
    Ok(())
}
