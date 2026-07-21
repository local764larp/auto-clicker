//! Colour tokens and WCAG contrast math.
//!
//! Platform-neutral by design. Neumorphism's defining rule — surface and
//! background share one base colour, with depth carried entirely by paired
//! light and dark shadows — means interactive affordances have near-zero
//! contrast. The brief is explicit that accessibility must not lose that
//! argument, so the contrast floor is enforced by the tests at the bottom of
//! this file rather than asserted in prose.

/// sRGB colour with components in `0.0..=1.0`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Rgb {
    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }

    /// `0xRRGGBB`.
    pub fn from_hex(hex: u32) -> Self {
        Self {
            r: ((hex >> 16) & 0xFF) as f32 / 255.0,
            g: ((hex >> 8) & 0xFF) as f32 / 255.0,
            b: (hex & 0xFF) as f32 / 255.0,
        }
    }

    pub fn to_hex(self) -> u32 {
        let q = |c: f32| ((c.clamp(0.0, 1.0) * 255.0).round() as u32) & 0xFF;
        (q(self.r) << 16) | (q(self.g) << 8) | q(self.b)
    }

    /// Move toward white by `t` (0..=1).
    pub fn lighten(self, t: f32) -> Self {
        let f = |c: f32| c + (1.0 - c) * t;
        Self { r: f(self.r), g: f(self.g), b: f(self.b) }
    }

    /// Move toward black by `t` (0..=1).
    pub fn darken(self, t: f32) -> Self {
        let f = |c: f32| c * (1.0 - t);
        Self { r: f(self.r), g: f(self.g), b: f(self.b) }
    }
}

/// sRGB transfer function, inverse. Per WCAG 2.x / IEC 61966-2-1.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB transfer function.
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// WCAG relative luminance.
pub fn relative_luminance(c: Rgb) -> f32 {
    0.2126 * srgb_to_linear(c.r) + 0.7152 * srgb_to_linear(c.g) + 0.0722 * srgb_to_linear(c.b)
}

/// WCAG contrast ratio, always >= 1.0 and order-independent.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// WCAG AA minimum for normal-size body text.
pub const WCAG_AA_NORMAL: f32 = 4.5;
/// WCAG AA minimum for large text (>=18pt, or >=14pt bold).
pub const WCAG_AA_LARGE: f32 = 3.0;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

/// The full token set for one theme.
///
/// `base` is the single mid-tone shared by the window background and every
/// surface drawn on it. Shadows are derived from it so the pairing stays
/// consistent; one inconsistent element breaks the whole illusion.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Palette {
    pub theme: Theme,
    pub base: Rgb,
    /// Cast down-right by raised elements, up-left by inset ones.
    pub shadow_dark: Rgb,
    /// Cast up-left by raised elements, down-right by inset ones.
    pub shadow_light: Rgb,
    pub text_primary: Rgb,
    pub text_secondary: Rgb,
    /// Active/running state. Must read independently of the shadow treatment,
    /// because shadows convey nothing to a low-vision user.
    pub accent: Rgb,
}

impl Palette {
    pub fn light() -> Self {
        let base = Rgb::from_hex(0xE0E5EC);
        Self {
            theme: Theme::Light,
            base,
            shadow_dark: base.darken(0.28),
            shadow_light: base.lighten(0.85),
            text_primary: Rgb::from_hex(0x2E3239),
            text_secondary: Rgb::from_hex(0x4A5058),
            accent: Rgb::from_hex(0x1B5FA8),
        }
    }

    pub fn dark() -> Self {
        let base = Rgb::from_hex(0x2E3239);
        Self {
            theme: Theme::Dark,
            base,
            shadow_dark: base.darken(0.45),
            shadow_light: base.lighten(0.18),
            text_primary: Rgb::from_hex(0xE0E5EC),
            text_secondary: Rgb::from_hex(0xB4BCC6),
            accent: Rgb::from_hex(0x5AA9F0),
        }
    }

