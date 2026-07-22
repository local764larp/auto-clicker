# Spec 4 — Hotkeys, Profiles, Persistence

**Date:** 2026-07-22
**Scope:** Global toggle hotkey, hold-to-click, profile persistence, the `HIGH_PRIORITY_CLASS`
opt-in, and the final README with measured numbers. Work item §5 + §7.10 of `BUILD_PROMPT.md`.
**Predecessors:** Specs 1–3 (engine, render foundation, widgets+wiring) — all merged.

## 1. Scope

**In:**
- **Toggle hotkey** — a global key (default F6) that starts/stops the engine, via `RegisterHotKey`
  on the GUI window (no hook, no system-wide latency).
- **Hold-to-click** — click only while a trigger key is held, via `RegisterRawInputDevices` with
  `RIDEV_INPUTSINK` (not a low-level hook, per the brief).
- **Profiles** — `serde_json` at `%APPDATA%\AutoClicker\profiles.json`, atomic write, schema
  version, corrupt/missing/future-version → defaults with a non-fatal notice.
- **High-priority toggle** — an opt-in that raises the process to `HIGH_PRIORITY_CLASS`, with the
  tradeoff stated. Never `REALTIME_PRIORITY_CLASS`.
- **Final README** already exists (root `README.md`); update it with the Spec 3 gate result.

**Decisions:**
1. **Profile schema is platform-neutral and lives in `clicker-core`** (`profile.rs`, deferred here
   from Spec 1). The serde types and the load/save-with-fallback logic test on Linux; only the
   `%APPDATA%` path resolution and atomic `MoveFileExW` are `#[cfg(windows)]`. Save is atomic:
   temp file in the same directory, then `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`.
2. **Both hotkeys register on the GUI window, not the core's message-only thread.** The GUI already
   has a live message pump; `RegisterHotKey(hwnd, ...)` + `WM_HOTKEY` is the minimal path. The
   core's F8 panic thread stays as-is (independent of the GUI, per Spec 1's safety requirement).
   Hotkeys must not collide with F8.
3. **Hold-to-click binds to a keyboard key, not a mouse button.** Raw input reports the trigger; a
   keyboard trigger sidesteps the synthetic-click feedback loop entirely (the clicker's own
   `SendInput` mouse events can never re-trigger a keyboard-held mode). If a future version wants a
   mouse-button hold, it must filter injected events — out of scope here.

## 2. Modules

```
crates/clicker-core/src/profile.rs   any (mostly): Profile serde types, defaults,
                                      load_or_default / save_atomic; win path helpers
crates/clicker-gui/src/hotkey.rs      win: register/handle toggle + raw-input hold
crates/clicker-gui/src/app.rs         win: apply/collect Profile; high-priority toggle
```

## 3. Profiles

```rust
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Profile {
    pub schema: u32,          // = SCHEMA_VERSION; a higher value → ignore, use defaults
    pub cps: u32,             // clamped to 1..=4000 on load
    pub button: Button,       // serde as a small string
    pub position_mode: PositionMode,
    pub fixed_x: i32,
    pub fixed_y: i32,
    pub limit_clicks: u64,
    pub limit_ns: u64,
    pub toggle_vk: u32,       // global toggle hotkey virtual-key
    pub hold_vk: u32,         // hold-to-click trigger virtual-key (0 = disabled)
    pub high_priority: bool,
}
```

- `SCHEMA_VERSION: u32 = 1`.
- `load_or_default(path)` returns `(Profile, LoadNote)` where `LoadNote::{Ok, Missing,
  Corrupt, FutureSchema}` — a corrupt or future-schema or missing file yields defaults and the
  note, never an error. The GUI surfaces a non-fatal notice, never crashes.
- `save_atomic(path, &Profile)` writes `path.tmp` then `MoveFileExW(REPLACE_EXISTING)`. On any IO
  error it returns `Err` and the caller keeps running (persistence is best-effort).
- All fields are clamped/validated on load (`cps` into `1..=4000`, unknown enum bytes → default),
  so a hand-edited file can never feed the engine a bad value.

**Testing (Linux):** round-trip serialize/deserialize; corrupt JSON → defaults + `Corrupt`;
future schema → defaults + `FutureSchema`; missing file → defaults + `Missing`; out-of-range `cps`
clamped; unknown button string → `Left`.

## 4. Hotkeys

**Toggle (`RegisterHotKey`).** On window create, register the toggle vk (default F6) with
`MOD_NOREPEAT`. `WM_HOTKEY` with the toggle id calls `app.toggle_running()` and invalidates. If
registration fails (key already owned), surface a non-fatal notice and continue — unlike F8, a
missing toggle hotkey is not a safety failure, so it does not block startup.

**Hold-to-click (`RegisterRawInputDevices`, `RIDEV_INPUTSINK`).** Register the keyboard for raw
input so key events arrive even when the window is not focused. `WM_INPUT` parses the raw keyboard
record; when the hold vk goes down, set `running = true`; on up, `running = false`. `RIDEV_INPUTSINK`
is what makes it work in the background; a low-level hook is avoided because it adds latency to
every system input event and is silently unhooked past `LowLevelHooksTimeout`. Hold mode is only
armed when `hold_vk != 0`.

**No collision with F8.** The toggle and hold vks are validated on load to not equal `VK_F8`; if a
profile sets one to F8, it is reset to the default with a notice.

## 5. High-priority toggle

A boolean the UI exposes (Spec 3 deferred it). When on, `SetPriorityClass(GetCurrentProcess,
HIGH_PRIORITY_CLASS)`; when off, `NORMAL_PRIORITY_CLASS`. The tradeoff ("may reduce system
responsiveness") is stated next to the control. `REALTIME_PRIORITY_CLASS` is never set — it can
starve the input stack and make the machine unrecoverable, the worst failure for this app.

## 6. Testing

- Profile serde + fallback: platform-neutral, on Linux.
- Atomic save round-trips through a temp dir (Windows).
- Manual: set a rate, quit, relaunch → rate restored; corrupt the JSON → app starts with defaults
  and a notice; press the toggle hotkey from another window → engine starts/stops; hold the trigger
  key → clicks only while held; toggle high-priority and confirm the engine still behaves.

## 7. Gate

Spec 4 — and the project — is complete when settings persist across restart, the toggle and hold
hotkeys work from an unfocused window, a corrupt profile falls back cleanly, and the README states
the measured ceiling and the GUI-active timing result.

## 8. Standing rules (inherited)

- Profile load never crashes the app; bad input degrades to defaults with a notice.
- No `unsafe` without an invariant comment. No `REALTIME_PRIORITY_CLASS`, ever.
- The engine hot loop stays untouched. No performance claim without a measurement.
