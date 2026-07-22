//! Persisted settings.
//!
//! The serde types and the parse-with-fallback logic are platform-neutral and
//! test on any target. Only the `%APPDATA%` path and the atomic `MoveFileExW`
//! rename are `#[cfg(windows)]`.
//!
//! A profile file is never trusted: every field is clamped or defaulted on
//! load, and a corrupt, missing, or future-schema file yields defaults plus a
//! note rather than an error. A hand-edited file can never feed the engine a
//! bad value, and a bad file never crashes startup.

use crate::shared::{Button, PositionMode};
use serde::{Deserialize, Serialize};

/// Bump when the on-disk shape changes incompatibly. A file with a higher
/// schema than we understand is ignored (defaults used).
pub const SCHEMA_VERSION: u32 = 1;

/// Clamp bounds, matching the GUI's slider/field range.
pub const CPS_MIN: u32 = 1;
pub const CPS_MAX: u32 = 4000;

/// `VK_F8`, reserved for the emergency stop; a profile may not bind it.
pub const VK_F8: u32 = 0x77;
/// Default global toggle key: `VK_F6`.
pub const DEFAULT_TOGGLE_VK: u32 = 0x75;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub schema: u32,
    pub cps: u32,
    pub button: Button,
    pub position_mode: PositionMode,
    pub fixed_x: i32,
    pub fixed_y: i32,
    pub limit_clicks: u64,
    pub limit_ns: u64,
    /// Global start/stop hotkey virtual-key.
    pub toggle_vk: u32,
    /// Hold-to-click trigger virtual-key; `0` disables hold mode.
    pub hold_vk: u32,
    pub high_priority: bool,
    /// Fraction of the interval the button is held per click, in percent.
    #[serde(default)]
    pub duty_pct: u8,
    /// Interval jitter, in percent.
    #[serde(default)]
    pub randomize_pct: u8,
    /// 0 = mouse button, 1 = keyboard key.
    #[serde(default)]
    pub click_kind: u8,
    /// Virtual-key pressed in keyboard mode.
    #[serde(default = "default_key_vk")]
    pub key_vk: u32,
}

fn default_key_vk() -> u32 {
    0x20 // Space
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            cps: 100,
            button: Button::Left,
            position_mode: PositionMode::FollowCursor,
            fixed_x: 0,
            fixed_y: 0,
            limit_clicks: 0,
            limit_ns: 0,
            toggle_vk: DEFAULT_TOGGLE_VK,
            hold_vk: 0,
            high_priority: false,
            duty_pct: 0,
            randomize_pct: 0,
            click_kind: 0,
            key_vk: default_key_vk(),
        }
    }
}

impl Profile {
    /// Clamp/validate every field so no hand-edited value reaches the engine.
    fn sanitized(mut self) -> Self {
        self.cps = self.cps.clamp(CPS_MIN, CPS_MAX);
        // A hotkey may not shadow the emergency stop.
        if self.toggle_vk == VK_F8 || self.toggle_vk == 0 {
            self.toggle_vk = DEFAULT_TOGGLE_VK;
        }
        if self.hold_vk == VK_F8 {
            self.hold_vk = 0; // disable rather than fight F8
        }
        self.duty_pct = self.duty_pct.min(95);
        self.randomize_pct = self.randomize_pct.min(95);
        self.schema = SCHEMA_VERSION;
        self
    }

