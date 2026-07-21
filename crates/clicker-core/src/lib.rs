//! Headless maximum-throughput click engine.
//!
//! The timing brain (`shared`, `schedule`, `clock`, `wait`, `sink`, `engine`)
//! is platform-neutral and tests on any target. Only `sink_win32`, `affinity`,
//! `hotkey`, and `runtime` are Windows-specific.

pub mod clock;
pub mod engine;
pub mod schedule;
pub mod shared;
pub mod sink;
pub mod wait;

pub use shared::{Button, Config, EngineState, PositionMode, SharedState};

#[cfg(windows)]
pub mod sink_win32;

#[cfg(windows)]
pub mod affinity;

#[cfg(windows)]
pub mod hotkey;

#[cfg(windows)]
pub mod runtime;

/// Test-only serialization for system-wide resources.
#[cfg(all(test, windows))]
pub(crate) mod testlock {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// `RegisterHotKey` is system-wide: exactly one window may own a given key
    /// combination at a time. Every test that registers the panic hotkey —
    /// directly via `PanicHotkey`, or indirectly via `EngineHandle::start` —
    /// must hold this lock, or the second one gets
    /// `ERROR_HOTKEY_ALREADY_REGISTERED` (1409).
    ///
    /// This lives at crate level rather than inside one test module precisely
    /// because the collision crosses module boundaries.
    pub fn f8_guard() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let m = LOCK.get_or_init(|| Mutex::new(()));
        m.lock().unwrap_or_else(|e| e.into_inner())
    }
}
