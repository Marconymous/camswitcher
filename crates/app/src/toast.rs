//! On-screen notification: a small topmost, click-through box near the bottom
//! of the screen that shows the new state for a moment after a hotkey or tray
//! action (useful while another app is in front).

use std::sync::Mutex;
use std::sync::OnceLock;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DEFAULT_CHARSET,
    DT_CALCRECT, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FW_SEMIBOLD, FillRect,
    GetDC, GetMonitorInfoW, HFONT, InvalidateRect, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, ReleaseDC, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetForegroundWindow, GetMessageW, HWND_TOPMOST, KillTimer,
    LWA_ALPHA, MSG, PostMessageW, RegisterClassW, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetTimer,
    SetWindowPos, ShowWindow, TranslateMessage, WM_APP, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

const WM_SHOW_TOAST: u32 = WM_APP + 1;
const HIDE_TIMER: usize = 1;
const VISIBLE_MS: u32 = 1300;
/// Font height and paddings at 96 dpi.
const FONT_PX: i32 = 22;
const PAD_X: i32 = 28;
const PAD_Y: i32 = 14;
/// Distance from the bottom of the work area.
const BOTTOM_GAP: i32 = 90;

/// The toast window (raw `HWND`, which isn't `Send`).
static WINDOW: OnceLock<isize> = OnceLock::new();
static TEXT: Mutex<String> = Mutex::new(String::new());

/// Starts the toast thread and its (hidden) window.
pub fn start() {
    let spawned = std::thread::Builder::new().name("toast".into()).spawn(|| unsafe { run() });
    if let Err(e) = spawned {
        log::warn!("toast thread: {e}");
    }
}

/// Shows `text` for a moment; replaces a toast that is still visible.
pub fn show(text: &str) {
    if let Ok(mut t) = TEXT.lock() {
        text.clone_into(&mut t);
    }
    if let Some(&hwnd) = WINDOW.get() {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as _)), WM_SHOW_TOAST, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe fn run() {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("CamSwitchToast");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: class, ..Default::default() };
        if RegisterClassW(&wc) == 0 {
            log::warn!("toast: RegisterClassW failed");
            return;
        }
        let hwnd = match CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
            class,
            w!("CamSwitch"),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(instance.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                log::warn!("toast window: {e}");
                return;
            }
        };
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 235, LWA_ALPHA);
        let _ = WINDOW.set(hwnd.0 as isize);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SHOW_TOAST => {
                show_now(hwnd);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == HIDE_TIMER => {
                let _ = KillTimer(Some(hwnd), HIDE_TIMER);
                let _ = ShowWindow(hwnd, SW_HIDE);
                LRESULT(0)
            }
            WM_PAINT => {
                paint(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn text_utf16() -> Vec<u16> {
    TEXT.lock().map(|t| t.encode_utf16().collect()).unwrap_or_default()
}

/// Font for the window's current DPI; caller deletes it.
unsafe fn font(hwnd: HWND) -> HFONT {
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        CreateFontW(
            -FONT_PX * dpi / 96,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        )
    }
}

unsafe fn show_now(hwnd: HWND) {
    unsafe {
        let mut text = text_utf16();
        if text.is_empty() {
            return;
        }
        // Place on the monitor the user is looking at (foreground window).
        let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut mi).as_bool() {
            return;
        }
        let work = mi.rcWork;
        // Size for the target monitor's DPI: move there first.
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), work.left, work.top, 10, 10, SWP_NOACTIVATE);
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let scale = |v: i32| v * dpi / 96;

        let hdc = GetDC(Some(hwnd));
        let f = font(hwnd);
        let old = SelectObject(hdc, f.into());
        let mut r = RECT::default();
        DrawTextW(hdc, &mut text, &mut r, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(hdc, old);
        let _ = DeleteObject(f.into());
        ReleaseDC(Some(hwnd), hdc);

        let w = (r.right - r.left + 2 * scale(PAD_X)).min(work.right - work.left);
        let h = r.bottom - r.top + 2 * scale(PAD_Y);
        let x = work.left + (work.right - work.left - w) / 2;
        let y = work.bottom - h - scale(BOTTOM_GAP);
        let radius = scale(16);
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, radius, radius);
        // The window owns the region from here on.
        SetWindowRgn(hwnd, Some(rgn), false);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let _ = InvalidateRect(Some(hwnd), None, true);
        SetTimer(Some(hwnd), HIDE_TIMER, VISIBLE_MS, None);
    }
}

unsafe fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let brush = CreateSolidBrush(COLORREF(0x00202020));
        FillRect(hdc, &rc, brush);
        let _ = DeleteObject(brush.into());
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x00FFFFFF));
        let f = font(hwnd);
        let old = SelectObject(hdc, f.into());
        let mut text = text_utf16();
        // An empty slice passes a dangling pointer, which DrawTextW dereferences.
        if !text.is_empty() {
            DrawTextW(hdc, &mut text, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        }
        SelectObject(hdc, old);
        let _ = DeleteObject(f.into());
        let _ = EndPaint(hwnd, &ps);
    }
}
