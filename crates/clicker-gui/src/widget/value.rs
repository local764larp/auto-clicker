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
        Ok(v) => v.clamp(min as u64, max as u64) as u32,
        Err(_) => max, // only reachable on overflow, since all bytes are digits
    }
}

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
        assert_eq!(parse_and_clamp("", 1, 4000), 1);
        assert_eq!(parse_and_clamp("12x9", 1, 4000), 1);
        assert_eq!(parse_and_clamp("99999", 1, 4000), 4000);
        assert_eq!(parse_and_clamp("0", 1, 4000), 1);
    }

    #[test]
    fn parse_saturates_rather_than_overflowing() {
        let huge = "999999999999999999999999";
        assert_eq!(parse_and_clamp(huge, 1, 4000), 4000);
    }
}
