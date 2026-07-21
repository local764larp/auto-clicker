//! Borderless-shell geometry: hit regions and caption layout.
//!
//! Pure and platform-neutral. A borderless window has to re-implement
//! everything the system frame gave it for free — resize edges, caption drag,
//! and the maximize-button region that Snap Layouts hangs off. Getting any of
//! those rects wrong produces a window that cannot be resized from one edge,
//! which is exactly the kind of bug that survives casual testing.

/// Where a point lands on the custom shell.
///
/// Names mirror the `HT*` constants the wndproc translates them into, but this
/// enum carries no Win32 dependency so it can be tested anywhere.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HitRegion {
    Client,
    Caption,
    MinButton,
    MaxButton,
    CloseButton,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Shell metrics in DIPs. Converted to pixels with the window's DPI scale.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ShellMetrics {
    /// Grab width of the resize border.
    pub border_dip: f32,
    /// Height of the custom title bar.
    pub caption_height_dip: f32,
    /// Width of each caption button (minimise / maximise / close).
    pub button_width_dip: f32,
}

impl Default for ShellMetrics {
    fn default() -> Self {
        Self {
            // 8 DIP is roughly the system frame's grab area. Thinner reads as
            // fussy; the system default is what users' hands expect.
            border_dip: 8.0,
            caption_height_dip: 44.0,
            button_width_dip: 46.0,
        }
    }
}

impl ShellMetrics {
    pub fn border_px(&self, scale: f32) -> f32 {
        self.border_dip * scale
    }
    pub fn caption_height_px(&self, scale: f32) -> f32 {
        self.caption_height_dip * scale
    }
    pub fn button_width_px(&self, scale: f32) -> f32 {
        self.button_width_dip * scale
    }
}

/// Classify a client-space point.
///
/// `x`/`y` are client pixels; `w`/`h` the client size in pixels. `maximized`
/// suppresses the resize edges, because a maximized window is not resizable
/// and leaving live edges there makes the top of the screen feel snaggy.
pub fn hit_test(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    scale: f32,
    m: &ShellMetrics,
    maximized: bool,
) -> HitRegion {
    let border = m.border_px(scale);

    if !maximized {
        let left = x < border;
        let right = x >= w - border;
        let top = y < border;
        let bottom = y >= h - border;

        // Corners are checked first: they are the intersection, and testing
        // edges first would make corners unreachable.
        match (top, bottom, left, right) {
            (true, _, true, _) => return HitRegion::TopLeft,
            (true, _, _, true) => return HitRegion::TopRight,
            (_, true, true, _) => return HitRegion::BottomLeft,
            (_, true, _, true) => return HitRegion::BottomRight,
            (true, _, _, _) => return HitRegion::Top,
            (_, true, _, _) => return HitRegion::Bottom,
            (_, _, true, _) => return HitRegion::Left,
            (_, _, _, true) => return HitRegion::Right,
            _ => {}
        }
    }

    if y < m.caption_height_px(scale) {
        let bw = m.button_width_px(scale);
        // Buttons sit at the right edge, ordered minimise, maximise, close.
        if x >= w - bw {
            return HitRegion::CloseButton;
        }
        if x >= w - 2.0 * bw {
            // Snap Layouts attaches to this region. The wndproc must return
            // HTMAXBUTTON here or the flyout never appears.
            return HitRegion::MaxButton;
        }
        if x >= w - 3.0 * bw {
            return HitRegion::MinButton;
        }
        return HitRegion::Caption;
    }

    HitRegion::Client
}

