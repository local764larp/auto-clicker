//! Borderless window shell with a custom title bar.
//!
//! The window keeps `WS_OVERLAPPEDWINDOW` and strips the visual frame in
//! `WM_NCCALCSIZE`, rather than using `WS_POPUP`. That distinction is the whole
//! reason Snap Layouts, Aero Snap, and the window menu keep working — a popup
//! loses all of them, which is the usual way a custom title bar quietly breaks
//! window management.

use crate::render::color::{Palette, Theme};
use crate::render::shell::{hit_test, HitRegion, ShellMetrics};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Windows' reference DPI. Everything in the interface is authored in DIPs at
/// this scale and multiplied at draw time.
pub const BASE_DPI: f32 = 96.0;

#[derive(Debug)]
pub enum WindowError {
    ClassRegistration(u32),
    Creation(u32),
}

impl core::fmt::Display for WindowError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WindowError::ClassRegistration(e) => write!(f, "RegisterClassExW failed ({e})"),
            WindowError::Creation(e) => write!(f, "CreateWindowExW failed ({e})"),
        }
    }
}

impl std::error::Error for WindowError {}

/// Per-window state, owned by the window via `GWLP_USERDATA`.
pub struct WindowState {
    pub dpi_scale: f32,
    pub metrics: ShellMetrics,
    pub palette: Palette,
    pub width: i32,
    pub height: i32,
}

impl WindowState {
    fn new(theme: Theme) -> Self {
        Self {
            dpi_scale: 1.0,
            metrics: ShellMetrics::default(),
            palette: Palette::for_theme(theme),
            width: 0,
            height: 0,
        }
    }

    pub fn is_maximized(hwnd: HWND) -> bool {
        let mut p = WINDOWPLACEMENT {
            length: core::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        // SAFETY: `p.length` is set as the API requires and `p` is a valid
        // writable WINDOWPLACEMENT for the duration of the call.
        unsafe { GetWindowPlacement(hwnd, &mut p).is_ok() && p.showCmd == SW_SHOWMAXIMIZED.0 as u32 }
    }
}

/// Set process DPI awareness. Must run before any window exists.
///
/// Per-Monitor v2 is what makes non-client areas, dialogs, and scroll bars
/// scale correctly on mixed-DPI setups. Without it the shell renders blurry on
/// any monitor that is not the primary.
pub fn init_dpi_awareness() {
    // SAFETY: takes an opaque context handle constant and no pointers. Failure
    // only means an awareness mode was already set for this process, which is
    // not a memory-safety concern.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

const CLASS_NAME: &str = "ClickerNeumorphWindow\0";

pub fn create_main_window(theme: Theme) -> Result<HWND, WindowError> {
    // SAFETY: the class name is a NUL-terminated UTF-16 buffer that outlives
    // both calls; the state box is handed to the window in WM_NCCREATE and
    // reclaimed in WM_NCDESTROY, so it is owned exactly once.
    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|_| WindowError::Creation(0))?;
        let class: Vec<u16> = CLASS_NAME.encode_utf16().collect();

        let wc = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(WindowError::ClassRegistration(
                windows::Win32::Foundation::GetLastError().0,
            ));
        }

        let state = Box::new(WindowState::new(theme));
        let title: Vec<u16> = "Auto Clicker\0".encode_utf16().collect();

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            // Overlapped, not popup: this is what preserves Snap Layouts.
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            520,
            680,
            None,
            None,
            Some(hinst.into()),
            Some(Box::into_raw(state) as *const _),
        )
        .map_err(|_| WindowError::Creation(windows::Win32::Foundation::GetLastError().0))?;

        Ok(hwnd)
    }
}

pub fn show(hwnd: HWND) {
    // SAFETY: `hwnd` is a live window created above.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        // Force a fresh WM_NCCALCSIZE so the frame strip applies immediately
        // rather than on the first user-initiated resize.
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    }
}

pub fn run_message_loop() -> i32 {
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid writable MSG for each call; a null HWND
    // retrieves messages for every window on this thread.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    msg.wParam.0 as i32
}

