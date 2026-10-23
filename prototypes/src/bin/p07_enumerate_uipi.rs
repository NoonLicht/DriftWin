//! Только чтение: отбор окон по §5.1, невидимые рамки, повышенные права (UIPI).
//! Единственная запись — no-op SetWindowPos (NOMOVE|NOSIZE|NOZORDER|NOACTIVATE) для проверки доступа.

use dw_prototypes::*;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PWSTR;

struct Row {
    class: String,
    title: String,
    exe: String,
    elevated: Option<bool>, // None = токен недоступен (вероятно, выше по привилегиям)
    noop_ok: bool,
    maximized: bool,
    border: (i32, i32, i32, i32),
}

fn is_elevated_proc(pid: u32) -> (String, Option<bool>) {
    unsafe {
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return ("?".into(), None);
        };
        let mut buf = [0u16; 520];
        let mut n = buf.len() as u32;
        let exe =
            if QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n)
                .is_ok()
            {
                String::from_utf16_lossy(&buf[..n as usize])
                    .rsplit('\\')
                    .next()
                    .unwrap_or("?")
                    .to_string()
            } else {
                "?".into()
            };
        let mut tok = Default::default();
        let el = if OpenProcessToken(p, TOKEN_QUERY, &mut tok).is_ok() {
            let mut e = TOKEN_ELEVATION::default();
            let mut ret = 0;
            let ok = GetTokenInformation(
                tok,
                TokenElevation,
                Some(&mut e as *mut _ as *mut _),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret,
            )
            .is_ok();
            let _ = CloseHandle(tok);
            ok.then_some(e.TokenIsElevated != 0)
        } else {
            None
        };
        let _ = CloseHandle(p);
        (exe, el)
    }
}

unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> windows::core::BOOL {
    unsafe {
        let rows = &mut *(lp.0 as *mut Vec<Row>);
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return true.into();
        }
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 || ex & WS_EX_NOACTIVATE.0 != 0 {
            return true.into();
        }
        if GetWindow(h, GW_OWNER).is_ok_and(|o| !o.0.is_null()) {
            return true.into();
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4);
        if cloaked != 0 {
            return true.into();
        }
        let class = class_name(h);
        if matches!(
            class.as_str(),
            "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW"
        ) {
            return true.into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let (exe, elevated) = is_elevated_proc(pid);
        let wr = window_rect(h);
        let mut fb = RECT::default();
        let _ = DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut fb as *mut _ as *mut _,
            size_of::<RECT>() as u32,
        );
        let noop_ok = SetWindowPos(
            h,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .is_ok();
        rows.push(Row {
            class,
            title: title(h),
            exe,
            elevated,
            noop_ok,
            maximized: IsZoomed(h).as_bool(),
            border: (
                fb.left - wr.left,
                wr.right - fb.right,
                fb.top - wr.top,
                wr.bottom - fb.bottom,
            ),
        });
        true.into()
    }
}

fn main() {
    init_dpi();
    let mut rows: Vec<Row> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut rows as *mut _ as isize));
        let (_, me) = is_elevated_proc(GetCurrentProcessId());
        println!("-- Этот процесс запущен с повышенными правами: {me:?}");
        if let Ok(s) = SHQueryUserNotificationState() {
            println!(
                "-- SHQueryUserNotificationState = {} (BUSY={}, D3D_FULLSCREEN={})",
                s.0, QUNS_BUSY.0, QUNS_RUNNING_D3D_FULL_SCREEN.0
            );
        }
    }
    println!("-- Окна, прошедшие фильтр §5.1: {}", rows.len());
    for r in &rows {
        let t: String = r.title.chars().take(36).collect();
        println!(
            "  {:<22} {:<26} «{}» elev={:?} noopSetWindowPos={} max={} рамка(л,п,в,н)={:?}",
            r.exe, r.class, t, r.elevated, r.noop_ok, r.maximized, r.border
        );
    }
    let blocked = rows.iter().filter(|r| !r.noop_ok).count();
    let elev = rows.iter().filter(|r| r.elevated != Some(false)).count();
    println!(
        "-- Итого: недоступны для SetWindowPos: {blocked}; с повышенными/недоступными правами: {elev}"
    );
    println!(
        "-- Окон ApplicationFrameWindow (UWP-хост): {}",
        rows.iter()
            .filter(|r| r.class == "ApplicationFrameWindow")
            .count()
    );
}
