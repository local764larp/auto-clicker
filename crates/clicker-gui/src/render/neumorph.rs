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
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_DESTINATION_OUT,
    D2D1_COMPOSITE_MODE_SOURCE_IN, D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_GRADIENT_STOP,
    D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Image, CLSID_D2D12DAffineTransform, CLSID_D2D1Composite,
    CLSID_D2D1GaussianBlur, CLSID_D2D1Shadow, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_BUFFER_PRECISION_8BPC_UNORM, D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
    D2D1_COLOR_SPACE_SRGB, D2D1_COMPOSITE_PROP_MODE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_PROPERTY_TYPE_ENUM, D2D1_PROPERTY_TYPE_FLOAT,
    D2D1_PROPERTY_TYPE_MATRIX_3X2, D2D1_PROPERTY_TYPE_VECTOR4, D2D1_ROUNDED_RECT,
    D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION, D2D1_SHADOW_PROP_COLOR,
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
unsafe fn set_enum(e: &windows::Win32::Graphics::Direct2D::ID2D1Effect, idx: u32, v: u32) {
    unsafe {
        let _ = e.SetValue(idx, D2D1_PROPERTY_TYPE_ENUM, &v.to_le_bytes());
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

        let sil_img: ID2D1Image = windows::core::Interface::cast(&silhouette)?;
        let (light_off, dark_off) = shadow_offsets(elevation, params.offset_dip);
        let whole = D2D_RECT_F { left: 0.0, top: 0.0, right: bmp_w, bottom: bmp_h };

        // Inset needs inverted, tinted masks built up front (their own draw
        // session), because the inner shadow is the shape's alpha inverted,
        // blurred, and clipped back inside — there is no built-in effect for it.
        let inset_masks = if matches!(elevation, Elevation::Inset) {
            let dark = inverted_tint(
                ctx, size, dpi, &whole, &sil_img, palette.shadow_dark, params.dark_alpha,
            )?;
            let light = inverted_tint(
                ctx, size, dpi, &whole, &sil_img, palette.shadow_light, params.light_alpha,
            )?;
            Some((dark, light))
        } else {
            None
        };

        // --- output: raised shadows go beneath the fill; inset shadows above. ---
        let output = ctx.CreateBitmap(size, None, 0, &props)?;
        ctx.SetTarget(&output);
        ctx.BeginDraw();
        ctx.Clear(Some(&color_f(Rgb::new(0.0, 0.0, 0.0), 0.0)));

        // Raised: outer dual shadow, dark down-right then light up-left, drawn
        // BENEATH the surface so only the halo shows around the edges.
        if matches!(elevation, Elevation::Raised) {
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

                // Translate via an affine transform so the effect graph stays in
                // one coordinate space rather than relying on a DrawImage offset.
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

        // Surface fill with the diagonal gradient.
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

        // Inset: inner shadows drawn ON TOP of the fill, dark up-left and light
        // down-right, each clipped to the shape interior.
        if let Some((dark, light)) = &inset_masks {
            for (mask, (dx, dy)) in [(dark, dark_off), (light, light_off)] {
                let mask_img: ID2D1Image = windows::core::Interface::cast(mask)?;

                // Inner shadows invert the translation of outer ones: the mask
                // is a hole, so shifting it down-right leaves the opaque spill
                // in the TOP-LEFT interior. To land the dark shadow where
                // `dark_off` points (top-left), the mask moves the other way.
                let affine = ctx.CreateEffect(&CLSID_D2D12DAffineTransform)?;
                affine.SetInput(0, &mask_img, true);
                set_translation(
                    &affine,
                    D2D1_2DAFFINETRANSFORM_PROP_TRANSFORM_MATRIX.0 as u32,
                    -dx,
                    -dy,
                );

                let blur = ctx.CreateEffect(&CLSID_D2D1GaussianBlur)?;
                let aff_out = affine.GetOutput()?;
                blur.SetInput(0, &aff_out, true);
                set_f32(&blur, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32, stddev);

                // Clip the blurred spill back inside the shape: keep the blur
                // (input 0) only where the silhouette (input 1) is opaque.
                let comp = ctx.CreateEffect(&CLSID_D2D1Composite)?;
                let blur_out = blur.GetOutput()?;
                comp.SetInput(0, &blur_out, true);
                comp.SetInput(1, &sil_img, true);
                set_enum(
                    &comp,
                    D2D1_COMPOSITE_PROP_MODE.0 as u32,
                    D2D1_COMPOSITE_MODE_SOURCE_IN.0 as u32,
                );

                let comp_out = comp.GetOutput()?;
                ctx.DrawImage(
                    &comp_out,
                    None,
                    None,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    D2D1_COMPOSITE_MODE_SOURCE_OVER,
                );
            }
        }

        ctx.EndDraw(None, None)?;

        // Restore the caller's target.
        ctx.SetTarget(previous.as_ref());

        Ok(RenderedSurface { bitmap: output, margin_dip: margin, width_dip: w, height_dip: h })
    }
}

/// Build an inverted, tinted alpha mask: opaque `tint` at `alpha` everywhere
/// outside the shape, transparent inside. Used as the seed for an inner shadow.
///
/// # Safety
/// `ctx` must be a live device context not currently inside a `BeginDraw`.
unsafe fn inverted_tint(
    ctx: &ID2D1DeviceContext,
    size: D2D_SIZE_U,
    dpi: f32,
    whole: &D2D_RECT_F,
    silhouette: &ID2D1Image,
    tint: Rgb,
    alpha: f32,
) -> Result<ID2D1Bitmap1, RenderError> {
    let props = bitmap_props(dpi);
    // SAFETY: caller contract — ctx is live and not mid-draw. The intermediate
    // BeginDraw is matched by EndDraw; the previous target is the caller's
    // responsibility to restore (render_surface does).
    unsafe {
        let bmp = ctx.CreateBitmap(size, None, 0, &props)?;
        ctx.SetTarget(&bmp);
        ctx.BeginDraw();
        ctx.Clear(Some(&color_f(Rgb::new(0.0, 0.0, 0.0), 0.0)));
        // Flood the tint, then punch the shape out with DESTINATION_OUT.
        let brush = ctx.CreateSolidColorBrush(&color_f(tint, alpha), None)?;
        ctx.FillRectangle(whole, &brush);
        ctx.DrawImage(
            silhouette,
            None,
            None,
            D2D1_INTERPOLATION_MODE_LINEAR,
            D2D1_COMPOSITE_MODE_DESTINATION_OUT,
        );
        ctx.EndDraw(None, None)?;
        Ok(bmp)
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

#[cfg(all(test, windows))]
mod golden {
    //! Deterministic golden checks on WARP.
    //!
    //! WARP is bit-identical across machines, so these render offscreen and
    //! assert the neumorphic invariants directly on the pixels — light comes
    //! from the top-left, raised casts an outer halo, inset a recessed inner
    //! shadow, flat none. Property assertions rather than stored PNGs: they need
    //! no bless step, give a legible failure, and encode the design rules that
    //! actually regress. The inset direction bug this module would have caught
    //! was found by eye during development; now it is caught automatically.

    use super::*;
    use crate::render::color::{Palette, Rgb, Theme};
    use crate::render::device::{create_context, DriverKind};
    use windows::Win32::Graphics::Direct2D::{
        D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ, D2D1_MAP_OPTIONS_READ,
    };

    struct Image {
        w: usize,
        h: usize,
        /// Straight BGRA, row-major, tightly packed.
        px: Vec<u8>,
    }

    impl Image {
        /// Perceptual luminance at a pixel, 0..255.
        fn lum(&self, x: usize, y: usize) -> f32 {
            let i = (y * self.w + x) * 4;
            let b = self.px[i] as f32;
            let g = self.px[i + 1] as f32;
            let r = self.px[i + 2] as f32;
            0.2126 * r + 0.7152 * g + 0.0722 * b
        }
    }

    /// Render a surface on WARP over an opaque base, read the pixels back.
    ///
    /// Compositing over an opaque base keeps every sample opaque, so luminance
    /// comparisons are clean rather than tangled with premultiplied alpha.
    fn render_over_base(elevation: Elevation, palette: &Palette) -> (Image, f32, usize) {
        let (_d3d, ctx) = create_context(DriverKind::Warp).expect("WARP context");
        let side = 100.0f32;

        let surf = render_surface(&ctx, side, side, 16.0, elevation, palette, 1.0)
            .expect("render_surface");
        let margin = surf.margin_dip;

        // SAFETY: WARP context is live; every bitmap and draw call below is
        // matched and the staging bitmap is mapped/unmapped in pairs.
        unsafe {
            let bw = (side + 2.0 * margin).ceil() as u32;
            let bh = bw;

            let props = bitmap_props(96.0);
            let target = ctx.CreateBitmap(D2D_SIZE_U { width: bw, height: bh }, None, 0, &props)
                .expect("target");
            ctx.SetTarget(&target);
            ctx.BeginDraw();
            ctx.Clear(Some(&color_f(palette.base, 1.0)));
            draw_surface(&ctx, &surf, margin, margin);
            ctx.EndDraw(None, None).expect("enddraw");

            // Stage a CPU-readable copy.
            let mut sprops = bitmap_props(96.0);
            sprops.bitmapOptions = D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW;
            let staging = ctx
                .CreateBitmap(D2D_SIZE_U { width: bw, height: bh }, None, 0, &sprops)
                .expect("staging");
            staging.CopyFromBitmap(None, &target, None).expect("copy");

            let mapped = staging.Map(D2D1_MAP_OPTIONS_READ).expect("map");
            let mut px = vec![0u8; (bw * bh * 4) as usize];
            for row in 0..bh as usize {
                let src = mapped.bits.add(row * mapped.pitch as usize);
                let dst = &mut px[row * bw as usize * 4..(row + 1) * bw as usize * 4];
                core::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), bw as usize * 4);
            }
            staging.Unmap().expect("unmap");
            ctx.SetTarget(None);

            (Image { w: bw as usize, h: bh as usize, px }, margin, side as usize)
        }
    }

    #[test]
    fn raised_casts_light_up_left_and_dark_down_right() {
        for palette in [Palette::light(), Palette::dark()] {
            let (img, m, side) = render_over_base(Elevation::Raised, &palette);
            let mi = m as usize;
            let mid = mi + side / 2;
            let base = img.lum(mid, mid);

            // Exterior halo, sampled just outside the middle of each edge.
            let top = img.lum(mid, mi.saturating_sub(6));
            let bottom = img.lum(mid, mi + side + 6);

            assert!(
                top > base + 2.0,
                "{:?}: top halo {top:.1} should be lighter than base {base:.1}",
                palette.theme
            );
            assert!(
                bottom < base - 2.0,
                "{:?}: bottom halo {bottom:.1} should be darker than base {base:.1}",
                palette.theme
            );
            assert!(top > bottom, "{:?}: light must be up, dark down", palette.theme);
        }
    }

    #[test]
    fn inset_recesses_dark_at_top_left() {
        for palette in [Palette::light(), Palette::dark()] {
            let (img, m, side) = render_over_base(Elevation::Inset, &palette);
            let mi = m as usize;

            // Interior, near opposite corners.
            let tl = img.lum(mi + 12, mi + 12);
            let br = img.lum(mi + side - 12, mi + side - 12);
            assert!(
                tl < br,
                "{:?}: inset top-left interior {tl:.1} should be darker than bottom-right {br:.1}",
                palette.theme
            );

            // And no outer halo: just outside the edge is essentially the base.
            let base = img.lum(mi + side / 2, mi + side / 2);
            let outside = img.lum(mi + side / 2, mi + side + 8);
            assert!(
                (outside - base).abs() < 12.0,
                "{:?}: inset should not cast an outer halo (outside {outside:.1} vs base {base:.1})",
                palette.theme
            );
        }
    }

    #[test]
    fn flat_has_no_depth() {
        let palette = Palette::light();
        let (img, m, side) = render_over_base(Elevation::Flat, &palette);
        let mi = m as usize;
        let base = img.lum(mi + side / 2, mi + side / 2);

        // Every corner and edge of the interior sits at the base luminance.
        for (x, y) in [
            (mi + 8, mi + 8),
            (mi + side - 8, mi + 8),
            (mi + 8, mi + side - 8),
            (mi + side - 8, mi + side - 8),
        ] {
            let l = img.lum(x, y);
            assert!(
                (l - base).abs() < 4.0,
                "flat should be uniform: corner {l:.1} vs centre {base:.1}"
            );
        }
    }

    #[test]
    fn warp_output_is_reproducible() {
        // The premise of using WARP for goldens: two renders are identical.
        let p = Palette::for_theme(Theme::Light);
        let a = render_over_base(Elevation::Raised, &p).0;
        let b = render_over_base(Elevation::Raised, &p).0;
        assert_eq!(a.px, b.px, "WARP render was not bit-reproducible");
    }
}
