//! Shadow geometry and surface gradients.
//!
//! Pure and platform-neutral, deliberately. These numbers decide whether the
//! neumorphic illusion holds, and a subtly wrong blur radius still "looks
//! fine" in isolation — so they are verified here, independently of Direct2D.
//! When a golden image later disagrees, the question is compositing, not maths.
//!
//! All lengths are device-independent pixels (DIPs). Nothing stores pixel
//! constants; scaling happens at draw time via [`ShadowParams::offset_px`].

use crate::render::color::Rgb;

/// Depth of a surface. The light source is fixed at the top-left, so `Raised`
/// casts light up-left and dark down-right, and `Inset` inverts both.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Elevation {
    Raised,
    Inset,
    Flat,
}

/// Offset as a fraction of the surface's shorter side.
const RAISED_OFFSET_RATIO: f32 = 0.06;
const INSET_OFFSET_RATIO: f32 = 0.045;

/// A tiny knob must not carry a huge shadow, and a full-width panel must not
/// carry a hairline one, so the ratio is clamped at both ends.
pub const RAISED_OFFSET_MIN_DIP: f32 = 2.0;
pub const RAISED_OFFSET_MAX_DIP: f32 = 10.0;
pub const INSET_OFFSET_MIN_DIP: f32 = 1.5;
pub const INSET_OFFSET_MAX_DIP: f32 = 7.0;

const RAISED_DARK_ALPHA: f32 = 0.42;
const RAISED_LIGHT_ALPHA: f32 = 0.90;
const INSET_DARK_ALPHA: f32 = 0.38;
const INSET_LIGHT_ALPHA: f32 = 0.80;

/// How far the surface gradient shifts from the base, as a lighten/darken
/// fraction. A few percent — enough to give the surface a direction, far short
/// of reading as a glossy button.
const GRADIENT_LIGHTEN: f32 = 0.055;
const GRADIENT_DARKEN: f32 = 0.035;

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ShadowParams {
    /// Offset magnitude along each axis, in DIPs. Sign is applied by
    /// [`shadow_offsets`].
    pub offset_dip: f32,
    /// Blur radius in DIPs. Kept equal to the offset magnitude.
    pub blur_dip: f32,
    pub dark_alpha: f32,
    pub light_alpha: f32,
}

impl ShadowParams {
    pub fn offset_px(&self, dpi_scale: f32) -> f32 {
        self.offset_dip * dpi_scale
    }
    pub fn blur_px(&self, dpi_scale: f32) -> f32 {
        self.blur_dip * dpi_scale
    }
}

/// Clamp a design radius so the rounded rect stays legal.
///
/// Direct2D degenerates when a corner radius exceeds half the shorter side.
/// Callers pass the design token (14–20 DIP); small controls get it reduced.
pub fn clamp_radius(radius_dip: f32, width_dip: f32, height_dip: f32) -> f32 {
    let limit = 0.5 * width_dip.min(height_dip);
    radius_dip.clamp(0.0, limit.max(0.0))
}

/// Derive shadow geometry from elevation and the surface's shorter side.
pub fn shadow_params(elevation: Elevation, min_side_dip: f32) -> ShadowParams {
    let side = min_side_dip.max(0.0);
    let (offset_dip, dark_alpha, light_alpha) = match elevation {
        Elevation::Raised => (
            (side * RAISED_OFFSET_RATIO).clamp(RAISED_OFFSET_MIN_DIP, RAISED_OFFSET_MAX_DIP),
            RAISED_DARK_ALPHA,
            RAISED_LIGHT_ALPHA,
        ),
        Elevation::Inset => (
            // Tighter than raised: a pressed element should read recessed
            // rather than merely inverted.
            (side * INSET_OFFSET_RATIO).clamp(INSET_OFFSET_MIN_DIP, INSET_OFFSET_MAX_DIP),
            INSET_DARK_ALPHA,
            INSET_LIGHT_ALPHA,
        ),
        Elevation::Flat => (0.0, 0.0, 0.0),
    };

    ShadowParams {
        offset_dip,
        // The brief calls for a blur roughly equal to the offset magnitude.
        // Equal is the defensible reading and cannot drift silently.
        blur_dip: offset_dip,
        dark_alpha,
        light_alpha,
    }
}