fn state_ptr(hwnd: HWND) -> *mut WindowState {
    // SAFETY: GWLP_USERDATA holds either null or the pointer stored in
    // WM_NCCREATE. Callers must null-check.
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // SAFETY: lParam points to the CREATESTRUCTW the system built from
            // our CreateWindowExW call; lpCreateParams is the Box we leaked.
            unsafe {
                let cs = l.0 as *const CREATESTRUCTW;
                if !cs.is_null() {
                    let p = (*cs).lpCreateParams as *mut WindowState;
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, p as isize);
                    if !p.is_null() {
                        (*p).dpi_scale = GetDpiForWindow(hwnd) as f32 / BASE_DPI;
                    }
                }
                DefWindowProcW(hwnd, msg, w, l)
            }
        }

        // Strip the visual frame while keeping the window overlapped.
        WM_NCCALCSIZE if w.0 != 0 => {
            // SAFETY: with wParam != 0, lParam points to an NCCALCSIZE_PARAMS
            // whose rgrc[0] is the proposed client rect.
            unsafe {
                let params = l.0 as *mut NCCALCSIZE_PARAMS;
                if !params.is_null() && WindowState::is_maximized(hwnd) {
                    // A maximized borderless window would otherwise cover the
                    // taskbar and bleed past the monitor edge. Inset by the
                    // frame the system would have drawn.
                    let dpi = GetDpiForWindow(hwnd);
                    let cx = GetSystemMetricsForDpi(SM_CXFRAME, dpi)
                        + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                    let cy = GetSystemMetricsForDpi(SM_CYFRAME, dpi)
                        + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                    let r = &mut (*params).rgrc[0];
                    r.left += cx;
                    r.right -= cx;
                    r.top += cy;
                    r.bottom -= cy;
                }
            }
            // Zero client-area adjustment: the client fills the whole window.
            LRESULT(0)
        }

        WM_NCHITTEST => {
            let p = state_ptr(hwnd);
            if p.is_null() {
                // SAFETY: forwarding with the arguments received.
                return unsafe { DefWindowProcW(hwnd, msg, w, l) };
            }
            // SAFETY: `p` is the state installed in WM_NCCREATE and lives
            // until WM_NCDESTROY.
            let st = unsafe { &*p };

            let screen_x = (l.0 & 0xFFFF) as i16 as i32;
            let screen_y = ((l.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut rc = RECT::default();
            // SAFETY: `rc` is a valid writable RECT; `hwnd` is live.
            unsafe {
                let _ = GetWindowRect(hwnd, &mut rc);
            }
            let x = (screen_x - rc.left) as f32;
            let y = (screen_y - rc.top) as f32;
            let w_px = (rc.right - rc.left) as f32;
            let h_px = (rc.bottom - rc.top) as f32;

            let region = hit_test(
                x,
                y,
                w_px,
                h_px,
                st.dpi_scale,
                &st.metrics,
                WindowState::is_maximized(hwnd),
            );

            LRESULT(match region {
                HitRegion::Client => HTCLIENT as isize,
                HitRegion::Caption => HTCAPTION as isize,
                HitRegion::MinButton => HTMINBUTTON as isize,
                // Snap Layouts attaches to HTMAXBUTTON; reporting HTCLIENT
                // here is what kills the flyout.
                HitRegion::MaxButton => HTMAXBUTTON as isize,
                HitRegion::CloseButton => HTCLOSE as isize,
                HitRegion::Left => HTLEFT as isize,
                HitRegion::Right => HTRIGHT as isize,
                HitRegion::Top => HTTOP as isize,
                HitRegion::Bottom => HTBOTTOM as isize,
                HitRegion::TopLeft => HTTOPLEFT as isize,
                HitRegion::TopRight => HTTOPRIGHT as isize,
                HitRegion::BottomLeft => HTBOTTOMLEFT as isize,
                HitRegion::BottomRight => HTBOTTOMRIGHT as isize,
            })
        }

        WM_DPICHANGED => {
            let p = state_ptr(hwnd);
            if !p.is_null() {
                // SAFETY: `p` is live state; the new DPI is in the low word.
                unsafe {
                    (*p).dpi_scale = (w.0 & 0xFFFF) as f32 / BASE_DPI;
                }
            }
            // SAFETY: lParam points to the system's suggested rect for the new
            // monitor. Honouring it is required, not optional — ignoring it
            // leaves the window mis-sized after a monitor change.
            unsafe {
                let sug = l.0 as *const RECT;
                if !sug.is_null() {
                    let r = *sug;
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }

        WM_SIZE => {
            let p = state_ptr(hwnd);
            if !p.is_null() {
                // SAFETY: `p` is live state; size is packed into lParam.
                unsafe {
                    (*p).width = (l.0 & 0xFFFF) as i32;
                    (*p).height = ((l.0 >> 16) & 0xFFFF) as i32;
                }
            }
            LRESULT(0)
        }

        WM_PAINT => {
            let p = state_ptr(hwnd);
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: standard BeginPaint/EndPaint pairing; `ps` is valid and
            // the brush is deleted before returning.
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                let base = if p.is_null() {
                    Palette::light().base
                } else {
                    (*p).palette.base
                };
                let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0) as u32;
                let brush =
                    CreateSolidBrush(COLORREF(q(base.r) | (q(base.g) << 8) | (q(base.b) << 16)));
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                FillRect(hdc, &rc, brush);
                let _ = DeleteObject(brush.into());
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }

        WM_NCDESTROY => {
            let p = state_ptr(hwnd);
            if !p.is_null() {
                // SAFETY: reclaims the Box leaked in create_main_window,
                // exactly once, after which the pointer is cleared.
                unsafe {
                    drop(Box::from_raw(p));
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
            }
            // SAFETY: forwarding with the arguments received.
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }

        WM_DESTROY => {
            // SAFETY: no pointer arguments.
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }

        // SAFETY: forwarding unhandled messages with the arguments received.
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}
