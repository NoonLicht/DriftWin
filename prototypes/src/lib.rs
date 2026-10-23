//! Общие помощники прототипов Этапа 0.
//! Прототипы двигают ТОЛЬКО собственные тестовые окна; чужие окна читаются, но не меняются.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, FillRect, HDC};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

unsafe extern "system" fn wndproc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if m == WM_ERASEBKGND {
            let color = GetWindowLongPtrW(h, GWLP_USERDATA) as u32;
            let mut r = RECT::default();
            let _ = GetClientRect(h, &mut r);
            let br = CreateSolidBrush(COLORREF(color));
            FillRect(HDC(wp.0 as _), &r, br);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(br.into());
            return LRESULT(1);
        }
        DefWindowProcW(h, m, wp, lp)
    }
}

fn ensure_class() -> PCWSTR {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let hi: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hi,
            lpszClassName: w!("DriftWinProtoWnd"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
    });
    w!("DriftWinProtoWnd")
}

/// Цветное окно верхнего уровня без активации (не крадёт фокус).
pub fn create_test_window(title: &str, x: i32, y: i32, w: i32, h: i32, bgr: u32) -> HWND {
    create_test_window_ex(WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW, title, x, y, w, h, bgr)
}

pub fn create_test_window_ex(
    ex: WINDOW_EX_STYLE,
    title: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    bgr: u32,
) -> HWND {
    unsafe {
        let hi: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let t: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let hwnd = CreateWindowExW(
            ex,
            ensure_class(),
            PCWSTR(t.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hi),
            None,
        )
        .expect("CreateWindowExW");
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, bgr as isize);
        hwnd
    }
}

/// Прокачать очередь сообщений текущего потока в течение `d`.
pub fn pump_for(d: Duration) {
    let end = Instant::now() + d;
    unsafe {
        while Instant::now() < end {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

pub fn window_rect(h: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(h, &mut r);
    }
    r
}

pub fn class_name(h: HWND) -> String {
    let mut b = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut b) } as usize;
    String::from_utf16_lossy(&b[..n])
}

pub fn title(h: HWND) -> String {
    let mut b = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, &mut b) } as usize;
    String::from_utf16_lossy(&b[..n])
}

/// PerMonitorV2 — в настоящем приложении это делается манифестом.
pub fn init_dpi() {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Блокирующий цикл сообщений (как у нормального приложения) до WM_QUIT.
pub fn run_message_loop() {
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
