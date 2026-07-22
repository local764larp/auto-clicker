/// Tab-order ring over widget ids. `next`/`prev` wrap and skip disabled ids;
/// the accent focus ring the renderer draws on `focused()` is doing real
/// accessibility work under neumorphism's low contrast, so this is not optional.
pub struct FocusRing<T> {
    order: Vec<(T, bool)>,
    current: Option<usize>,
}

impl<T: Copy + PartialEq> FocusRing<T> {
    pub fn new(order: Vec<(T, bool)>) -> Self {
        Self { order, current: None }
    }

    pub fn focused(&self) -> Option<T> {
        self.current.map(|i| self.order[i].0)
    }

    fn step(&mut self, forward: bool) {
        let n = self.order.len();
        if n == 0 || !self.order.iter().any(|(_, en)| *en) {
            self.current = None;
            return;
        }
        let start = self.current.unwrap_or(if forward { n - 1 } else { 0 });
        for k in 1..=n {
            let i = if forward {
                (start + k) % n
            } else {
                (start + n - k) % n
            };
            if self.order[i].1 {
                self.current = Some(i);
                return;
            }
        }
    }

    pub fn next(&mut self) {
        self.step(true);
    }
    pub fn prev(&mut self) {
        self.step(false);
    }

    pub fn focus(&mut self, id: T) {
        if let Some(i) = self.order.iter().position(|(t, en)| *t == id && *en) {
            self.current = Some(i);
        }
    }

    pub fn set_enabled(&mut self, id: T, enabled: bool) {
        if let Some(e) = self.order.iter_mut().find(|(t, _)| *t == id) {
            e.1 = enabled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> FocusRing<u32> {
        FocusRing::new(vec![(0, true), (1, true), (2, true)])
    }

    #[test]
    fn starts_unfocused() {
        assert_eq!(ring().focused(), None);
    }

    #[test]
    fn next_advances_and_wraps() {
        let mut r = ring();
        r.next();
        assert_eq!(r.focused(), Some(0));
        r.next();
        assert_eq!(r.focused(), Some(1));
        r.next();
        r.next();
        assert_eq!(r.focused(), Some(0), "should wrap");
    }

    #[test]
    fn prev_retreats_and_wraps() {
        let mut r = ring();
        r.next();
        r.prev();
        assert_eq!(r.focused(), Some(2), "prev from first wraps to last");
    }

    #[test]
    fn skips_disabled() {
        let mut r = FocusRing::new(vec![(0, true), (1, false), (2, true)]);
        r.next();
        assert_eq!(r.focused(), Some(0));
        r.next();
        assert_eq!(r.focused(), Some(2), "must skip the disabled one");
    }

    #[test]
    fn all_disabled_focuses_nothing() {
        let mut r = FocusRing::new(vec![(0, false), (1, false)]);
        r.next();
        assert_eq!(r.focused(), None);
    }
}
