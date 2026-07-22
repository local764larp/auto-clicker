use core::sync::atomic::{
    AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum Button {
    Left = 0,
    Middle = 1,
    Right = 2,
}

impl Button {
    /// Saturating conversion. A corrupt byte degrades to `Left` rather than
    /// panicking — this runs in the hot loop and must never fail.
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Button::Middle,
            2 => Button::Right,
            _ => Button::Left,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum PositionMode {
    FollowCursor = 0,
    FixedPoint = 1,
}

impl PositionMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => PositionMode::FixedPoint,
            _ => PositionMode::FollowCursor,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EngineState {
    Idle = 0,
    Running = 1,
    StoppedByLimit = 2,
    Error = 3,
}

impl EngineState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => EngineState::Running,
            2 => EngineState::StoppedByLimit,
            3 => EngineState::Error,
            _ => EngineState::Idle,
        }
    }
}

/// One consistent read of every config field, taken once per loop iteration.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub interval_ns: u64,
    pub button: Button,
    pub position: Option<(i32, i32)>,
    pub limit_clicks: u64,
    pub limit_ns: u64,
    pub batch_size: u16,
    /// Fraction of the interval the button is held down, in percent (0 = a
    /// press/release with no hold). Only honoured at `batch_size == 1`.
    pub duty_pct: u8,
    /// Interval jitter, in percent. Each period is scaled by a random factor in
    /// `[1 - r, 1 + r]`. 0 = perfectly regular.
    pub randomize_pct: u8,
    /// 0 = click a mouse button, 1 = press a keyboard key (`key_vk`).
    pub click_kind: u8,
    /// Virtual-key pressed in keyboard mode.
    pub key_vk: u32,
}

