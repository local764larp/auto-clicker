//! Global hotkeys and process priority for the GUI window.
//!
//! The toggle hotkey uses `RegisterHotKey` — no hook, no system-wide input
//! latency. Hold-to-click uses raw input with `RIDEV_INPUTSINK` so key events
//! arrive even when the window is unfocused, again avoiding a low-level hook
//! (which would add latency to every system input event and be silently
//! unhooked past `LowLevelHooksTimeout`). Both bind keyboard keys, so the
//! clicker's own synthetic *mouse* events can never re-trigger them.

use core::ffi::c_void;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTHEADER, RIDEV_INPUTSINK, RID_INPUT, RIM_TYPEKEYBOARD,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
};
use windows::Win32::UI::WindowsAndMessaging::{WM_KEYDOWN, WM_SYSKEYDOWN};

pub const TOGGLE_HOTKEY_ID: i32 = 0xC10D;

/// Register the global start/stop hotkey. Returns false if the key is already
/// owned; the caller surfaces a non-fatal notice (unlike F8, a missing toggle
/// hotkey is not a safety failure and must not block startup).
pub fn register_toggle(hwnd: HWND, vk: u32) -> bool {
    // SAFETY: `hwnd` is a live window; MOD_NOREPEAT suppresses auto-repeat.
    unsafe {
        RegisterHotKey(Some(hwnd), TOGGLE_HOTKEY_ID, HOT_KEY_MODIFIERS(MOD_NOREPEAT.0), vk).is_ok()
    }
}

pub fn unregister_toggle(hwnd: HWND) {
    // SAFETY: unregistering an id that may not be registered is harmless.
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), TOGGLE_HOTKEY_ID);
    }
}

/// Register the keyboard for raw input so hold-to-click sees keys in the
/// background. Returns false on failure.
pub fn register_raw_keyboard(hwnd: HWND) -> bool {
    let rid = RAWINPUTDEVICE {
        usUsagePage: 0x01, // generic desktop
        usUsage: 0x06,     // keyboard
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    // SAFETY: `rid` is a single fully-initialised device descriptor; the size
    // argument matches its type.
    unsafe {
        RegisterRawInputDevices(&[rid], core::mem::size_of::<RAWINPUTDEVICE>() as u32).is_ok()
    }
}

/// Parse a `WM_INPUT` message into `(virtual_key, pressed)` if it is a keyboard
/// event. Returns `None` for mouse/other raw input or on any parse failure.
pub fn parse_raw_key(lparam: LPARAM) -> Option<(u32, bool)> {
    let hri = HRAWINPUT(lparam.0 as *mut c_void);
    let header_size = core::mem::size_of::<RAWINPUTHEADER>() as u32;

    // SAFETY: query the size, then read exactly that many bytes into `buf`.
    // `RAWINPUT` is `#[repr(C)]`; reading its header and keyboard union member
    // from a correctly-sized buffer is sound. The keyboard branch is only taken
    // when the header reports a keyboard record.
    unsafe {
        let mut size = 0u32;
        let r = GetRawInputData(hri, RID_INPUT, None, &mut size, header_size);
        if r == u32::MAX || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let got = GetRawInputData(
            hri,
            RID_INPUT,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut size,
            header_size,
        );
        if got == u32::MAX || (got as usize) < core::mem::size_of::<RAWINPUTHEADER>() {
            return None;
        }
        let raw = &*(buf.as_ptr() as *const RAWINPUT);
        if raw.header.dwType != RIM_TYPEKEYBOARD.0 {
            return None;
        }
        let kb = raw.data.keyboard;
        let pressed = kb.Message == WM_KEYDOWN || kb.Message == WM_SYSKEYDOWN;
        Some((kb.VKey as u32, pressed))
    }
}

/// Raise or restore the process priority.
///
/// `HIGH_PRIORITY_CLASS` is the ceiling: `REALTIME_PRIORITY_CLASS` is never set,
/// because it can starve the input stack and leave the machine unrecoverable —
/// the worst failure mode for this application.
pub fn set_high_priority(high: bool) {
    use windows::Win32::System::Threading::{
        GetCurrentProcess, SetPriorityClass, HIGH_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    };
    let class = if high { HIGH_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS };
    // SAFETY: GetCurrentProcess returns a pseudo-handle needing no close; the
    // class is a valid priority constant.
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), class);
    }
}
