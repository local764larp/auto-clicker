//! Per-widget appearance cache.
//!
//! `render_surface` is expensive (Gaussian shadow effects) and its inputs
//! rarely change, so a normal repaint blits the cached bitmap and only a
//! size / DPI / state / theme change re-renders. This is the caching layer the
//! brief calls for; `render_surface` already produces exactly the bitmap to
//! cache, so this is a map in front of it, not new rendering.

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
