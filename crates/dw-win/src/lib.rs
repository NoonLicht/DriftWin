//! Windows-бэкенд: отбор окон, пакетное перемещение, baseline и аварийный возврат.

use std::path::Path;

use anyhow::Result;
use dw_core::{ScreenRect, WindowId};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PWSTR;

/// Невидимая рамка окна: видимый прямоугольник = оконный, обрезанный на эти величины.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Border {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

#[derive(Debug, Clone)]
pub struct Managed {
    pub id: WindowId,
    pub pid: u32,
    pub exe: String,
    pub title: String,
    pub border: Border,
    pub visible: ScreenRect,
}

#[derive(Debug, Clone)]
pub struct Skipped {
    pub exe: String,
    pub title: String,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Белый список имён exe (в нижнем регистре). `None` = все окна (только по явному `--all`).
    pub only: Option<Vec<String>>,
}

const EXCLUDE_EXE: &[&str] = &["taskmgr.exe"];
const EXCLUDE_CLASS: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
];

pub fn hwnd_of(id: WindowId) -> HWND {
    HWND(id.0 as usize as *mut _)
}

fn class_of(h: HWND) -> String {
    let mut b = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut b) } as usize;
    String::from_utf16_lossy(&b[..n])
}

fn title_of(h: HWND) -> String {
    let mut b = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, &mut b) } as usize;
    String::from_utf16_lossy(&b[..n])
}

/// (имя exe, повышены ли права; `None` — токен недоступен).
fn process_info(pid: u32) -> (String, Option<bool>) {
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
        let elevated = if OpenProcessToken(p, TOKEN_QUERY, &mut tok).is_ok() {
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
        (exe, elevated)
    }
}

fn window_rect(h: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(h, &mut r);
    }
    r
}

fn frame_bounds(h: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut _ as *mut _,
            size_of::<RECT>() as u32,
        )
        .ok()?;
    }
    Some(r)
}

fn measure(h: HWND) -> Option<(Border, ScreenRect)> {
    let wr = window_rect(h);
    let fb = frame_bounds(h)?;
    let border = Border {
        l: fb.left - wr.left,
        t: fb.top - wr.top,
        r: wr.right - fb.right,
        b: wr.bottom - fb.bottom,
    };
    let vis = ScreenRect {
        x: fb.left,
        y: fb.top,
        w: fb.right - fb.left,
        h: fb.bottom - fb.top,
    };
    Some((border, vis))
}

struct ScanCtx<'a> {
    filter: &'a Filter,
    found: Vec<Managed>,
    skipped: Vec<Skipped>,
}

unsafe extern "system" fn scan_cb(h: HWND, lp: LPARAM) -> windows::core::BOOL {
    unsafe {
        let ctx = &mut *(lp.0 as *mut ScanCtx);
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return true.into();
        }
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
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
        if EXCLUDE_CLASS.contains(&class_of(h).as_str()) {
            return true.into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let (exe, elevated) = process_info(pid);
        let lexe = exe.to_lowercase();
        if let Some(only) = &ctx.filter.only
            && !only.contains(&lexe)
        {
            return true.into();
        }
        let title = title_of(h);
        let mut skip = |reason| {
            ctx.skipped.push(Skipped {
                exe: exe.clone(),
                title: title.clone(),
                reason,
            })
        };
        if EXCLUDE_EXE.contains(&lexe.as_str()) {
            return true.into();
        }
        if elevated != Some(false) {
            skip("повышенные права (UIPI)");
            return true.into();
        }
        if IsZoomed(h).as_bool() {
            skip("развёрнуто на весь экран (maximized)");
            return true.into();
        }
        let Some((border, visible)) = measure(h) else {
            return true.into();
        };
        ctx.found.push(Managed {
            id: WindowId(h.0 as usize as u64),
            pid,
            exe,
            title,
            border,
            visible,
        });
        true.into()
    }
}

