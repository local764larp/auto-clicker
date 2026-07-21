use crate::shared::Button;
use crate::sink::{ClickSink, SinkError};
use windows::Win32::Foundation::GetLastError;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
    MOUSE_EVENT_FLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

/// Maximum clicks per syscall. The buffer is `2 * MAX_BATCH` INPUT structs,
/// allocated once at construction and never grown.
pub const MAX_BATCH: usize = 32;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VirtualScreen {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

/// Map a virtual-desktop pixel to the 0..=65535 absolute range `SendInput`
/// expects when `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` is set.
pub fn normalize_abs(x: i32, y: i32, vs: VirtualScreen) -> (i32, i32) {
    let denom_x = (vs.width - 1).max(1) as i64;
    let denom_y = (vs.height - 1).max(1) as i64;
    let nx = ((x - vs.left) as i64 * 65535 / denom_x).clamp(0, 65535) as i32;
    let ny = ((y - vs.top) as i64 * 65535 / denom_y).clamp(0, 65535) as i32;
    (nx, ny)
}

fn down_up_flags(b: Button) -> (MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS) {
    match b {
        Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
    }
}

/// Production sink. One `SendInput` call carries K complete clicks as
/// `2*K` events — the largest remaining user-mode throughput win.
pub struct SendInputSink {
    buf: Vec<INPUT>,
    vs: VirtualScreen,
}

impl SendInputSink {
    pub fn new() -> Self {
        let mut s = Self {
            buf: vec![INPUT::default(); MAX_BATCH * 2],
            vs: VirtualScreen { left: 0, top: 0, width: 1, height: 1 },
        };
        s.refresh_virtual_screen();
        s
    }

    pub fn buffer_capacity(&self) -> usize {
        self.buf.len()
    }

    /// Re-read virtual-screen metrics. Call on display change, never per click.
    pub fn refresh_virtual_screen(&mut self) {
        // SAFETY: GetSystemMetrics takes a plain enum index and returns an i32.
        // No pointers are involved and it cannot fail in a memory-unsafe way.
        unsafe {
            self.vs = VirtualScreen {
                left: GetSystemMetrics(SM_XVIRTUALSCREEN),
                top: GetSystemMetrics(SM_YVIRTUALSCREEN),
                width: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
                height: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
            };
        }
    }
}

impl Default for SendInputSink {
    fn default() -> Self {
        Self::new()
    }
}

impl ClickSink for SendInputSink {
    fn emit_batch(
        &mut self,
        button: Button,
        pos: Option<(i32, i32)>,
        count: u16,
    ) -> Result<u16, SinkError> {
        let count = (count as usize).min(MAX_BATCH);
        if count == 0 {
            return Ok(0);
        }

        let (down, up) = down_up_flags(button);

        // Fixed-position mode ORs the move onto the DOWN event rather than
        // adding a third event, so a positioned click is still one syscall and
        // still two events. Follow-cursor omits the move entirely and never
        // calls GetCursorPos — the click lands where the cursor already is.
        let (move_flags, dx, dy) = match pos {
            Some((x, y)) => {
                let (nx, ny) = normalize_abs(x, y, self.vs);
                (
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    nx,
                    ny,
                )
            }
            None => (MOUSE_EVENT_FLAGS(0), 0, 0),
        };

        for i in 0..count {
            self.buf[i * 2] = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx,
                        dy,
                        mouseData: 0,
                        dwFlags: down | move_flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            self.buf[i * 2 + 1] = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: up,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
        }

        let events = count * 2;
        // SAFETY: `self.buf` holds at least `events` initialised INPUT structs
        // (the loop above wrote every one of them), the slice is valid for the
        // duration of the call, and the size argument matches the element type
        // exactly. SendInput does not retain the pointer past return.
        let inserted =
            unsafe { SendInput(&self.buf[..events], core::mem::size_of::<INPUT>() as i32) };

        if inserted as usize != events {
            // SAFETY: GetLastError reads thread-local error state and takes no
            // arguments.
            let last_error = unsafe { GetLastError() }.0;
            return Err(SinkError::Blocked {
                inserted,
                expected: events as u32,
                last_error,
            });
        }

        Ok(count as u16)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn primary_monitor_origin_maps_to_zero() {
        let vs = VirtualScreen { left: 0, top: 0, width: 1920, height: 1080 };
        assert_eq!(normalize_abs(0, 0, vs), (0, 0));
    }

    #[test]
    fn bottom_right_maps_to_full_scale() {
        let vs = VirtualScreen { left: 0, top: 0, width: 1920, height: 1080 };
        assert_eq!(normalize_abs(1919, 1079, vs), (65535, 65535));
    }

    #[test]
    fn negative_origin_left_monitor_maps_correctly() {
        // A second monitor to the left gives a negative virtual-screen origin.
        // Getting this wrong confines all clicks to the primary display.
        let vs = VirtualScreen { left: -1920, top: 0, width: 3840, height: 1080 };
        assert_eq!(normalize_abs(-1920, 0, vs), (0, 0));
        let (mid_x, _) = normalize_abs(0, 0, vs);
        assert!((32750..=32790).contains(&mid_x), "midpoint was {mid_x}");
    }

    #[test]
    fn degenerate_virtual_screen_does_not_divide_by_zero() {
        let vs = VirtualScreen { left: 0, top: 0, width: 1, height: 1 };
        let (x, y) = normalize_abs(0, 0, vs);
        assert_eq!((x, y), (0, 0));
    }

    #[test]
    fn batch_buffer_is_preallocated_to_the_maximum() {
        let s = SendInputSink::new();
        assert_eq!(s.buffer_capacity(), MAX_BATCH * 2);
    }
}
