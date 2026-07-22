//! Borderless window shell + widget host.
//!
//! The window keeps `WS_OVERLAPPEDWINDOW` and strips the visual frame in
//! `WM_NCCALCSIZE`, rather than using `WS_POPUP`. That distinction is the whole
//! reason Snap Layouts, Aero Snap, and the window menu keep working — a popup
//! loses all of them, which is the usual way a custom title bar quietly breaks
//! window management.
//!
//! This file is the input adapter: it translates Win32 messages into
//! `WidgetEvent`s, routes them to the hovered/focused widget's model, applies
//! any resulting action to a config atomic via `App`, and repaints. The engine
//! thread is never touched here except the one `clicks_emitted` read the timer
//! does.

use crate::app::App;
use crate::render::color::{Rgb, Theme};
use crate::render::device::{is_device_lost, DriverKind, RenderDevice};
use crate::render::shadow::clamp_radius;
use crate::render::shell::{hit_test, HitRegion, ShellMetrics};
use crate::widget::text::TextStyle;
use crate::widget::value;
use crate::widget::{
    layout, render as wrender, transition, KeyCode, WidgetEvent, WidgetId, ALL_WIDGETS,
};
use clicker_core::PositionMode;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const BASE_DPI: f32 = 96.0;
pub const WINDOW_RADIUS_DIP: f32 = 16.0;
const READOUT_TIMER: usize = 1;
const READOUT_INTERVAL_MS: u32 = 100;

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
    pub theme: Theme,
    pub device: Option<RenderDevice>,
    pub app: Option<App>,
    pub hovered: Option<WidgetId>,
    pub pressed: Option<WidgetId>,
}

fn d2d_color(c: Rgb, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a }
}

impl WindowState {
    fn new(theme: Theme) -> Self {
        Self {
            dpi_scale: 1.0,
            metrics: ShellMetrics::default(),
            theme,
            device: None,
            app: None,
            hovered: None,
            pressed: None,
        }
    }

