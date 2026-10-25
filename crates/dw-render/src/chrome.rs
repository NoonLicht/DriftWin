//! Кнопки «свернуть» и «закрыть» в правом верхнем углу окна, открытого на весь экран.

use std::sync::atomic::{AtomicI32, Ordering::Relaxed};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, EndPaint, FillRect, HGDIOBJ,
    InvalidateRect, LineTo, MoveToEx, PAINTSTRUCT, PS_SOLID, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use crate::{Event, push_event};

const WM_MOUSELEAVE: u32 = 0x02A3;
const BTN_W: i32 = 42;
const BTN_H: i32 = 30;
/// Ширина всей панели: две кнопки.
pub const WIDTH: i32 = BTN_W * 2;

/// Кнопка под курсором: 0 — свернуть, 1 — закрыть, -1 — никакая.
static HOVER: AtomicI32 = AtomicI32::new(-1);

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16)
}

fn paint(hwnd: HWND) {
    let hover = HOVER.load(Relaxed);
    let mut ps = PAINTSTRUCT::default();
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        for i in 0..2 {
            let rc = RECT {
                left: i * BTN_W,
                top: 0,
                right: (i + 1) * BTN_W,
                bottom: BTN_H,
            };
            let col = match (hover == i, i) {
                (true, 1) => rgb(200, 50, 62),
                (true, _) => rgb(72, 74, 96),
                _ => rgb(28, 29, 40),
            };
            let brush = CreateSolidBrush(col);
            FillRect(hdc, &rc, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            let pen = CreatePen(PS_SOLID, 2, rgb(238, 240, 250));
            let old = SelectObject(hdc, HGDIOBJ(pen.0));
            let (cx, cy) = (rc.left + BTN_W / 2, BTN_H / 2);
            if i == 0 {
                let _ = MoveToEx(hdc, cx - 6, cy, None);
                let _ = LineTo(hdc, cx + 7, cy);
            } else {
                let _ = MoveToEx(hdc, cx - 5, cy - 5, None);
                let _ = LineTo(hdc, cx + 6, cy + 6);
                let _ = MoveToEx(hdc, cx + 5, cy - 5, None);
                let _ = LineTo(hdc, cx - 6, cy + 6);
            }
            SelectObject(hdc, old);
            let _ = DeleteObject(HGDIOBJ(pen.0));
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

fn set_hover(hwnd: HWND, v: i32) {
    if HOVER.swap(v, Relaxed) != v {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

unsafe extern "system" fn chrome_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let button = || i32::from(lp.0 as i16) / BTN_W;
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => {
            paint(hwnd);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            set_hover(hwnd, button().clamp(0, 1));
            let mut t = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut t);
            }
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            set_hover(hwnd, -1);
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            push_event(if button() == 0 {
                Event::ChromeMinimize
            } else {
                Event::ChromeClose
            });
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

pub struct Chrome {
    hwnd: HWND,
    /// Где панель сейчас показана; `None` — скрыта.
    at: Option<(i32, i32)>,
}

impl Chrome {
    pub fn new() -> Self {
        let hwnd = unsafe {
            let Ok(hi) = GetModuleHandleW(None) else {
                return Self {
                    hwnd: HWND::default(),
                    at: None,
                };
            };
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(chrome_proc),
                hInstance: hi.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: w!("DriftWinChrome"),
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let h = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                w!("DriftWinChrome"),
                w!("DriftWin chrome"),
                WS_POPUP,
                0,
                0,
                WIDTH,
                BTN_H,
                None,
                None,
                Some(hi.into()),
                None,
            )
            .unwrap_or_default();
            if !h.is_invalid() {
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), 230, LWA_ALPHA);
            }
            h
        };
        Self { hwnd, at: None }
    }

    /// Показать панель с левым верхним углом в `(x, y)`.
    pub fn show_at(&mut self, x: i32, y: i32) {
        if self.hwnd.is_invalid() || self.at == Some((x, y)) {
            return;
        }
        self.at = Some((x, y));
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                WIDTH,
                BTN_H,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    pub fn hide(&mut self) {
        if self.at.take().is_some() {
            HOVER.store(-1, Relaxed);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }
}

impl Default for Chrome {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Chrome {
    fn drop(&mut self) {
        if !self.hwnd.is_invalid() {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}
