//! Tauri backend for the Auto Clicker.
//!
//! The interface is a web UI; the engine is `clicker-core`, unchanged. This
//! layer owns the engine thread and the shared atomics and exposes a small set
//! of commands the frontend calls. The hot loop is never touched here — commands
//! only read `clicks_emitted` and write config atomics, exactly the contract the
//! native GUI used.

use clicker_core::profile::{self, Profile};
use clicker_core::runtime::EngineHandle;
use clicker_core::{EngineState, SharedState};
use std::sync::{Arc, Mutex};

struct AppState {
    shared: Arc<SharedState>,
    engine: Mutex<Option<EngineHandle>>,
}

fn apply_to_shared(s: &SharedState, p: &Profile) {
    s.set_interval_ns(1_000_000_000 / (p.cps.max(1) as u64));
    s.set_button(p.button);
    s.set_position_mode(p.position_mode);
    s.set_fixed_point(p.fixed_x, p.fixed_y);
    s.set_limit_clicks(p.limit_clicks);
    s.set_limit_ns(p.limit_ns);
}

#[derive(serde::Serialize)]
struct Status {
    running: bool,
    engine_state: u8,
    clicks: u64,
    pinned_core: Option<u32>,
    has_engine: bool,
}

#[tauri::command]
fn get_status(state: tauri::State<'_, AppState>) -> Status {
    let has_engine = state.engine.lock().unwrap().is_some();
    let pinned_core = state
        .engine
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|h| h.pinned_core());
    Status {
        running: state.shared.running(),
        engine_state: state.shared.engine_state() as u8,
        clicks: state.shared.clicks_emitted(),
        pinned_core,
        has_engine,
    }
}

#[tauri::command]
fn set_running(run: bool, state: tauri::State<'_, AppState>) {
    if run {
        // Clear any prior StoppedByLimit/Error so a fresh run starts clean.
        state.shared.set_engine_state(EngineState::Idle);
    }
    state.shared.set_running(run);
}

#[tauri::command]
fn apply_config(profile: Profile, state: tauri::State<'_, AppState>) {
    apply_to_shared(&state.shared, &profile);
}

#[tauri::command]
fn load_profile() -> Profile {
    profile::load_or_default().0
}

#[tauri::command]
fn save_profile(profile: Profile) -> Result<(), String> {
    profile::save_atomic(&profile).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let shared = Arc::new(SharedState::new());

    // Restore persisted settings before the engine starts reading them.
    let (prof, _note) = profile::load_or_default();
    apply_to_shared(&shared, &prof);

    // Start the pinned engine thread (registers the F8 kill switch). If it
    // fails — F8 already owned — the app still runs; clicking is simply
    // unavailable and `has_engine` reports false.
    let engine = EngineHandle::start(shared.clone()).ok();

    let state = AppState { shared, engine: Mutex::new(engine) };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_status,
            set_running,
            apply_config,
            load_profile,
            save_profile
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Auto Clicker");
}
