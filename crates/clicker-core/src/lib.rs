//! Headless maximum-throughput click engine.
//!
//! The timing brain (`shared`, `schedule`, `clock`, `wait`, `sink`, `engine`)
//! is platform-neutral and tests on any target. Only `sink_win32`, `affinity`,
//! `hotkey`, and `runtime` are Windows-specific.

pub mod shared;

pub use shared::{Button, Config, EngineState, PositionMode, SharedState};

pub mod schedule;
pub mod clock;
pub mod wait;
