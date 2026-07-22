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
    zones: Arc<Mutex<Zones>>,
}

/// Cursor-position stop regions. Purely a safety layer in the app; the engine's
/// hot loop is never involved. Screen coordinates in pixels.
#[derive(Clone, Default, serde::Deserialize)]
struct Zones {
    corner: bool,
    corner_px: i32,
    edge: bool,
    edge_px: i32,
    custom: bool,
    /// `[x, y, w, h]` rectangles.
    rects: Vec<[i32; 4]>,
}

/// True if `(x, y)` falls inside any active stop region for a screen of size
/// `(sw, sh)` with top-left `(sx, sy)`. Pure so the geometry is obvious.
fn cursor_in_stop_zone(x: i32, y: i32, sx: i32, sy: i32, sw: i32, sh: i32, z: &Zones) -> bool {
    if z.corner && z.corner_px > 0 {
        let c = z.corner_px;
        let near_x = x < sx + c || x > sx + sw - c;
        let near_y = y < sy + c || y > sy + sh - c;
        if near_x && near_y {
            return true;
        }
    }
    if z.edge && z.edge_px > 0 {
        let e = z.edge_px;
        if x < sx + e || x > sx + sw - e || y < sy + e || y > sy + sh - e {
            return true;
        }
    }
    if z.custom {
        for r in &z.rects {
            if x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3] {
                return true;
            }
        }
    }
    false
}

#[cfg(windows)]
fn spawn_zone_monitor(shared: Arc<SharedState>, zones: Arc<Mutex<Zones>>) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(15));
        if !shared.running() {
            continue;
        }
        let z = zones.lock().unwrap().clone();
        if !z.corner && !z.edge && !z.custom {
            continue;
        }
        let mut p = POINT::default();
        // SAFETY: `p` is a valid writable POINT; the metrics calls take plain
        // enum indices. Only reads the cursor and screen size.
        let (x, y, sx, sy, sw, sh) = unsafe {
            let _ = GetCursorPos(&mut p);
            (
                p.x,
                p.y,
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        if cursor_in_stop_zone(x, y, sx, sy, sw, sh, &z) {
            shared.set_running(false);
            shared.set_engine_state(EngineState::Idle);
        }
    });
}

fn apply_to_shared(s: &SharedState, p: &Profile) {
    s.set_interval_ns(1_000_000_000 / (p.cps.max(1) as u64));
    s.set_button(p.button);
    s.set_position_mode(p.position_mode);
    s.set_fixed_point(p.fixed_x, p.fixed_y);
    s.set_limit_clicks(p.limit_clicks);
    s.set_limit_ns(p.limit_ns);
    s.set_duty_pct(p.duty_pct);
    s.set_randomize_pct(p.randomize_pct);
    s.set_click_kind(p.click_kind);
    s.set_key_vk(p.key_vk);
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

// --- Presets: named profiles stored beside the profile file ---

fn presets_path() -> Option<std::path::PathBuf> {
    let p = profile::profile_path()?;
    Some(p.with_file_name("presets.json"))
}

fn read_presets() -> std::collections::BTreeMap<String, Profile> {
    presets_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_presets(map: &std::collections::BTreeMap<String, Profile>) -> Result<(), String> {
    let path = presets_path().ok_or("no APPDATA")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_presets() -> Vec<String> {
    read_presets().into_keys().collect()
}

#[tauri::command]
fn get_preset(name: String) -> Option<Profile> {
    read_presets().get(&name).cloned()
}

#[tauri::command]
fn save_preset(name: String, profile: Profile) -> Result<(), String> {
    let mut map = read_presets();
    map.insert(name, profile);
    write_presets(&map)
}

#[tauri::command]
fn delete_preset(name: String) -> Result<(), String> {
    let mut map = read_presets();
    map.remove(&name);
    write_presets(&map)
}

#[tauri::command]
fn set_zones(zones: Zones, state: tauri::State<'_, AppState>) {
    *state.zones.lock().unwrap() = zones;
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

    let shared_for_hotkey = shared.clone();
    let zones = Arc::new(Mutex::new(Zones::default()));
    #[cfg(windows)]
    spawn_zone_monitor(shared.clone(), zones.clone());
    let state = AppState { shared, engine: Mutex::new(engine), zones };

    use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Shortcut, ShortcutState};
    // Default global toggle: F6. Fires only on key-down, and flips running.
    let toggle = Shortcut::new(None, Code::F6);

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |_app, _sc, event| {
                    if event.state() == ShortcutState::Pressed {
                        let now = !shared_for_hotkey.running();
                        if now {
                            shared_for_hotkey.set_engine_state(EngineState::Idle);
                        }
                        shared_for_hotkey.set_running(now);
                    }
                })
                .build(),
        )
        .setup(move |app| {
            // A failed registration (key already owned) is non-fatal.
            let _ = app.global_shortcut().register(toggle);
            Ok(())
        })
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_status,
            set_running,
            apply_config,
            load_profile,
            save_profile,
            list_presets,
            get_preset,
            save_preset,
            delete_preset,
            set_zones
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Auto Clicker");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zones() -> Zones {
        Zones { corner: true, corner_px: 50, edge: true, edge_px: 40, custom: false, rects: vec![] }
    }

    // 1920x1080 screen at origin.
    const SW: i32 = 1920;
    const SH: i32 = 1080;

    #[test]
    fn centre_is_not_in_any_zone() {
        assert!(!cursor_in_stop_zone(960, 540, 0, 0, SW, SH, &zones()));
    }

    #[test]
    fn top_left_corner_stops() {
        assert!(cursor_in_stop_zone(5, 5, 0, 0, SW, SH, &zones()));
    }

    #[test]
    fn an_edge_but_not_corner_stops_via_edge() {
        // Middle of the top edge: inside edge band, not a corner.
        assert!(cursor_in_stop_zone(960, 10, 0, 0, SW, SH, &zones()));
    }

    #[test]
    fn edge_off_leaves_the_middle_of_an_edge_safe() {
        let mut z = zones();
        z.edge = false;
        // Corner still on, but 960,10 is not within corner_px of a corner.
        assert!(!cursor_in_stop_zone(960, 10, 0, 0, SW, SH, &z));
    }

    #[test]
    fn custom_rect_stops_inside_it() {
        let mut z = Zones::default();
        z.custom = true;
        z.rects = vec![[100, 100, 200, 200]];
        assert!(cursor_in_stop_zone(150, 150, 0, 0, SW, SH, &z));
        assert!(!cursor_in_stop_zone(50, 50, 0, 0, SW, SH, &z));
    }

    #[test]
    fn negative_origin_multi_monitor_is_handled() {
        // A monitor to the left gives a negative virtual-screen origin.
        assert!(cursor_in_stop_zone(-1915, 5, -1920, 0, 3840, SH, &zones()));
    }
}