/// Shared between the GUI/host thread and the engine thread. Every field is an
/// atomic; there is no lock anywhere in this type by design.
#[derive(Debug)]
pub struct SharedState {
    running: AtomicBool,
    shutdown: AtomicBool,
    interval_ns: AtomicU64,
    button: AtomicU8,
    position_mode: AtomicU8,
    fixed_x: AtomicI32,
    fixed_y: AtomicI32,
    limit_clicks: AtomicU64,
    limit_ns: AtomicU64,
    clicks_emitted: AtomicU64,
    engine_state: AtomicU8,
    batch_size: AtomicU16,
    duty_pct: AtomicU8,
    randomize_pct: AtomicU8,
    click_kind: AtomicU8,
    key_vk: AtomicU32,
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedState {
    pub const fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            interval_ns: AtomicU64::new(10_000_000), // 100 CPS
            button: AtomicU8::new(Button::Left as u8),
            position_mode: AtomicU8::new(PositionMode::FollowCursor as u8),
            fixed_x: AtomicI32::new(0),
            fixed_y: AtomicI32::new(0),
            limit_clicks: AtomicU64::new(0),
            limit_ns: AtomicU64::new(0),
            clicks_emitted: AtomicU64::new(0),
            engine_state: AtomicU8::new(EngineState::Idle as u8),
            batch_size: AtomicU16::new(1),
            duty_pct: AtomicU8::new(0),
            randomize_pct: AtomicU8::new(0),
            click_kind: AtomicU8::new(0),
            key_vk: AtomicU32::new(0x20), // Space
        }
    }

    /// Single `Relaxed` read of every config field. These are hints that may be
    /// one click stale, which is acceptable and cheaper than acquiring.
    pub fn snapshot(&self) -> Config {
        let mode = PositionMode::from_u8(self.position_mode.load(Ordering::Relaxed));
        let position = match mode {
            PositionMode::FollowCursor => None,
            PositionMode::FixedPoint => Some((
                self.fixed_x.load(Ordering::Relaxed),
                self.fixed_y.load(Ordering::Relaxed),
            )),
        };
        Config {
            interval_ns: self.interval_ns.load(Ordering::Relaxed),
            button: Button::from_u8(self.button.load(Ordering::Relaxed)),
            position,
            limit_clicks: self.limit_clicks.load(Ordering::Relaxed),
            limit_ns: self.limit_ns.load(Ordering::Relaxed),
            batch_size: self.batch_size.load(Ordering::Relaxed),
            duty_pct: self.duty_pct.load(Ordering::Relaxed),
            randomize_pct: self.randomize_pct.load(Ordering::Relaxed),
            click_kind: self.click_kind.load(Ordering::Relaxed),
            key_vk: self.key_vk.load(Ordering::Relaxed),
        }
    }

    // --- ordered flags: Acquire/Release carries meaning here ---

    pub fn running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
    pub fn set_running(&self, v: bool) {
        self.running.store(v, Ordering::Release);
    }
    pub fn shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }
    pub fn engine_state(&self) -> EngineState {
        EngineState::from_u8(self.engine_state.load(Ordering::Acquire))
    }
    pub fn set_engine_state(&self, s: EngineState) {
        self.engine_state.store(s as u8, Ordering::Release);
    }

    // --- counter: engine is the sole writer, GUI is a pure reader ---

    pub fn clicks_emitted(&self) -> u64 {
        self.clicks_emitted.load(Ordering::Relaxed)
    }
    /// Plain store, never `fetch_add`. The engine owns this value; a locked
    /// read-modify-write per click is wasted work at ceiling rates.
    pub fn store_clicks(&self, v: u64) {
        self.clicks_emitted.store(v, Ordering::Relaxed);
    }

    // --- config setters ---

    pub fn set_interval_ns(&self, v: u64) {
        self.interval_ns.store(v, Ordering::Relaxed);
    }
    pub fn set_button(&self, b: Button) {
        self.button.store(b as u8, Ordering::Relaxed);
    }
    pub fn set_position_mode(&self, m: PositionMode) {
        self.position_mode.store(m as u8, Ordering::Relaxed);
    }
    pub fn set_fixed_point(&self, x: i32, y: i32) {
        self.fixed_x.store(x, Ordering::Relaxed);
        self.fixed_y.store(y, Ordering::Relaxed);
    }
    pub fn set_limit_clicks(&self, v: u64) {
        self.limit_clicks.store(v, Ordering::Relaxed);
    }
    pub fn set_limit_ns(&self, v: u64) {
        self.limit_ns.store(v, Ordering::Relaxed);
    }
    pub fn set_batch_size(&self, v: u16) {
        self.batch_size.store(v.max(1), Ordering::Relaxed);
    }
    pub fn set_duty_pct(&self, v: u8) {
        self.duty_pct.store(v.min(95), Ordering::Relaxed);
    }
    pub fn set_randomize_pct(&self, v: u8) {
        self.randomize_pct.store(v.min(95), Ordering::Relaxed);
    }
    pub fn set_click_kind(&self, v: u8) {
        self.click_kind.store(v, Ordering::Relaxed);
    }
    pub fn set_key_vk(&self, v: u32) {
        self.key_vk.store(v, Ordering::Relaxed);
    }
    pub fn key_vk(&self) -> u32 {
        self.key_vk.load(Ordering::Relaxed)
    }
    pub fn click_kind(&self) -> u8 {
        self.click_kind.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reflects_writes() {
        let s = SharedState::new();
        s.set_interval_ns(1_000_000);
        s.set_button(Button::Right);
        s.set_position_mode(PositionMode::FixedPoint);
        s.set_fixed_point(640, 480);
        s.set_limit_clicks(50);
        s.set_batch_size(8);

        let cfg = s.snapshot();
        assert_eq!(cfg.interval_ns, 1_000_000);
        assert_eq!(cfg.button, Button::Right);
        assert_eq!(cfg.position, Some((640, 480)));
        assert_eq!(cfg.limit_clicks, 50);
        assert_eq!(cfg.batch_size, 8);
    }

    #[test]
    fn follow_cursor_yields_no_position() {
        let s = SharedState::new();
        s.set_fixed_point(100, 100);
        s.set_position_mode(PositionMode::FollowCursor);
        assert_eq!(s.snapshot().position, None);
    }

    #[test]
    fn defaults_are_idle_and_stopped() {
        let s = SharedState::new();
        assert!(!s.running());
        assert!(!s.shutdown());
        assert_eq!(s.engine_state(), EngineState::Idle);
        assert_eq!(s.clicks_emitted(), 0);
        assert_eq!(s.snapshot().batch_size, 1);
    }

    #[test]
    fn unknown_enum_bytes_degrade_to_defaults() {
        assert_eq!(Button::from_u8(200), Button::Left);
        assert_eq!(EngineState::from_u8(200), EngineState::Idle);
    }
}
