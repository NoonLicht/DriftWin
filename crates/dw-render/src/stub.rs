//! Заглушки: окно-плейсхолдер на месте закрытого приложения. Имя по центру, кнопка закрытия,
//! клик или Enter — запустить приложение заново. Обычное окно верхнего уровня: его можно двигать,
//! менять размер (минимум 120x120) и фокусировать; в Alt-Tab не входит (`WS_EX_TOOLWINDOW`).

use std::collections::HashMap;
use std::sync::Mutex;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use crate::{Event, push_event};

/// Имя класса окна заглушки (по нему `dw-win` узнаёт заглушки при сканировании).
pub const STUB_CLASS: &str = "DriftWinStub";

/// Минимальный размер заглушки при ручном изменении размера.
pub const MIN_SIDE: i32 = 120;
const CLOSE_BTN: i32 = 28;

struct View {
    name: String,
    launching: bool,
}

static STUBS: Mutex<Option<HashMap<isize, View>>> = Mutex::new(None);

fn with_views<R>(f: impl FnOnce(&mut HashMap<isize, View>) -> R) -> R {
    let mut g = STUBS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

fn rgb(c: [u8; 3]) -> COLORREF {
    COLORREF(u32::from(c[0]) | u32::from(c[1]) << 8 | u32::from(c[2]) << 16)
}

fn close_rect(client: &RECT) -> RECT {
    RECT {
        left: client.right - CLOSE_BTN - 6,
        top: 6,
        right: client.right - 6,
        bottom: 6 + CLOSE_BTN,
    }
}

fn text(hdc: HDC, s: &str, r: &mut RECT, flags: DRAW_TEXT_FORMAT) {
    let mut t: Vec<u16> = s.encode_utf16().collect();
    unsafe { DrawTextW(hdc, &mut t, r, flags) };
}

fn font(height: i32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
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

fn paint(hwnd: HWND, hdc: HDC) {
    let mut c = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut c);
    }
    let (name, launching) = with_views(|m| {
        m.get(&(hwnd.0 as isize))
            .map(|v| (v.name.clone(), v.launching))
            .unwrap_or_default()
    });
    unsafe {
        let bg = CreateSolidBrush(rgb([34, 37, 46]));
        FillRect(hdc, &c, bg);
        let _ = DeleteObject(bg.into());
        let edge = CreateSolidBrush(rgb(if launching {
            [90, 160, 255]
        } else {
            [96, 104, 128]
        }));
        FrameRect(hdc, &c, edge);
        let _ = DeleteObject(edge.into());
        SetBkMode(hdc, TRANSPARENT);

        let big = font(-26, 600);
        let old = SelectObject(hdc, big.into());
        SetTextColor(hdc, rgb([225, 230, 245]));
        let mut r = RECT {
            left: c.left + 12,
            top: c.top + (c.bottom - c.top) / 2 - 40,
            right: c.right - 12,
            bottom: c.top + (c.bottom - c.top) / 2 + 4,
        };
        text(
            hdc,
            &name,
            &mut r,
            DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        SelectObject(hdc, old);
        let _ = DeleteObject(big.into());

        let small = font(-15, 400);
        let old = SelectObject(hdc, small.into());
        SetTextColor(hdc, rgb([150, 158, 180]));
        let mut r = RECT {
            left: c.left + 12,
            top: c.top + (c.bottom - c.top) / 2 + 8,
            right: c.right - 12,
            bottom: c.top + (c.bottom - c.top) / 2 + 40,
        };
        let sub = if launching {
            format!("{name} запускается…")
        } else {
            "Enter или клик — запустить".to_string()
        };
        text(
            hdc,
            &sub,
            &mut r,
            DT_SINGLELINE | DT_CENTER | DT_TOP | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        // кнопка закрытия
        let cr = close_rect(&c);
        let b = CreateSolidBrush(rgb([96, 104, 128]));
        FrameRect(hdc, &cr, b);
        let _ = DeleteObject(b.into());
        SetTextColor(hdc, rgb([200, 205, 220]));
        let mut cr2 = cr;
        text(
            hdc,
            "×",
            &mut cr2,
            DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX,
        );
        SelectObject(hdc, old);
        let _ = DeleteObject(small.into());
    }
}

unsafe extern "system" fn stub_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let key = hwnd.0 as usize as u64;
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                paint(hwnd, hdc);
                let _ = EndPaint(hwnd, &ps);
            }
            return LRESULT(0);
        }
        WM_SIZE => unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        },
        WM_GETMINMAXINFO => {
            let mm = lp.0 as *mut MINMAXINFO;
            unsafe {
                (*mm).ptMinTrackSize.x = MIN_SIDE;
                (*mm).ptMinTrackSize.y = MIN_SIDE;
            }
            return LRESULT(0);
        }
        // без неклиентской области: рамку и скруглённые углы рисует DWM, края для изменения размера — ниже
        WM_NCCALCSIZE if wp.0 != 0 => return LRESULT(0),
        WM_NCHITTEST => {
            let (x, y) = (
                i32::from((lp.0 & 0xffff) as i16),
                i32::from(((lp.0 >> 16) & 0xffff) as i16),
            );
            let mut r = RECT::default();
            unsafe {
                let _ = GetWindowRect(hwnd, &mut r);
            }
            let e = 6;
            let (l, rt, t, bt) = (
                x < r.left + e,
                x >= r.right - e,
                y < r.top + e,
                y >= r.bottom - e,
            );
            let code = match (l, rt, t, bt) {
                (true, _, true, _) => HTTOPLEFT,
                (_, true, true, _) => HTTOPRIGHT,
                (true, _, _, true) => HTBOTTOMLEFT,
                (_, true, _, true) => HTBOTTOMRIGHT,
                (true, ..) => HTLEFT,
                (_, true, ..) => HTRIGHT,
                (_, _, true, _) => HTTOP,
                (_, _, _, true) => HTBOTTOM,
                _ => HTCLIENT,
            };
            return LRESULT(code as isize);
        }
        WM_LBUTTONUP => {
            let (x, y) = (
                i32::from((lp.0 & 0xffff) as i16),
                i32::from(((lp.0 >> 16) & 0xffff) as i16),
            );
            let mut c = RECT::default();
            unsafe {
                let _ = GetClientRect(hwnd, &mut c);
            }
            let cr = close_rect(&c);
            if x >= cr.left && x < cr.right && y >= cr.top && y < cr.bottom {
                push_event(Event::StubClose(key));
            } else {
                push_event(Event::StubLaunch(key));
            }
            return LRESULT(0);
        }
        WM_KEYDOWN if wp.0 == usize::from(VK_RETURN.0) => {
            push_event(Event::StubLaunch(key));
            return LRESULT(0);
        }
        WM_CLOSE => {
            // Alt+F4 на заглушке — то же, что кнопка закрытия
            push_event(Event::StubClose(key));
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn register() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        if let Ok(hi) = GetModuleHandleW(None) {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(stub_proc),
                hInstance: hi.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: w!("DriftWinStub"),
                ..Default::default()
            };
            RegisterClassExW(&wc);
        }
    });
}

