use crate::shared::Button;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SinkError {
    /// Fewer events were accepted than submitted. Most often UIPI refusing to
    /// inject into a higher-integrity target, or a locked workstation /
    /// secure desktop. Never treat this as a silent no-op.
    Blocked { inserted: u32, expected: u32, last_error: u32 },
}

impl core::fmt::Display for SinkError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SinkError::Blocked { inserted, expected, last_error } => write!(
                f,
                "input blocked: {inserted}/{expected} events accepted (GetLastError={last_error}). \
                 The target window may run at a higher integrity level; running this tool \
                 elevated would be required to click it."
            ),
        }
    }
}

impl std::error::Error for SinkError {}

pub trait ClickSink {
    /// Emit `count` complete clicks. Returns how many were emitted.
    fn emit_batch(
        &mut self,
        button: Button,
        pos: Option<(i32, i32)>,
        count: u16,
    ) -> Result<u16, SinkError>;

    fn emit(&mut self, button: Button, pos: Option<(i32, i32)>) -> Result<(), SinkError> {
        self.emit_batch(button, pos, 1).map(|_| ())
    }

    /// Press the button down without releasing. Paired with [`ClickSink::release`]
    /// by the engine's duty-cycle path so the button is held for a fraction of
    /// the interval. The default is a full click (no real hold), which lets
    /// sinks that do not support holding degrade gracefully.
    fn press(&mut self, button: Button, pos: Option<(i32, i32)>) -> Result<(), SinkError> {
        self.emit_batch(button, pos, 1).map(|_| ())
    }

    /// Release a previously pressed button. Default: nothing (the default
    /// `press` already completed the click).
    fn release(&mut self, _button: Button) -> Result<(), SinkError> {
        Ok(())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RecordedClick {
    pub button: Button,
    pub pos: Option<(i32, i32)>,
    pub at_ns: u64,
}

/// Test sink. Timestamps every call against a `VirtualClock` and can model
/// per-emit syscall cost, so engine tests see realistic time advancement.
#[derive(Debug)]
pub struct RecordingSink {
    clock: std::sync::Arc<crate::clock::VirtualClock>,
    cost_ns: u64,
    clicks: Vec<RecordedClick>,
    fail_after: Option<u64>,
    total: u64,
    pending_down: Option<(Button, Option<(i32, i32)>, u64)>,
    holds: Vec<u64>,
}

impl RecordingSink {
    pub fn new(clock: std::sync::Arc<crate::clock::VirtualClock>) -> Self {
        Self::with_cost(clock, 0)
    }

    pub fn with_cost(clock: std::sync::Arc<crate::clock::VirtualClock>, cost_ns: u64) -> Self {
        Self {
            clock,
            cost_ns,
            clicks: Vec::new(),
            fail_after: None,
            total: 0,
            pending_down: None,
            holds: Vec::new(),
        }
    }

    /// Durations, in ns, that the button was held between press and release
    /// (duty-cycle path only). Empty for the fast batched path.
    pub fn holds(&self) -> &[u64] {
        &self.holds
    }

    /// After `n` successful clicks, every subsequent emit returns `Blocked`.
    pub fn fail_after(&mut self, n: u64) {
        self.fail_after = Some(n);
    }

    pub fn clicks(&self) -> &[RecordedClick] {
        &self.clicks
    }

    pub fn total(&self) -> u64 {
        self.total
    }
}

impl ClickSink for RecordingSink {
    fn emit_batch(
        &mut self,
        button: Button,
        pos: Option<(i32, i32)>,
        count: u16,
    ) -> Result<u16, SinkError> {
        use crate::clock::Clock;
        for _ in 0..count {
            if let Some(limit) = self.fail_after {
                if self.total >= limit {
                    return Err(SinkError::Blocked {
                        inserted: 0,
                        expected: count as u32 * 2,
                        last_error: 5, // ERROR_ACCESS_DENIED
                    });
                }
            }
            self.clicks.push(RecordedClick { button, pos, at_ns: self.clock.now_ns() });
            self.total += 1;
            if self.cost_ns > 0 {
                self.clock.advance(self.cost_ns);
            }
        }
        Ok(count)
    }

    fn press(&mut self, button: Button, pos: Option<(i32, i32)>) -> Result<(), SinkError> {
        use crate::clock::Clock;
        if let Some(limit) = self.fail_after {
            if self.total >= limit {
                return Err(SinkError::Blocked { inserted: 0, expected: 2, last_error: 5 });
            }
        }
        self.pending_down = Some((button, pos, self.clock.now_ns()));
        Ok(())
    }

    fn release(&mut self, _button: Button) -> Result<(), SinkError> {
        use crate::clock::Clock;
        if let Some((button, pos, down_ns)) = self.pending_down.take() {
            let now = self.clock.now_ns();
            self.clicks.push(RecordedClick { button, pos, at_ns: down_ns });
            self.holds.push(now.saturating_sub(down_ns));
            self.total += 1;
            if self.cost_ns > 0 {
                self.clock.advance(self.cost_ns);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Clock, VirtualClock};
    use std::sync::Arc;

    #[test]
    fn records_button_position_and_time() {
        let clock = Arc::new(VirtualClock::new(500));
        let mut s = RecordingSink::new(clock.clone());
        s.emit(Button::Right, Some((10, 20))).unwrap();
        assert_eq!(s.clicks().len(), 1);
        assert_eq!(s.clicks()[0].button, Button::Right);
        assert_eq!(s.clicks()[0].pos, Some((10, 20)));
        assert_eq!(s.clicks()[0].at_ns, 500);
    }

    #[test]
    fn batch_records_each_click_and_returns_the_count() {
        let clock = Arc::new(VirtualClock::new(0));
        let mut s = RecordingSink::new(clock.clone());
        assert_eq!(s.emit_batch(Button::Left, None, 4).unwrap(), 4);
        assert_eq!(s.clicks().len(), 4);
        assert_eq!(s.total(), 4);
    }

    #[test]
    fn per_emit_cost_advances_the_clock() {
        let clock = Arc::new(VirtualClock::new(0));
        let mut s = RecordingSink::with_cost(clock.clone(), 1_000);
        s.emit_batch(Button::Left, None, 3).unwrap();
        assert_eq!(clock.now_ns(), 3_000);
    }

    #[test]
    fn fail_after_reports_a_blocked_error() {
        let clock = Arc::new(VirtualClock::new(0));
        let mut s = RecordingSink::new(clock.clone());
        s.fail_after(2);
        assert!(s.emit(Button::Left, None).is_ok());
        assert!(s.emit(Button::Left, None).is_ok());
        let err = s.emit(Button::Left, None).unwrap_err();
        assert!(matches!(err, SinkError::Blocked { .. }));
    }

    #[test]
    fn zero_count_batch_is_a_noop() {
        let clock = Arc::new(VirtualClock::new(0));
        let mut s = RecordingSink::new(clock.clone());
        assert_eq!(s.emit_batch(Button::Left, None, 0).unwrap(), 0);
        assert_eq!(s.clicks().len(), 0);
    }
}