    pub fn is_maximized(hwnd: HWND) -> bool {
        let mut p = WINDOWPLACEMENT {
            length: core::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        // SAFETY: `p.length` is set as the API requires and `p` is a valid
        // writable WINDOWPLACEMENT for the duration of the call.
        unsafe {
            GetWindowPlacement(hwnd, &mut p).is_ok() && p.showCmd == SW_SHOWMAXIMIZED.0 as u32
        }
    }
}

/// Set process DPI awareness. Must run before any window exists.
pub fn init_dpi_awareness() {
    // SAFETY: takes an opaque context constant and no pointers; a failure only
    // means an awareness mode was already set, which is not a safety concern.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

const CLASS_NAME: &str = "ClickerNeumorphWindow\0";

pub fn create_main_window(theme: Theme) -> Result<HWND, WindowError> {
    // SAFETY: the class name is NUL-terminated UTF-16 outliving both calls; the
    // state box is handed to the window in WM_NCCREATE and reclaimed in
    // WM_NCDESTROY, so it is owned exactly once.
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
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            420,
            560,
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
    // SAFETY: `msg` is a valid writable MSG for each call; a null HWND retrieves
    // messages for every window on this thread.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    msg.wParam.0 as i32
}

fn state_ptr(hwnd: HWND) -> *mut WindowState {
    // SAFETY: GWLP_USERDATA holds null or the pointer stored in WM_NCCREATE.
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState }
}

/// Client size in DIPs.
fn client_dip(hwnd: HWND, scale: f32) -> (f32, f32) {
    let mut rc = RECT::default();
    // SAFETY: `rc` is valid and writable; `hwnd` is live.
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    ((rc.right - rc.left) as f32 / scale, (rc.bottom - rc.top) as f32 / scale)
}

fn invalidate_all(hwnd: HWND) {
    // SAFETY: `hwnd` is live; a null rect invalidates the whole client area.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Feed one event to a widget's model and apply the resulting action.
fn dispatch(hwnd: HWND, st: &mut WindowState, id: WidgetId, ev: WidgetEvent) {
    let Some(app) = st.app.as_mut() else { return };
    let (next, action) = transition(app.state(id), ev);
    app.set_state(id, next);
    if action == Some(crate::widget::Action::Fire) {
        match id {
            WidgetId::StartStop => app.toggle_running(),
            WidgetId::ModeToggle => {
                let m = if app.mode == PositionMode::FollowCursor {
                    PositionMode::FixedPoint
                } else {
                    PositionMode::FollowCursor
                };
                app.set_mode(m);
            }
            _ => {}
        }
    }
    invalidate_all(hwnd);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // SAFETY: lParam points to the CREATESTRUCTW the system built; its
            // lpCreateParams is the Box we leaked in create_main_window.
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

        WM_CREATE => {
            let p = state_ptr(hwnd);
            if !p.is_null() {
                // SAFETY: `p` is the state installed in WM_NCCREATE.
                let st = unsafe { &mut *p };
                let mut rc = RECT::default();
                // SAFETY: `rc` is valid and writable; `hwnd` is live.
                unsafe {
                    let _ = GetClientRect(hwnd, &mut rc);
                }
                let size =
                    ((rc.right - rc.left).max(1) as u32, (rc.bottom - rc.top).max(1) as u32);
                match RenderDevice::new(hwnd, DriverKind::Hardware, size, st.dpi_scale) {
                    Ok(d) => st.device = Some(d),
                    Err(e) => eprintln!("device creation failed: {e}"),
                }
                let render_only = match App::new(st.dpi_scale, st.theme) {
                    Ok(a) => {
                        let ro = !a.has_engine();
                        st.app = Some(a);
                        ro
                    }
                    Err(e) => {
                        eprintln!("engine failed to start: {e}");
                        false
                    }
                };
                // In render-only mode (timing gate) the timer drives a
                // continuous 60 Hz full repaint — a heavier load than the real
                // 10-Hz-on-change design, so the gate over-stresses the engine.
                let interval = if render_only { 16 } else { READOUT_INTERVAL_MS };
                // SAFETY: `hwnd` is live; the readout timer drives the CPS sample.
                unsafe {
                    let _ = SetTimer(Some(hwnd), READOUT_TIMER, interval, None);
                }
            }
            LRESULT(0)
        }

        WM_NCCALCSIZE if w.0 != 0 => {
            // SAFETY: with wParam != 0, lParam points to an NCCALCSIZE_PARAMS
            // whose rgrc[0] is the proposed client rect.
            unsafe {
                let params = l.0 as *mut NCCALCSIZE_PARAMS;
                if !params.is_null() && WindowState::is_maximized(hwnd) {
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
            LRESULT(0)
        }

        WM_NCHITTEST => {
            let p = state_ptr(hwnd);
            if p.is_null() {
                // SAFETY: forwarding with the arguments received.
                return unsafe { DefWindowProcW(hwnd, msg, w, l) };
            }
            // SAFETY: `p` is live state until WM_NCDESTROY.
            let st = unsafe { &*p };
            let screen_x = (l.0 & 0xFFFF) as i16 as i32;
            let screen_y = ((l.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut rc = RECT::default();
            // SAFETY: `rc` is valid and writable; `hwnd` is live.
            unsafe {
                let _ = GetWindowRect(hwnd, &mut rc);
            }
            let region = hit_test(
                (screen_x - rc.left) as f32,
                (screen_y - rc.top) as f32,
                (rc.right - rc.left) as f32,
                (rc.bottom - rc.top) as f32,
                st.dpi_scale,
                &st.metrics,
                WindowState::is_maximized(hwnd),
            );
            LRESULT(match region {
                HitRegion::Client => HTCLIENT as isize,
                HitRegion::Caption => HTCAPTION as isize,
                HitRegion::MinButton => HTMINBUTTON as isize,
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

        WM_MOUSEMOVE => {
            let p = state_ptr(hwnd);
            if !p.is_null() {
                // SAFETY: `p` is live state owned by this window.
                let st = unsafe { &mut *p };
                let scale = st.dpi_scale;
                let px = (l.0 & 0xFFFF) as i16 as f32 / scale;
                let py = ((l.0 >> 16) & 0xFFFF) as i16 as f32 / scale;
                let (cw, ch) = client_dip(hwnd, scale);
                let hit = layout::layout(cw, ch).hit(px, py);
                if hit != st.hovered {
                    if let Some(old) = st.hovered {
                        dispatch(hwnd, st, old, WidgetEvent::PointerLeave);
                    }
                    if let Some(new) = hit {
                        dispatch(hwnd, st, new, WidgetEvent::PointerEnter);
                    }
                    st.hovered = hit;
                }
                // Live slider drag while the button is held.
                if st.pressed == Some(WidgetId::RateSlider) {
                    drag_slider(hwnd, st, px, py);
                }
            }
            LRESULT(0)
        }

        WM_LBUTTONDOWN => {
            let p = state_ptr(hwnd);
            if !p.is_null() {
                // SAFETY: `p` is live state.
                let st = unsafe { &mut *p };
                let scale = st.dpi_scale;
                let px = (l.0 & 0xFFFF) as i16 as f32 / scale;
                let py = ((l.0 >> 16) & 0xFFFF) as i16 as f32 / scale;
                let (cw, ch) = client_dip(hwnd, scale);
                let rects = layout::layout(cw, ch);
                if let Some(id) = rects.hit(px, py) {
                    // SAFETY: capture is released on WM_LBUTTONUP.
                    unsafe {
                        SetCapture(hwnd);
                    }
                    st.pressed = Some(id);
                    focus_widget(hwnd, st, id);
                    let (lx, ly) = rects.get(id).local(px, py);
                    dispatch(hwnd, st, id, WidgetEvent::PointerDown { x: lx, y: ly });
                    if id == WidgetId::RateSlider {
                        drag_slider(hwnd, st, px, py);
                    }
                }
            }
            LRESULT(0)
        }

        WM_LBUTTONUP => {
            let p = state_ptr(hwnd);
            if !p.is_null() {
                // SAFETY: `p` is live state.
                let st = unsafe { &mut *p };
                // SAFETY: releasing the capture taken on button-down.
                unsafe {
                    let _ = ReleaseCapture();
                }
                if let Some(id) = st.pressed.take() {
                    let scale = st.dpi_scale;
                    let px = (l.0 & 0xFFFF) as i16 as f32 / scale;
                    let py = ((l.0 >> 16) & 0xFFFF) as i16 as f32 / scale;
                    let (cw, ch) = client_dip(hwnd, scale);
                    let (lx, ly) = layout::layout(cw, ch).get(id).local(px, py);
                    dispatch(hwnd, st, id, WidgetEvent::PointerUp { x: lx, y: ly });
                }
            }
            LRESULT(0)
        }

        WM_KEYDOWN => {
            let p = state_ptr(hwnd);
            if !p.is_null() {
                // SAFETY: `p` is live state.
                let st = unsafe { &mut *p };
                let vk = w.0 as u32;
                match vk {
                    0x09 => key_tab(hwnd, st),                       // VK_TAB
                    0x54 => key_theme(hwnd, st),                     // 'T'
                    0x25 | 0x27 => key_arrow(hwnd, st, vk == 0x27),  // Left/Right
                    0x20 | 0x0D => key_fire(hwnd, st),               // Space/Enter
                    _ => {}
                }
            }
            LRESULT(0)
        }

        WM_TIMER if w.0 == READOUT_TIMER => {
            let p = state_ptr(hwnd);
            if !p.is_null() {
                // SAFETY: `p` is live state. One Relaxed load of clicks_emitted;
                // invalidate only the readout rect so nothing else repaints.
                let st = unsafe { &mut *p };
                if st.app.as_ref().is_some_and(|a| !a.has_engine()) {
                    // Render-only gate: force a full repaint to stress the GUI.
                    invalidate_all(hwnd);
                    return LRESULT(0);
                }
                if let Some(app) = st.app.as_mut() {
                    let before = app.last_cps;
                    let now = app.sample_cps(READOUT_INTERVAL_MS as f64 / 1000.0);
                    if now != before {
                        let (cw, ch) = client_dip(hwnd, st.dpi_scale);
                        let r = layout::layout(cw, ch).get(WidgetId::CpsReadout);
                        let scale = st.dpi_scale;
                        let rc = RECT {
                            left: (r.x * scale) as i32,
                            top: (r.y * scale) as i32,
                            right: ((r.x + r.w) * scale) as i32,
                            bottom: ((r.y + r.h) * scale) as i32,
                        };
                        // SAFETY: `hwnd` is live; `rc` is a valid rect.
                        unsafe {
                            let _ = InvalidateRect(Some(hwnd), Some(&rc), false);
                        }
                    }
                }
            }
            LRESULT(0)
        }

        WM_DPICHANGED => {
            let p = state_ptr(hwnd);
            if !p.is_null() {
                // SAFETY: `p` is live state; new DPI in the low word.
                unsafe {
                    let scale = (w.0 & 0xFFFF) as f32 / BASE_DPI;
                    (*p).dpi_scale = scale;
                    if let Some(app) = (*p).app.as_mut() {
                        app.dpi_scale = scale;
                        app.cache.clear();
                    }
                }
            }
            // SAFETY: lParam is the system's suggested rect for the new monitor.
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
                // SAFETY: `p` is live state; size packed into lParam.
                unsafe {
                    let (cw, ch) = ((l.0 & 0xFFFF) as u32, ((l.0 >> 16) & 0xFFFF) as u32);
                    if let Some(dev) = (*p).device.as_mut() {
                        if let Err(e) = dev.resize(cw.max(1), ch.max(1)) {
                            if is_device_lost(&e.0) {
                                let _ = dev.recreate(hwnd);
                            }
                        }
                    }
                    if let Some(app) = (*p).app.as_mut() {
                        app.cache.clear(); // sizes are DIP-relative to client width
                    }
                }
            }
            LRESULT(0)
        }

        WM_PAINT => {
            let p = state_ptr(hwnd);
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: standard BeginPaint/EndPaint pairing around the frame.
            unsafe {
                let _ = BeginPaint(hwnd, &mut ps);
            }
            if !p.is_null() {
                // SAFETY: `p` is live state owned by this window.
                let st = unsafe { &mut *p };
                if let Err(e) = paint(hwnd, st) {
                    if is_device_lost(&e.0) {
                        if let Some(dev) = st.device.as_mut() {
                            let _ = dev.recreate(hwnd);
                        }
                    } else {
                        eprintln!("paint failed: {e}");
                    }
                }
            }
            // SAFETY: paired with BeginPaint above.
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }

        WM_NCDESTROY => {
            let p = state_ptr(hwnd);
            if !p.is_null() {
                // SAFETY: reclaims the Box leaked in create_main_window, once.
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
                let _ = KillTimer(Some(hwnd), READOUT_TIMER);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }

        // SAFETY: forwarding unhandled messages with the arguments received.
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

/// Slider drag: map the pointer x to a CPS and write it.
fn drag_slider(hwnd: HWND, st: &mut WindowState, px: f32, _py: f32) {
    let scale = st.dpi_scale;
    let (cw, ch) = client_dip(hwnd, scale);
    let r = layout::layout(cw, ch).get(WidgetId::RateSlider);
    let gmin = r.x + wrender::SLIDER_THUMB / 2.0;
    let gmax = r.x + r.w - wrender::SLIDER_THUMB / 2.0;
    let cps = value::slider_value(px, gmin, gmax, value::CPS_MIN, value::CPS_SLIDER_MAX);
    if let Some(app) = st.app.as_mut() {
        app.set_cps(cps);
    }
    invalidate_all(hwnd);
}

fn focus_widget(hwnd: HWND, st: &mut WindowState, id: WidgetId) {
    // Focus is orthogonal to interaction state: only the ring moves.
    if let Some(app) = st.app.as_mut() {
        app.focus.focus(id);
    }
    invalidate_all(hwnd);
}

fn key_tab(hwnd: HWND, st: &mut WindowState) {
    // SAFETY: GetKeyState takes a plain VK and returns a SHORT.
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    if let Some(app) = st.app.as_mut() {
        if shift {
            app.focus.prev();
        } else {
            app.focus.next();
        }
    }
    invalidate_all(hwnd);
}

fn key_theme(hwnd: HWND, st: &mut WindowState) {
    if let Some(app) = st.app.as_mut() {
        app.toggle_theme();
        st.theme = app.palette.theme;
    }
    invalidate_all(hwnd);
}

fn key_arrow(hwnd: HWND, st: &mut WindowState, right: bool) {
    if let Some(app) = st.app.as_mut() {
        if app.focus.focused() == Some(WidgetId::RateSlider) {
            let step = 10i64;
            let next = (app.cps as i64 + if right { step } else { -step })
                .clamp(value::CPS_MIN as i64, value::CPS_SLIDER_MAX as i64);
            app.set_cps(next as u32);
            invalidate_all(hwnd);
        }
    }
}

fn key_fire(hwnd: HWND, st: &mut WindowState) {
    if let Some(app) = st.app.as_mut() {
        if let Some(id) = app.focus.focused() {
            dispatch(hwnd, st, id, WidgetEvent::Key(KeyCode::Space));
        }
    }
}

/// Draw one frame: base surface, then every widget with its focus ring and text.
fn paint(hwnd: HWND, st: &mut WindowState) -> Result<(), crate::render::device::RenderError> {
    let scale = st.dpi_scale;
    let (w_dip, h_dip) = client_dip(hwnd, scale);
    let radius = if WindowState::is_maximized(hwnd) {
        0.0
    } else {
        clamp_radius(WINDOW_RADIUS_DIP, w_dip, h_dip)
    };

    // Disjoint field borrows: device and app are separate fields.
    let (Some(dev), Some(app)) = (st.device.as_ref(), st.app.as_mut()) else {
        return Ok(());
    };
    let rects = layout::layout(w_dip, h_dip);
    let fixed = app.mode == PositionMode::FixedPoint;

    // Compute every widget's surfaces once, shared by the prepare and blit
    // passes so they never disagree.
    let mut specs: Vec<(WidgetId, layout::Rect, Vec<wrender::SurfaceSpec>)> = Vec::new();
    for id in ALL_WIDGETS {
        let r = rects.get(id);
        let s = wrender::surface_specs(id, r, app.state(id), app.cps, fixed);
        specs.push((id, r, s));
    }

    // Prepare pass: render any missing surfaces OUTSIDE begin_draw.
    let prep: Vec<(layout::Rect, Vec<wrender::SurfaceSpec>)> =
        specs.iter().map(|(_, r, s)| (*r, s.clone())).collect();
    wrender::prepare(&mut app.cache, &dev.ctx, &prep, &app.palette, scale)?;

    dev.begin_draw();
    // SAFETY: the context has a bound target between begin_draw and end_draw.
    unsafe {
        dev.ctx.Clear(Some(&d2d_color(Rgb::new(0.0, 0.0, 0.0), 0.0)));
        let brush = dev.ctx.CreateSolidColorBrush(&d2d_color(app.palette.base, 1.0), None)?;
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: 0.0, top: 0.0, right: w_dip, bottom: h_dip },
            radiusX: radius,
            radiusY: radius,
        };
        dev.ctx.FillRoundedRectangle(&rr, &brush);

        let focused = app.focus.focused();
        for (id, r, list) in &specs {
            wrender::blit_specs(&mut app.cache, &dev.ctx, *r, list, &app.palette, scale)?;

            // Text / accent overlays.
            match id {
                WidgetId::StartStop => {
                    let label = if app.is_running() { "Stop" } else { "Start" };
                    let color = if app.is_running() { app.palette.accent } else { app.palette.text_primary };
                    let _ = crate::widget::text::draw_text(
                        &dev.ctx,
                        label,
                        rect_f(*r),
                        app.text.format(TextStyle::Body),
                        color,
                    );
                }
                WidgetId::CpsReadout => {
                    let big = D2D_RECT_F {
                        left: r.x,
                        top: r.y + 8.0,
                        right: r.x + r.w,
                        bottom: r.y + r.h - 24.0,
                    };
                    let _ = crate::widget::text::draw_text(
                        &dev.ctx,
                        &app.last_cps.to_string(),
                        big,
                        app.text.format(TextStyle::Large),
                        app.palette.text_primary,
                    );
                    let cap = D2D_RECT_F {
                        left: r.x,
                        top: r.y + r.h - 28.0,
                        right: r.x + r.w,
                        bottom: r.y + r.h,
                    };
                    let _ = crate::widget::text::draw_text(
                        &dev.ctx,
                        "CLICKS / SEC",
                        cap,
                        app.text.format(TextStyle::Body),
                        app.palette.text_secondary,
                    );
                }
                WidgetId::IntervalField => {
                    let _ = crate::widget::text::draw_text(
                        &dev.ctx,
                        &format!("{} CPS", app.field_text),
                        rect_f(*r),
                        app.text.format(TextStyle::Body),
                        app.palette.text_primary,
                    );
                }
                _ => {}
            }

            if focused == Some(*id) {
                wrender::accent_ring(&dev.ctx, *r, &app.palette);
            }
        }
    }
    dev.end_draw()?;
    dev.present()?;
    Ok(())
}

fn rect_f(r: layout::Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h }
}