impl HitRegion {
    /// True for regions the system handles itself once the wndproc reports
    /// them (drag, resize, Snap Layouts).
    pub fn is_system_managed(self) -> bool {
        !matches!(
            self,
            HitRegion::Client | HitRegion::MinButton | HitRegion::CloseButton
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 800.0;
    const H: f32 = 600.0;

    fn m() -> ShellMetrics {
        ShellMetrics::default()
    }

    fn hit(x: f32, y: f32) -> HitRegion {
        hit_test(x, y, W, H, 1.0, &m(), false)
    }

    #[test]
    fn centre_is_client() {
        assert_eq!(hit(400.0, 300.0), HitRegion::Client);
    }

    #[test]
    fn header_is_draggable_caption() {
        assert_eq!(hit(200.0, 20.0), HitRegion::Caption);
    }

    #[test]
    fn all_four_corners_are_reachable() {
        // Testing edges before corners is the classic mistake: it makes every
        // corner resolve to an edge and diagonal resize silently disappears.
        assert_eq!(hit(1.0, 1.0), HitRegion::TopLeft);
        assert_eq!(hit(W - 1.0, 1.0), HitRegion::TopRight);
        assert_eq!(hit(1.0, H - 1.0), HitRegion::BottomLeft);
        assert_eq!(hit(W - 1.0, H - 1.0), HitRegion::BottomRight);
    }

    #[test]
    fn all_four_edges_are_reachable() {
        assert_eq!(hit(400.0, 1.0), HitRegion::Top);
        assert_eq!(hit(400.0, H - 1.0), HitRegion::Bottom);
        assert_eq!(hit(1.0, 300.0), HitRegion::Left);
        assert_eq!(hit(W - 1.0, 300.0), HitRegion::Right);
    }

    #[test]
    fn caption_buttons_are_ordered_min_max_close_from_the_right() {
        let bw = m().button_width_dip;
        let y = m().caption_height_dip / 2.0;
        assert_eq!(hit(W - bw / 2.0, y), HitRegion::CloseButton);
        assert_eq!(hit(W - bw - bw / 2.0, y), HitRegion::MaxButton);
        assert_eq!(hit(W - 2.0 * bw - bw / 2.0, y), HitRegion::MinButton);
        assert_eq!(hit(W - 4.0 * bw, y), HitRegion::Caption);
    }

    #[test]
    fn maximize_button_is_reported_so_snap_layouts_can_attach() {
        // Snap Layouts only offers its flyout when the window reports
        // HTMAXBUTTON. Losing this is the usual way a custom title bar breaks
        // window management.
        let bw = m().button_width_dip;
        let r = hit(W - bw - 1.0, 10.0);
        assert_eq!(r, HitRegion::MaxButton);
        assert!(r.is_system_managed());
    }

    #[test]
    fn maximized_windows_have_no_resize_edges() {
        // A maximized window is not resizable; live edges there make the top
        // of the screen feel snaggy.
        let r = hit_test(1.0, 1.0, W, H, 1.0, &m(), true);
        assert_eq!(r, HitRegion::Caption);
        let r = hit_test(400.0, H - 1.0, W, H, 1.0, &m(), true);
        assert_eq!(r, HitRegion::Client);
    }

    #[test]
    fn resize_border_scales_with_dpi() {
        // At 200%, a point 12px in is still inside an 8-DIP border; at 100% it
        // is not. DPI bugs here are invisible on the developer's monitor.
        assert_eq!(hit_test(12.0, 300.0, W, H, 2.0, &m(), false), HitRegion::Left);
        assert_eq!(hit_test(12.0, 300.0, W, H, 1.0, &m(), false), HitRegion::Client);
    }

    #[test]
    fn caption_height_scales_with_dpi() {
        let y = m().caption_height_dip + 5.0;
        assert_eq!(hit_test(400.0, y, W, H, 1.0, &m(), false), HitRegion::Client);
        assert_eq!(hit_test(400.0, y, W, H, 2.0, &m(), false), HitRegion::Caption);
    }

    #[test]
    fn client_and_close_are_not_system_managed() {
        assert!(!HitRegion::Client.is_system_managed());
        assert!(!HitRegion::CloseButton.is_system_managed());
        assert!(HitRegion::Caption.is_system_managed());
        assert!(HitRegion::BottomRight.is_system_managed());
    }

    #[test]
    fn degenerate_window_sizes_do_not_panic() {
        for (w, h) in [(0.0, 0.0), (1.0, 1.0), (4.0, 4.0)] {
            let _ = hit_test(0.0, 0.0, w, h, 1.0, &m(), false);
            let _ = hit_test(w, h, w, h, 1.0, &m(), false);
        }
    }
}
