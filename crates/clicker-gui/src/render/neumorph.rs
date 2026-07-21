//! The neumorphic surface primitive.
//!
//! Direct2D has no neumorphism primitive, so both directions are built from
//! effects. This is the single most intricate piece of rendering in the
//! project, and every widget is meant to sit on top of it — so it is one
//! function, not shadow construction copy-pasted per widget.
//!
//! Each surface renders into its own `ID2D1Bitmap1`, sized to include the
//! shadow halo. That doubles as the caching layer the brief calls for: a normal
//! repaint blits the cached bitmap; only a size/DPI/state change re-renders.
//!
//! ## Draw-session discipline
//!
//! [`render_surface`] runs its own `BeginDraw`/`EndDraw` cycles against
//! intermediate targets, so it must be called while the context is **not**
//! already inside a `BeginDraw`. The caller renders (or refreshes) its surfaces
//! first, then blits them during the main frame.

use crate::render::color::{Palette, Rgb};
use crate::render::device::RenderError;
use crate::render::shadow::{clamp_radius, shadow_offsets, shadow_params, surface_gradient, Elevation};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_SOURCE_OVER,
    D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Image, CLSID_D2D12DAffineTransform, CLSID_D2D1Shadow,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_BUFFER_PRECISION_8BPC_UNORM,
    D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED, D2D1_COLOR_SPACE_SRGB,
    D2D1_EXTEND_MODE_CLAMP, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_PROPERTY_TYPE_FLOAT, D2D1_PROPERTY_TYPE_MATRIX_3X2, D2D1_PROPERTY_TYPE_VECTOR4,
    D2D1_ROUNDED_RECT, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION, D2D1_SHADOW_PROP_COLOR,
    D2D1_2DAFFINETRANSFORM_PROP_TRANSFORM_MATRIX,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows_numerics::Vector2;

/// A rendered surface bitmap plus the padding around the surface rect that
/// holds its shadow halo.
pub struct RenderedSurface {
    pub bitmap: ID2D1Bitmap1,
    /// Padding, in DIPs, between the bitmap edge and the surface rect. The
    /// caller places the bitmap at `(surface_x - margin, surface_y - margin)`.
    pub margin_dip: f32,
    pub width_dip: f32,
    pub height_dip: f32,
}

/// Gaussian standard deviation as a fraction of the blur radius. A Gaussian's
/// visible extent is roughly three standard deviations, so this keeps the
/// visible blur near the offset magnitude — the brief's target.
const STDDEV_FACTOR: f32 = 0.5;

fn color_f(c: Rgb, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a }
}

fn bitmap_props(dpi: f32) -> D2D1_BITMAP_PROPERTIES1 {
    D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: dpi,
        dpiY: dpi,
        bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
        colorContext: core::mem::ManuallyDrop::new(None),
    }
}

// SAFETY wrappers: each sets one typed effect property from its little-endian
// bytes. The property index/type pairs are exactly those documented for the
// Shadow and 2D Affine Transform effects.
unsafe fn set_f32(e: &windows::Win32::Graphics::Direct2D::ID2D1Effect, idx: u32, v: f32) {
    unsafe {
        let _ = e.SetValue(idx, D2D1_PROPERTY_TYPE_FLOAT, &v.to_le_bytes());
    }
}
unsafe fn set_vec4(
    e: &windows::Win32::Graphics::Direct2D::ID2D1Effect,
    idx: u32,
    v: [f32; 4],
) {
    let mut bytes = [0u8; 16];
    for (i, f) in v.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
    }
    unsafe {
        let _ = e.SetValue(idx, D2D1_PROPERTY_TYPE_VECTOR4, &bytes);
    }
}
unsafe fn set_translation(
    e: &windows::Win32::Graphics::Direct2D::ID2D1Effect,
    idx: u32,
    dx: f32,
    dy: f32,
) {
    // 3x2 affine: [M11 M12 M21 M22 M31 M32] with translation in M31/M32.
    let m = [1.0f32, 0.0, 0.0, 1.0, dx, dy];
    let mut bytes = [0u8; 24];
    for (i, f) in m.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
    }
    unsafe {
        let _ = e.SetValue(idx, D2D1_PROPERTY_TYPE_MATRIX_3X2, &bytes);
    }
}