    pub fn to_json(&self) -> String {
        // A fixed, small struct — serialization cannot realistically fail, but
        // fall back to `{}` rather than panicking if it somehow does.
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// What happened when loading — surfaced to the user as a non-fatal notice.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LoadNote {
    Ok,
    Missing,
    Corrupt,
    FutureSchema,
}

/// Parse profile bytes, always returning a usable profile.
///
/// Platform-neutral and the substance of the test suite: corrupt JSON, a
/// future schema, and out-of-range values all degrade to sanitized defaults.
pub fn parse_or_default(bytes: Option<&[u8]>) -> (Profile, LoadNote) {
    let Some(bytes) = bytes else {
        return (Profile::default(), LoadNote::Missing);
    };
    // Tolerate a UTF-8 BOM: some editors (Notepad) prepend one, and a hand-edit
    // should not be treated as corrupt. serde_json rejects a leading BOM.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    // Peek the schema first so a future version is a clean fallback, not a
    // parse against a shape we do not understand.
    match serde_json::from_slice::<Profile>(bytes) {
        Ok(p) if p.schema > SCHEMA_VERSION => (Profile::default(), LoadNote::FutureSchema),
        Ok(p) => (p.sanitized(), LoadNote::Ok),
        Err(_) => (Profile::default(), LoadNote::Corrupt),
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::path::PathBuf;

    /// `%APPDATA%\AutoClicker\profiles.json`.
    pub fn profile_path() -> Option<PathBuf> {
        let appdata = std::env::var_os("APPDATA")?;
        let mut p = PathBuf::from(appdata);
        p.push("AutoClicker");
        p.push("profiles.json");
        Some(p)
    }

    /// Read and parse the profile, always returning something usable.
    pub fn load_or_default() -> (Profile, LoadNote) {
        let Some(path) = profile_path() else {
            return (Profile::default(), LoadNote::Missing);
        };
        match std::fs::read(&path) {
            Ok(bytes) => parse_or_default(Some(&bytes)),
            Err(_) => (Profile::default(), LoadNote::Missing),
        }
    }

    /// Write atomically: temp file in the same directory, then rename over the
    /// target. A crash mid-write can never leave a half-written profile.
    pub fn save_atomic(profile: &Profile) -> std::io::Result<()> {
        use std::io::Write;
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING};

        let Some(path) = profile_path() else {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no APPDATA"));
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(profile.to_json().as_bytes())?;
            f.sync_all()?;
        }

        let tmp_w = HSTRING::from(tmp.as_os_str());
        let dst_w = HSTRING::from(path.as_os_str());
        // SAFETY: both paths are valid NUL-terminated wide strings owned by the
        // HSTRINGs for the duration of the call. REPLACE_EXISTING makes the
        // rename atomically overwrite the previous profile.
        unsafe {
            MoveFileExW(&tmp_w, &dst_w, MOVEFILE_REPLACE_EXISTING)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use win::{load_or_default, profile_path, save_atomic};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let mut p = Profile::default();
        p.cps = 750;
        p.button = Button::Right;
        p.position_mode = PositionMode::FixedPoint;
        p.fixed_x = 640;
        p.high_priority = true;
        let json = p.to_json();
        let (back, note) = parse_or_default(Some(json.as_bytes()));
        assert_eq!(note, LoadNote::Ok);
        assert_eq!(back, p);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let (p, note) = parse_or_default(None);
        assert_eq!(note, LoadNote::Missing);
        assert_eq!(p, Profile::default());
    }

    #[test]
    fn corrupt_json_yields_defaults() {
        let (p, note) = parse_or_default(Some(b"{ this is not json"));
        assert_eq!(note, LoadNote::Corrupt);
        assert_eq!(p, Profile::default());
    }

    #[test]
    fn future_schema_is_ignored() {
        let mut p = Profile::default();
        p.schema = SCHEMA_VERSION + 5;
        let json = p.to_json();
        let (back, note) = parse_or_default(Some(json.as_bytes()));
        assert_eq!(note, LoadNote::FutureSchema);
        assert_eq!(back, Profile::default());
    }

    #[test]
    fn out_of_range_cps_is_clamped() {
        let json = r#"{"schema":1,"cps":999999,"button":"Left","position_mode":"FollowCursor",
            "fixed_x":0,"fixed_y":0,"limit_clicks":0,"limit_ns":0,"toggle_vk":117,"hold_vk":0,
            "high_priority":false}"#;
        let (p, note) = parse_or_default(Some(json.as_bytes()));
        assert_eq!(note, LoadNote::Ok);
        assert_eq!(p.cps, CPS_MAX);
    }

    #[test]
    fn a_hotkey_may_not_shadow_the_emergency_stop() {
        let mut p = Profile::default();
        p.toggle_vk = VK_F8;
        p.hold_vk = VK_F8;
        let (back, _) = parse_or_default(Some(p.to_json().as_bytes()));
        assert_ne!(back.toggle_vk, VK_F8, "toggle must not bind F8");
        assert_eq!(back.hold_vk, 0, "hold on F8 must be disabled");
    }

    #[test]
    fn a_utf8_bom_is_tolerated() {
        // Notepad and PowerShell's utf8 encoding prepend a BOM; a hand-edited
        // profile with one must still load rather than silently reset.
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(Profile::default().to_json().as_bytes());
        let (_, note) = parse_or_default(Some(&bytes));
        assert_eq!(note, LoadNote::Ok, "a BOM should not read as corrupt");
    }

    #[test]
    fn zero_toggle_falls_back_to_default() {
        let mut p = Profile::default();
        p.toggle_vk = 0;
        let (back, _) = parse_or_default(Some(p.to_json().as_bytes()));
        assert_eq!(back.toggle_vk, DEFAULT_TOGGLE_VK);
    }

    #[test]
    fn unknown_button_string_is_a_parse_error_then_defaults() {
        let json = r#"{"schema":1,"cps":100,"button":"Purple","position_mode":"FollowCursor",
            "fixed_x":0,"fixed_y":0,"limit_clicks":0,"limit_ns":0,"toggle_vk":117,"hold_vk":0,
            "high_priority":false}"#;
        let (p, note) = parse_or_default(Some(json.as_bytes()));
        assert_eq!(note, LoadNote::Corrupt);
        assert_eq!(p.button, Button::Left);
    }
}
