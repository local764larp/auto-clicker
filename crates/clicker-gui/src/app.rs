//! Application state: owns the engine and mediates every write to its config.
//!
//! Widget actions are the only writers to the config atomics, and they all go
//! through here. The pure helpers (`apply_cps`, `cps_from_delta`) are tested on
//! any target; the `App` struct that owns the `EngineHandle` is Windows-only.

use clicker_core::SharedState;
use std::sync::Arc;

/// Write the engine's period for a target CPS. The only path that sets
/// `interval_ns`, so the derivation lives in one tested place.
pub fn apply_cps(shared: &Arc<SharedState>, cps: u32) {
    shared.set_interval_ns(crate::widget::value::cps_to_interval_ns(cps));
}

/// CPS from a click-count delta over an elapsed window.
pub fn cps_from_delta(delta_clicks: u64, elapsed_s: f64) -> u32 {
    if elapsed_s <= 0.0 {
        return 0;
    }
    (delta_clicks as f64 / elapsed_s).round() as u32
}

#[cfg(windows)]
pub use win::App;

#[cfg(windows)]
mod win {
    use super::*;
    use crate::render::color::{Palette, Theme};
    use crate::widget::render::SurfaceCache;
    use crate::widget::text::TextRenderer;
    use crate::widget::value;
    use crate::widget::{FocusRing, WidgetId, WidgetState, ALL_WIDGETS};
    use clicker_core::runtime::EngineHandle;
    use clicker_core::{EngineState, PositionMode};
    use std::collections::HashMap;

    pub struct App {
        pub shared: Arc<SharedState>,
        /// `None` in render-only mode (env `CLICKER_NO_ENGINE`), used by the
        /// timing gate so the GUI can run its message pump and rendering
        /// alongside the bench's engine without a second F8 registration.
        _engine: Option<EngineHandle>,
        pub states: HashMap<WidgetId, WidgetState>,
        pub focus: FocusRing<WidgetId>,
        pub cps: u32,
        pub field_text: String,
        pub mode: PositionMode,
        pub palette: Palette,
        pub dpi_scale: f32,
        pub cache: SurfaceCache,
        pub text: TextRenderer,
        last_clicks: u64,
        pub last_cps: u32,
    }

    impl App {
        pub fn new(dpi_scale: f32, theme: Theme) -> Result<App, String> {
            let shared = Arc::new(SharedState::new());
            let cps = 100u32;
            apply_cps(&shared, cps);
            // Render-only mode for the timing gate: skip the engine (and its F8
            // registration) so the GUI can run beside the bench's engine.
            let engine = if std::env::var_os("CLICKER_NO_ENGINE").is_some() {
                None
            } else {
                Some(EngineHandle::start(shared.clone()).map_err(|e| e.to_string())?)
            };

            let mut states = HashMap::new();
            for id in ALL_WIDGETS {
                states.insert(id, WidgetState::Idle);
            }
            states.insert(WidgetId::CpsReadout, WidgetState::Disabled); // display only

            let focus = FocusRing::new(vec![
                (WidgetId::StartStop, true),
                (WidgetId::ModeToggle, true),
                (WidgetId::RateSlider, true),
                (WidgetId::IntervalField, true),
            ]);

            Ok(App {
                shared,
                _engine: engine,
                states,
                focus,
                cps,
                field_text: cps.to_string(),
                mode: PositionMode::FollowCursor,
                palette: Palette::for_theme(theme),
                dpi_scale,
                cache: SurfaceCache::new(),
                text: TextRenderer::new().map_err(|e| e.to_string())?,
                last_clicks: 0,
                last_cps: 0,
            })
        }

        /// False in render-only mode (timing gate).
        pub fn has_engine(&self) -> bool {
            self._engine.is_some()
        }

        pub fn state(&self, id: WidgetId) -> WidgetState {
            *self.states.get(&id).unwrap_or(&WidgetState::Idle)
        }

        pub fn set_state(&mut self, id: WidgetId, s: WidgetState) {
            self.states.insert(id, s);
        }

        pub fn set_cps(&mut self, cps: u32) {
            self.cps = cps.clamp(value::CPS_MIN, value::CPS_HARD_MAX);
            self.field_text = self.cps.to_string();
            apply_cps(&self.shared, self.cps);
        }

        pub fn is_running(&self) -> bool {
            self.shared.running()
        }

        pub fn toggle_running(&mut self) {
            let now = !self.shared.running();
            if now {
                // Clear a prior StoppedByLimit/Error so a fresh run starts clean.
                self.shared.set_engine_state(EngineState::Idle);
            }
            self.shared.set_running(now);
        }

        pub fn set_mode(&mut self, mode: PositionMode) {
            self.mode = mode;
            self.shared.set_position_mode(mode);
        }

        /// Sample the click counter; returns the current CPS for the readout.
        pub fn sample_cps(&mut self, elapsed_s: f64) -> u32 {
            let now = self.shared.clicks_emitted();
            let delta = now.saturating_sub(self.last_clicks);
            self.last_clicks = now;
            self.last_cps = cps_from_delta(delta, elapsed_s);
            self.last_cps
        }

        pub fn toggle_theme(&mut self) {
            self.palette = match self.palette.theme {
                Theme::Light => Palette::dark(),
                Theme::Dark => Palette::light(),
            };
            self.cache.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_cps_writes_the_derived_interval() {
        let shared = Arc::new(SharedState::new());
        apply_cps(&shared, 500);
        assert_eq!(shared.snapshot().interval_ns, 2_000_000);
    }

    #[test]
    fn sample_cps_uses_the_delta_since_last_tick() {
        assert_eq!(cps_from_delta(100, 0.100), 1000);
        assert_eq!(cps_from_delta(0, 0.100), 0);
    }

    #[test]
    fn cps_from_delta_guards_zero_window() {
        assert_eq!(cps_from_delta(50, 0.0), 0);
    }
}