/// Render a neumorphic surface into a fresh bitmap.
///
/// `ctx` must not be inside a `BeginDraw` — this function opens its own.
pub fn render_surface(
    ctx: &ID2D1DeviceContext,
    width_dip: f32,
    height_dip: f32,
    radius_dip: f32,
    elevation: Elevation,
    palette: &Palette,
    dpi_scale: f32,
) -> Result<RenderedSurface, RenderError> {
    let w = width_dip.max(1.0);
    let h = height_dip.max(1.0);
    let radius = clamp_radius(radius_dip, w, h);
    let params = shadow_params(elevation, w.min(h));

    // Halo extent: offset plus ~3 sigma, with a little slack.
    let stddev = params.blur_dip * STDDEV_FACTOR;
    let margin = (params.offset_dip + 3.0 * stddev + 4.0).ceil();

    let bmp_w = w + 2.0 * margin;
    let bmp_h = h + 2.0 * margin;
    let dpi = dpi_scale * 96.0;
    let size = D2D_SIZE_U {
        width: (bmp_w * dpi_scale).ceil() as u32,
        height: (bmp_h * dpi_scale).ceil() as u32,
    };

    // The surface rect within the bitmap, inset by the margin.
    let surf = D2D_RECT_F {
        left: margin,
        top: margin,
        right: margin + w,
        bottom: margin + h,
    };
    let rr = D2D1_ROUNDED_RECT { rect: surf, radiusX: radius, radiusY: radius };

    // SAFETY: `ctx` is a live device context. We save its current target and
    // restore it before returning, so switching targets here is transparent to
    // the caller. Each intermediate BeginDraw is matched by an EndDraw.
    unsafe {
        let previous = ctx.GetTarget().ok();

        // --- silhouette: an opaque rounded rect on transparent, for the shadow
        // effect to blur by alpha. Its colour is irrelevant; the shadow effect
        // recolours by its COLOR property. ---
        let props = bitmap_props(dpi);
        let silhouette = ctx.CreateBitmap(size, None, 0, &props)?;
        ctx.SetTarget(&silhouette);
        ctx.BeginDraw();
        ctx.Clear(Some(&color_f(Rgb::new(0.0, 0.0, 0.0), 0.0)));
        let white = ctx.CreateSolidColorBrush(&color_f(Rgb::new(1.0, 1.0, 1.0), 1.0), None)?;
        if !matches!(elevation, Elevation::Flat) {
            ctx.FillRoundedRectangle(&rr, &white);
        }
        ctx.EndDraw(None, None)?;

        // --- output: shadows, then the surface fill with its gradient on top. ---
        let output = ctx.CreateBitmap(size, None, 0, &props)?;
        ctx.SetTarget(&output);
        ctx.BeginDraw();
        ctx.Clear(Some(&color_f(Rgb::new(0.0, 0.0, 0.0), 0.0)));

        if !matches!(elevation, Elevation::Flat) {
            let (light_off, dark_off) = shadow_offsets(elevation, params.offset_dip);
            let sil_img: ID2D1Image = windows::core::Interface::cast(&silhouette)?;

            // Dark shadow first (down-right for raised), then light on top.
            for (color, alpha, (dx, dy)) in [
                (palette.shadow_dark, params.dark_alpha, dark_off),
                (palette.shadow_light, params.light_alpha, light_off),
            ] {
                let shadow = ctx.CreateEffect(&CLSID_D2D1Shadow)?;
                shadow.SetInput(0, &sil_img, true);
                set_f32(&shadow, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32, stddev);
                set_vec4(
                    &shadow,
                    D2D1_SHADOW_PROP_COLOR.0 as u32,
                    [color.r, color.g, color.b, alpha],
                );

                // Translate the (blurred) shadow via an affine transform, which
                // keeps the effect graph in the same coordinate space rather
                // than relying on a DrawImage offset.
                let affine = ctx.CreateEffect(&CLSID_D2D12DAffineTransform)?;
                let shadow_out = shadow.GetOutput()?;
                affine.SetInput(0, &shadow_out, true);
                set_translation(
                    &affine,
                    D2D1_2DAFFINETRANSFORM_PROP_TRANSFORM_MATRIX.0 as u32,
                    dx,
                    dy,
                );

                let out_img = affine.GetOutput()?;
                ctx.DrawImage(
                    &out_img,
                    None,
                    None,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    D2D1_COMPOSITE_MODE_SOURCE_OVER,
                );
            }
        }

        // Surface fill with the diagonal gradient on top of the halo.
        let (g_start, g_end) = surface_gradient(palette.base, elevation);
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: color_f(g_start, 1.0) },
            D2D1_GRADIENT_STOP { position: 1.0, color: color_f(g_end, 1.0) },
        ];
        let collection = ctx.CreateGradientStopCollection(
            &stops,
            D2D1_COLOR_SPACE_SRGB,
            D2D1_COLOR_SPACE_SRGB,
            D2D1_BUFFER_PRECISION_8BPC_UNORM,
            D2D1_EXTEND_MODE_CLAMP,
            D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
        )?;
        let grad_props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            // Top-left to bottom-right: the light source is at the top-left.
            startPoint: Vector2 { X: surf.left, Y: surf.top },
            endPoint: Vector2 { X: surf.right, Y: surf.bottom },
        };
        let brush = ctx.CreateLinearGradientBrush(&grad_props, None, &collection)?;
        ctx.FillRoundedRectangle(&rr, &brush);

        ctx.EndDraw(None, None)?;

        // Restore the caller's target.
        ctx.SetTarget(previous.as_ref());

        Ok(RenderedSurface { bitmap: output, margin_dip: margin, width_dip: w, height_dip: h })
    }
}

/// Blit a rendered surface so its surface rect's top-left lands at `(x, y)` in
/// DIPs. Call inside the caller's `BeginDraw`.
///
/// # Safety
/// `ctx` must be a live device context currently inside a `BeginDraw`.
pub unsafe fn draw_surface(ctx: &ID2D1DeviceContext, s: &RenderedSurface, x: f32, y: f32) {
    let img: Result<ID2D1Image, _> = windows::core::Interface::cast(&s.bitmap);
    if let Ok(img) = img {
        let offset = Vector2 { X: x - s.margin_dip, Y: y - s.margin_dip };
        unsafe {
            let _ = ctx.DrawImage(
                &img,
                Some(&offset),
                None,
                D2D1_INTERPOLATION_MODE_LINEAR,
                D2D1_COMPOSITE_MODE_SOURCE_OVER,
            );
        }
    }
}