/// Найти управляемые окна. Второй результат — окна, которые подошли бы, но их двигать нельзя.
pub fn scan(filter: &Filter) -> (Vec<Managed>, Vec<Skipped>) {
    let mut ctx = ScanCtx {
        filter,
        found: Vec::new(),
        skipped: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(scan_cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    (ctx.found, ctx.skipped)
}

pub fn is_alive(m: &Managed) -> bool {
    let h = hwnd_of(m.id);
    unsafe {
        if !IsWindow(Some(h)).as_bool() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        pid == m.pid
    }
}

/// Текущий видимый прямоугольник окна (для обнаружения ручного перемещения пользователем).
pub fn current_visible(m: &Managed) -> Option<ScreenRect> {
    measure(hwnd_of(m.id)).map(|x| x.1)
}

pub fn is_minimized_or_maximized(m: &Managed) -> bool {
    let h = hwnd_of(m.id);
    unsafe { IsIconic(h).as_bool() || IsZoomed(h).as_bool() }
}

/// Окно, которое сейчас в фокусе.
pub fn foreground() -> Option<WindowId> {
    let h = unsafe { GetForegroundWindow() };
    (!h.0.is_null()).then_some(WindowId(h.0 as usize as u64))
}

/// Отметка для собственных синтетических нажатий (хук их игнорирует; Alt-тап не должен ничего запускать).
const FOCUS_MAGIC: usize = 0xD21F7;

fn alt_tap() {
    let k = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_MENU,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                dwExtraInfo: FOCUS_MAGIC,
                ..Default::default()
            },
        },
    };
    unsafe { SendInput(&[k(false), k(true)], size_of::<INPUT>() as i32) };
}

/// Передать фокус окну. Сначала обычный `SetForegroundWindow`; если ОС отказала —
/// Alt-тап (даёт процессу право на смену foreground) и повтор. Возвращает, получилось ли.
pub fn focus(m: &Managed) -> bool {
    let h = hwnd_of(m.id);
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
        if GetForegroundWindow() != h {
            alt_tap();
            let _ = SetForegroundWindow(h);
        }
        GetForegroundWindow() == h
    }
}

/// Пределы «парковки»: некоторые приложения хранят 16-битные координаты (см. отчёт Этапа 0).
pub const PARK_LIMIT: i32 = 30_000;

/// Двигать окна (без изменения размера), асинхронно; по одному `SetWindowPos`.
pub fn apply(items: &[(&Managed, ScreenRect)]) {
    let flags = SWP_NOACTIVATE
        | SWP_NOZORDER
        | SWP_NOOWNERZORDER
        | SWP_NOCOPYBITS
        | SWP_NOSIZE
        | SWP_NOSENDCHANGING
        | SWP_ASYNCWINDOWPOS;
    for (m, r) in items {
        let x = (r.x - m.border.l).clamp(-PARK_LIMIT, PARK_LIMIT);
        let y = (r.y - m.border.t).clamp(-PARK_LIMIT, PARK_LIMIT);
        unsafe {
            let _ = SetWindowPos(hwnd_of(m.id), None, x, y, 0, 0, flags);
        }
    }
}

/// Прямоугольник основного монитора: (x, y, ширина, высота).
pub fn primary_monitor() -> (i32, i32, u32, u32) {
    unsafe {
        let mon = MonitorFromPoint(Default::default(), MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let r = mi.rcMonitor;
        (
            r.left,
            r.top,
            (r.right - r.left) as u32,
            (r.bottom - r.top) as u32,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub id: u64,
    pub pid: u32,
    pub exe: String,
    /// Оконный прямоугольник (GetWindowRect) до управления.
    pub left: i32,
    pub top: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Baseline {
    pub version: u32,
    pub entries: Vec<BaselineEntry>,
}

impl Baseline {
    pub fn entry_for(m: &Managed, home_visible: ScreenRect) -> BaselineEntry {
        BaselineEntry {
            id: m.id.0,
            pid: m.pid,
            exe: m.exe.clone(),
            left: home_visible.x - m.border.l,
            top: home_visible.y - m.border.t,
        }
    }

    /// Атомарная запись: временный файл + переименование.
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }

    /// Вернуть окна на исходные места. Возвращает число перемещённых.
    pub fn restore(&self) -> usize {
        let flags =
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOSIZE | SWP_ASYNCWINDOWPOS;
        let mut n = 0;
        for e in &self.entries {
            let h = HWND(e.id as usize as *mut _);
            unsafe {
                if !IsWindow(Some(h)).as_bool() || IsIconic(h).as_bool() || IsZoomed(h).as_bool() {
                    continue;
                }
                let mut pid = 0u32;
                GetWindowThreadProcessId(h, Some(&mut pid));
                if pid != e.pid {
                    continue;
                }
                if SetWindowPos(h, None, e.left, e.top, 0, 0, flags).is_ok() {
                    n += 1;
                }
            }
        }
        n
    }
}

/// PerMonitorV2 для процесса. Обязательно во всех процессах, двигающих окна (иначе координаты виртуализуются).
pub fn init_dpi() {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}
