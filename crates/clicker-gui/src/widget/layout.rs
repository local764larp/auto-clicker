#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum WidgetId {
    StartStop,
    ModeToggle,
    RateSlider,
    IntervalField,
    CpsReadout,
}

/// Every widget id in layout/paint order.
pub const ALL_WIDGETS: [WidgetId; 5] = [
    WidgetId::CpsReadout,
    WidgetId::IntervalField,
    WidgetId::RateSlider,
    WidgetId::ModeToggle,
    WidgetId::StartStop,
];

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

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
        ALL_WIDGETS.into_iter().find(|&id| self.get(id).contains(px, py))
    }
}

/// Fixed vertical stack. The control set is small and static, so this is
/// positioned rects rather than a layout engine. All values in DIPs.
pub fn layout(client_w: f32, client_h: f32) -> WidgetRects {
    let _ = client_h;
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
        for id in ALL_WIDGETS {
            let r = l.get(id);
            assert!(r.w > 0.0 && r.h > 0.0, "{id:?} has an empty rect");
        }
    }

    #[test]
    fn widgets_do_not_overlap() {
        let l = layout(520.0, 680.0);
        for (i, a) in ALL_WIDGETS.iter().enumerate() {
            for b in &ALL_WIDGETS[i + 1..] {
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
        for id in ALL_WIDGETS {
            let r = l.get(id);
            assert!(r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= w && r.y + r.h <= h, "{id:?} out of bounds");
        }
    }
}