/// Signed `(light, dark)` offsets in DIPs, given an offset magnitude.
///
/// The light source is fixed top-left. Every raised element in the interface
/// must agree on this; one element lit from elsewhere breaks the whole effect.
pub fn shadow_offsets(elevation: Elevation, offset_dip: f32) -> ((f32, f32), (f32, f32)) {
    match elevation {
        Elevation::Raised => ((-offset_dip, -offset_dip), (offset_dip, offset_dip)),
        Elevation::Inset => ((offset_dip, offset_dip), (-offset_dip, -offset_dip)),
        Elevation::Flat => ((0.0, 0.0), (0.0, 0.0)),
    }
}

/// Gradient endpoints `(top_left, bottom_right)` for a surface.
///
/// A subtle diagonal gradient is what separates crisp neumorphism from a
/// blurry grey blob. Raised surfaces brighten toward the light source; inset
/// surfaces invert.
pub fn surface_gradient(base: Rgb, elevation: Elevation) -> (Rgb, Rgb) {
    match elevation {
        Elevation::Raised => (base.lighten(GRADIENT_LIGHTEN), base.darken(GRADIENT_DARKEN)),
        Elevation::Inset => (base.darken(GRADIENT_DARKEN), base.lighten(GRADIENT_LIGHTEN)),
        Elevation::Flat => (base, base),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::color::{relative_luminance, Rgb};

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    // --- geometry ---

    #[test]
    fn radius_is_clamped_to_half_the_shorter_side() {
        // A rounded rect whose radius exceeds half its shorter side degenerates
        // in Direct2D. Callers pass a design radius; this keeps it legal.
        assert!(approx(clamp_radius(18.0, 200.0, 40.0), 18.0, 1e-6));
        assert!(approx(clamp_radius(18.0, 200.0, 20.0), 10.0, 1e-6));
        assert!(approx(clamp_radius(18.0, 12.0, 200.0), 6.0, 1e-6));
    }

    #[test]
    fn radius_is_never_negative() {
        assert!(clamp_radius(-5.0, 100.0, 100.0) >= 0.0);
    }

    // --- shadow offsets and blur ---

    #[test]
    fn raised_offset_scales_with_surface_size() {
        let small = shadow_params(Elevation::Raised, 40.0);
        let large = shadow_params(Elevation::Raised, 300.0);
        assert!(
            large.offset_dip > small.offset_dip,
            "a larger surface should cast a larger shadow: {} vs {}",
            small.offset_dip,
            large.offset_dip
        );
    }

    #[test]
    fn offset_is_clamped_at_both_ends() {
        // A tiny knob must not get a huge shadow, and a full-width panel must
        // not get a hairline one.
        let tiny = shadow_params(Elevation::Raised, 4.0);
        let huge = shadow_params(Elevation::Raised, 4000.0);
        assert!(approx(tiny.offset_dip, RAISED_OFFSET_MIN_DIP, 1e-6), "{tiny:?}");
        assert!(approx(huge.offset_dip, RAISED_OFFSET_MAX_DIP, 1e-6), "{huge:?}");
    }

    #[test]
    fn blur_matches_offset_magnitude() {
        // The brief: blur radius roughly equal to the offset magnitude. Equal
        // is the defensible reading, and a ratio is easy to regress silently.
        for side in [40.0, 120.0, 400.0] {
            for e in [Elevation::Raised, Elevation::Inset] {
                let p = shadow_params(e, side);
                assert!(
                    approx(p.blur_dip, p.offset_dip, 1e-6),
                    "{e:?} at {side}: blur {} != offset {}",
                    p.blur_dip,
                    p.offset_dip
                );
            }
        }
    }

    #[test]
    fn inset_reads_tighter_than_raised() {
        // A pressed element should look recessed, not merely inverted; a
        // smaller offset is what sells that.
        let raised = shadow_params(Elevation::Raised, 120.0);
        let inset = shadow_params(Elevation::Inset, 120.0);
        assert!(
            inset.offset_dip < raised.offset_dip,
            "inset {} should be tighter than raised {}",
            inset.offset_dip,
            raised.offset_dip
        );
    }

    #[test]
    fn flat_has_no_shadow_at_all() {
        let p = shadow_params(Elevation::Flat, 120.0);
        assert_eq!(p.offset_dip, 0.0);
        assert_eq!(p.blur_dip, 0.0);
        assert_eq!(p.dark_alpha, 0.0);
        assert_eq!(p.light_alpha, 0.0);
    }

    #[test]
    fn offsets_are_monotonic_in_size() {
        let mut prev = 0.0f32;
        for side in [10.0f32, 20.0, 40.0, 80.0, 160.0, 320.0, 640.0] {
            let o = shadow_params(Elevation::Raised, side).offset_dip;
            assert!(o >= prev, "offset went backwards at side {side}: {o} < {prev}");
            prev = o;
        }
    }

    #[test]
    fn alphas_stay_in_unit_range() {
        for side in [4.0, 40.0, 400.0, 4000.0] {
            for e in [Elevation::Raised, Elevation::Inset, Elevation::Flat] {
                let p = shadow_params(e, side);
                assert!((0.0..=1.0).contains(&p.dark_alpha), "{e:?} {side}: {p:?}");
                assert!((0.0..=1.0).contains(&p.light_alpha), "{e:?} {side}: {p:?}");
            }
        }
    }

    // --- DPI ---

    #[test]
    fn dip_to_pixel_scaling_is_exact_at_common_scales() {
        let p = ShadowParams { offset_dip: 8.0, blur_dip: 8.0, dark_alpha: 0.5, light_alpha: 0.9 };
        assert!(approx(p.offset_px(1.0), 8.0, 1e-6));
        assert!(approx(p.offset_px(1.5), 12.0, 1e-6));
        assert!(approx(p.offset_px(2.0), 16.0, 1e-6));
        assert!(approx(p.blur_px(2.0), 16.0, 1e-6));
    }

    #[test]
    fn scaling_does_not_alter_alpha() {
        // Alpha is not a spatial quantity; scaling it would darken the UI on
        // high-DPI displays, which is a classic and invisible-at-100% bug.
        let p = shadow_params(Elevation::Raised, 120.0);
        let a = p.dark_alpha;
        let _ = p.offset_px(2.0);
        assert!(approx(p.dark_alpha, a, 1e-6));
    }

    // --- light direction ---

    #[test]
    fn raised_casts_light_up_left_and_dark_down_right() {
        let (light, dark) = shadow_offsets(Elevation::Raised, 6.0);
        assert_eq!(light, (-6.0, -6.0), "light must come from the top-left");
        assert_eq!(dark, (6.0, 6.0));
    }

    #[test]
    fn inset_inverts_the_pair() {
        let (light, dark) = shadow_offsets(Elevation::Inset, 4.0);
        assert_eq!(light, (4.0, 4.0));
        assert_eq!(dark, (-4.0, -4.0));
    }

    #[test]
    fn flat_offsets_are_zero() {
        let (light, dark) = shadow_offsets(Elevation::Flat, 6.0);
        assert_eq!(light, (0.0, 0.0));
        assert_eq!(dark, (0.0, 0.0));
    }

    // --- surface gradient ---

    #[test]
    fn raised_gradient_is_lighter_toward_the_light_source() {
        let base = Rgb::from_hex(0xE0E5EC);
        let (start, end) = surface_gradient(base, Elevation::Raised);
        assert!(
            relative_luminance(start) > relative_luminance(end),
            "raised surfaces should brighten toward top-left"
        );
    }

    #[test]
    fn inset_gradient_inverts() {
        let base = Rgb::from_hex(0xE0E5EC);
        let (start, end) = surface_gradient(base, Elevation::Inset);
        assert!(relative_luminance(start) < relative_luminance(end));
    }

    #[test]
    fn flat_gradient_is_uniform() {
        let base = Rgb::from_hex(0xE0E5EC);
        let (start, end) = surface_gradient(base, Elevation::Flat);
        assert_eq!(start, end);
        assert_eq!(start, base);
    }

    #[test]
    fn gradient_stays_subtle() {
        // A few percent. A strong gradient stops reading as neumorphism and
        // starts reading as a glossy button.
        let base = Rgb::from_hex(0xE0E5EC);
        let (start, end) = surface_gradient(base, Elevation::Raised);
        let delta = relative_luminance(start) - relative_luminance(end);
        assert!(delta > 0.0, "gradient must be visible");
        assert!(delta < 0.12, "gradient is too strong to read as neumorphic: {delta}");
    }

    #[test]
    fn gradient_works_on_the_dark_base_too() {
        // darken() on a near-black base can collapse to zero delta; the dark
        // theme must still show a direction.
        let base = Rgb::from_hex(0x2E3239);
        let (start, end) = surface_gradient(base, Elevation::Raised);
        assert!(
            relative_luminance(start) > relative_luminance(end),
            "dark theme lost its gradient direction"
        );
    }
}