/// Создать заглушку. `rect` — оконный прямоугольник (x, y, ширина, высота). Возвращает ключ (HWND).
pub fn create_stub(name: &str, rect: (i32, i32, i32, i32)) -> Option<u64> {
    register();
    let title: Vec<u16> = format!("{name} (заглушка DriftWin)\0")
        .encode_utf16()
        .collect();
    let hwnd = unsafe {
        let hi = GetModuleHandleW(None).ok()?;
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("DriftWinStub"),
            windows::core::PCWSTR(title.as_ptr()),
            WS_POPUP | WS_THICKFRAME | WS_VISIBLE,
            rect.0,
            rect.1,
            rect.2.max(MIN_SIDE),
            rect.3.max(MIN_SIDE),
            None,
            None,
            Some(hi.into()),
            None,
        )
        .ok()?
    };
    with_views(|m| {
        m.insert(
            hwnd.0 as isize,
            View {
                name: name.to_string(),
                launching: false,
            },
        )
    });
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        // пересчитать неклиентскую область (WM_NCCALCSIZE даёт нулевую), иначе DWM ещё считает невидимые рамки
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        // пока DWM не составил кадр, границы окна (DWMWA_EXTENDED_FRAME_BOUNDS) ещё неверны
        let _ = windows::Win32::Graphics::Dwm::DwmFlush();
    }
    Some(hwnd.0 as usize as u64)
}

/// Показать «запускается…» (или вернуть обычную надпись).
pub fn set_stub_launching(key: u64, launching: bool) {
    let h = key as usize as isize;
    with_views(|m| {
        if let Some(v) = m.get_mut(&h) {
            v.launching = launching;
        }
    });
    unsafe {
        let _ = InvalidateRect(Some(HWND(h as *mut _)), None, false);
    }
}

pub fn destroy_stub(key: u64) {
    let h = key as usize as isize;
    with_views(|m| m.remove(&h));
    unsafe {
        let _ = DestroyWindow(HWND(h as *mut _));
    }
}
