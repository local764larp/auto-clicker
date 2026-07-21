use crate::shared::{EngineState, SharedState};
use std::sync::mpsc;
use std::sync::Arc;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT, VK_F8,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, PostThreadMessageW,
    TranslateMessage, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_HOTKEY, WM_QUIT,
};

pub const DEFAULT_PANIC_VK: u32 = VK_F8.0 as u32;
const HOTKEY_ID: i32 = 0xC10C;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HotkeyError {
    /// The key is already claimed by another process.
    Register(u32),
    WindowCreation(u32),
}

impl core::fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HotkeyError::Register(e) => write!(
                f,
                "could not register the emergency-stop hotkey (GetLastError={e}); \
                 another application already owns that key. The engine must not \
                 start without a working kill switch."
            ),
            HotkeyError::WindowCreation(e) => {
                write!(f, "could not create the message-only window (GetLastError={e})")
            }
        }
    }
}

impl std::error::Error for HotkeyError {}

/// Owns a message-only window on its own thread, registers the panic hotkey,
/// and clears `running` the instant the key is pressed.
#[derive(Debug)]
pub struct PanicHotkey {
    thread_id: u32,
    handle: Option<std::thread::JoinHandle<()>>,
    ready: bool,
}

impl PanicHotkey {
    /// Clear `running` directly on the shared atomic. No queue, no channel, no
    /// round-trip through render or layout code.
    pub fn stop_now(shared: &SharedState) {
        shared.set_running(false);
        shared.set_engine_state(EngineState::Idle);
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn register(shared: Arc<SharedState>, vk: u32) -> Result<Self, HotkeyError> {
        let (tx, rx) = mpsc::channel::<Result<u32, HotkeyError>>();

        let handle = std::thread::Builder::new()
            .name("clicker-panic-hotkey".into())
            .spawn(move || {
                let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
                // SAFETY: HWND_MESSAGE creates a message-only window. `class`
                // is a NUL-terminated UTF-16 buffer that outlives the call, and
                // the window is destroyed before this thread returns.
                let hwnd = unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        PCWSTR(class.as_ptr()),
                        PCWSTR::null(),
                        WINDOW_STYLE(0),
                        0,
                        0,
                        0,
                        0,
                        Some(HWND_MESSAGE),
                        None,
                        None,
                        None,
                    )
                };

                let hwnd: HWND = match hwnd {
                    Ok(h) => h,
                    Err(_) => {
                        // SAFETY: no arguments; reads thread-local error state.
                        let e = unsafe { GetLastError() }.0;
                        let _ = tx.send(Err(HotkeyError::WindowCreation(e)));
                        return;
                    }
                };

                // SAFETY: `hwnd` is the live window created immediately above.
                // MOD_NOREPEAT suppresses auto-repeat storms while held.
                let reg = unsafe {
                    RegisterHotKey(Some(hwnd), HOTKEY_ID, HOT_KEY_MODIFIERS(MOD_NOREPEAT.0), vk)
                };

                if reg.is_err() {
                    // SAFETY: no arguments.
                    let e = unsafe { GetLastError() }.0;
                    // SAFETY: `hwnd` is live and destroyed exactly once here.
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                    let _ = tx.send(Err(HotkeyError::Register(e)));
                    return;
                }

                // SAFETY: no arguments; returns this thread's id.
                let tid = unsafe { GetCurrentThreadId() };
                let _ = tx.send(Ok(tid));

                let mut msg = MSG::default();
                loop {
                    // SAFETY: `msg` is a valid writable MSG. Passing a null HWND
                    // retrieves messages for any window on this thread, which is
                    // what receives both WM_HOTKEY and our WM_QUIT.
                    let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                    if got.0 <= 0 {
                        break; // WM_QUIT or error
                    }
                    if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == HOTKEY_ID {
                        Self::stop_now(&shared);
                    }
                    // SAFETY: `msg` was just filled by GetMessageW.
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }

                // SAFETY: `hwnd` is still live; UnregisterHotKey then
                // DestroyWindow each run exactly once on this path.
                unsafe {
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
                    let _ = DestroyWindow(hwnd);
                }
            })
            .expect("failed to spawn hotkey thread");

        match rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self { thread_id, handle: Some(handle), ready: true }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                Err(HotkeyError::WindowCreation(0))
            }
        }
    }
}

impl Drop for PanicHotkey {
    fn drop(&mut self) {
        // SAFETY: posting WM_QUIT to our own thread id is safe; the worst case
        // is the thread has already exited and the post fails harmlessly.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::shared::SharedState;
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, Instant};

    /// `RegisterHotKey` is system-wide: exactly one window may own a given key
    /// combination at a time. Tests that actually register F8 must therefore
    /// run one at a time, or the second gets ERROR_HOTKEY_ALREADY_REGISTERED
    /// (1409). Serializing here rather than relying on `--test-threads=1`
    /// keeps a plain `cargo test` green — a test that only passes under a
    /// special flag is a test that will get muted.
    fn f8_guard() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let m = LOCK.get_or_init(|| Mutex::new(()));
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn registers_and_unregisters_cleanly() {
        let _serial = f8_guard();
        let shared = Arc::new(SharedState::new());
        let hk = PanicHotkey::register(shared.clone(), DEFAULT_PANIC_VK)
            .expect("F8 should be available on a test machine");
        drop(hk);

        // A second registration only succeeds if the first fully released F8.
        let hk2 = PanicHotkey::register(shared, DEFAULT_PANIC_VK)
            .expect("hotkey was not released on drop");
        drop(hk2);
    }

    #[test]
    fn stop_now_clears_running_immediately() {
        let shared = Arc::new(SharedState::new());
        shared.set_running(true);
        PanicHotkey::stop_now(&shared);
        assert!(!shared.running());
        assert_eq!(shared.engine_state(), EngineState::Idle);
    }

    #[test]
    fn message_thread_becomes_ready_promptly() {
        let _serial = f8_guard();
        let shared = Arc::new(SharedState::new());
        let start = Instant::now();
        let hk = PanicHotkey::register(shared, DEFAULT_PANIC_VK).unwrap();
        assert!(hk.is_ready(), "thread did not signal readiness");
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