    pub fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Light => Self::light(),
            Theme::Dark => Self::dark(),
        }
    }

    /// Contrast of primary text against the surface it sits on.
    pub fn primary_contrast(&self) -> f32 {
        contrast_ratio(self.text_primary, self.base)
    }

    pub fn secondary_contrast(&self) -> f32 {
        contrast_ratio(self.text_secondary, self.base)
    }

    pub fn accent_contrast(&self) -> f32 {
        contrast_ratio(self.accent, self.base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn hex_round_trips() {
        for hex in [0x000000u32, 0xFFFFFF, 0xE0E5EC, 0x2E3239, 0x1B5FA8] {
            assert_eq!(Rgb::from_hex(hex).to_hex(), hex, "failed for {hex:#08X}");
        }
    }

    #[test]
    fn srgb_linear_round_trips() {
        for step in 0..=20 {
            let c = step as f32 / 20.0;
            let back = linear_to_srgb(srgb_to_linear(c));
            assert!(approx(c, back, 1e-4), "{c} -> {back}");
        }
    }

    #[test]
    fn luminance_matches_known_anchors() {
        assert!(approx(relative_luminance(Rgb::new(0.0, 0.0, 0.0)), 0.0, 1e-6));
        assert!(approx(relative_luminance(Rgb::new(1.0, 1.0, 1.0)), 1.0, 1e-6));
    }

    #[test]
    fn black_on_white_is_the_maximum_ratio() {
        let r = contrast_ratio(Rgb::new(0.0, 0.0, 0.0), Rgb::new(1.0, 1.0, 1.0));
        assert!(approx(r, 21.0, 0.01), "expected 21:1, got {r}");
    }

    #[test]
    fn contrast_is_order_independent() {
        let a = Rgb::from_hex(0xE0E5EC);
        let b = Rgb::from_hex(0x2E3239);
        assert!(approx(contrast_ratio(a, b), contrast_ratio(b, a), 1e-6));
    }

    #[test]
    fn identical_colours_have_unit_contrast() {
        let c = Rgb::from_hex(0xE0E5EC);
        assert!(approx(contrast_ratio(c, c), 1.0, 1e-6));
    }

    // --- The accessibility floor. These are the tests that matter. ---

    #[test]
    fn primary_text_clears_wcag_aa_in_both_themes() {
        for p in [Palette::light(), Palette::dark()] {
            let r = p.primary_contrast();
            assert!(
                r >= WCAG_AA_NORMAL,
                "{:?}: primary text contrast {r:.2}:1 is below the {WCAG_AA_NORMAL}:1 floor",
                p.theme
            );
        }
    }

    #[test]
    fn secondary_text_clears_wcag_aa_in_both_themes() {
        for p in [Palette::light(), Palette::dark()] {
            let r = p.secondary_contrast();
            assert!(
                r >= WCAG_AA_NORMAL,
                "{:?}: secondary text contrast {r:.2}:1 is below the {WCAG_AA_NORMAL}:1 floor",
                p.theme
            );
        }
    }

    #[test]
    fn accent_is_distinguishable_without_relying_on_shadows() {
        // The accent signals the running state. A low-vision user gets nothing
        // from the shadow treatment, so the colour must carry it alone.
        for p in [Palette::light(), Palette::dark()] {
            let r = p.accent_contrast();
            assert!(
                r >= WCAG_AA_NORMAL,
                "{:?}: accent contrast {r:.2}:1 is below the {WCAG_AA_NORMAL}:1 floor",
                p.theme
            );
        }
    }

    #[test]
    fn shadows_straddle_the_base_in_both_themes() {
        // Neumorphism needs a genuine light/dark pair. If both derived shadows
        // landed on the same side of the base the illusion collapses.
        for p in [Palette::light(), Palette::dark()] {
            let base = relative_luminance(p.base);
            let dark = relative_luminance(p.shadow_dark);
            let light = relative_luminance(p.shadow_light);
            assert!(dark < base, "{:?}: shadow_dark is not darker than base", p.theme);
            assert!(light > base, "{:?}: shadow_light is not lighter than base", p.theme);
        }
    }

    #[test]
    fn surfaces_share_the_base_colour_exactly() {
        // The defining rule. A surface tinted away from the background is not
        // neumorphism, so there is deliberately no separate "surface" token.
        assert_eq!(Palette::light().base, Rgb::from_hex(0xE0E5EC));
        assert_eq!(Palette::dark().base, Rgb::from_hex(0x2E3239));
    }

    #[test]
    fn lighten_and_darken_move_in_the_expected_direction() {
        let c = Rgb::from_hex(0x808080);
        assert!(relative_luminance(c.lighten(0.5)) > relative_luminance(c));
        assert!(relative_luminance(c.darken(0.5)) < relative_luminance(c));
        // Endpoints are saturating, not wrapping.
        assert_eq!(c.lighten(1.0).to_hex(), 0xFFFFFF);
        assert_eq!(c.darken(1.0).to_hex(), 0x000000);
    }
}
