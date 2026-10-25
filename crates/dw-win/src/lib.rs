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
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, DEVMODEW, DIB_RGB_COLORS, DeleteDC, DeleteObject, ENUM_CURRENT_SETTINGS,
    EnumDisplayMonitors, EnumDisplaySettingsW, GetDC, GetDIBits, GetMonitorInfoW, HBITMAP, HDC,
    HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW, MonitorFromPoint, ReleaseDC,
    SRCCOPY, SelectObject, SetWindowRgn,
};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY,
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
    pub class: String,
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
    /// Имена exe (нижний регистр), которые не трогаем.
    pub exclude_exe: Vec<String>,
    /// Классы окон, которые не трогаем (без учёта регистра).
    pub exclude_class: Vec<String>,
}

const EXCLUDE_EXE: &[&str] = &["taskmgr.exe"];
const EXCLUDE_CLASS: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
];

/// Класс окон-заглушек DriftWin (см. `dw-render`).
pub const STUB_CLASS: &str = "DriftWinStub";

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
        let is_stub = class_of(h) == STUB_CLASS;
        if !is_stub && ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
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
        let class = class_of(h);
        if EXCLUDE_CLASS.contains(&class.as_str())
            || ctx
                .filter
                .exclude_class
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&class))
        {
            return true.into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let (exe, elevated) = process_info(pid);
        let lexe = exe.to_lowercase();
        if let Some(only) = &ctx.filter.only
            && !is_stub
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
        if EXCLUDE_EXE.contains(&lexe.as_str()) || ctx.filter.exclude_exe.contains(&lexe) {
            return true.into();
        }
        if !is_stub && elevated != Some(false) {
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
            class,
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

/// Попросить окно закрыться (как крестик в заголовке).
pub fn close(m: &Managed) {
    unsafe {
        let _ = PostMessageW(
            Some(hwnd_of(m.id)),
            WM_CLOSE,
            Default::default(),
            Default::default(),
        );
    }
}

/// Изменить размер окна так, чтобы видимый кадр стал `w`x`h` (положение не меняется).
pub fn resize_visible(m: &Managed, w: i32, h: i32) {
    let flags = SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOMOVE;
    unsafe {
        let _ = SetWindowPos(
            hwnd_of(m.id),
            None,
            0,
            0,
            w + m.border.l + m.border.r,
            h + m.border.t + m.border.b,
            flags,
        );
    }
}

/// Окно, которое сейчас в фокусе.
pub fn foreground() -> Option<WindowId> {
    let h = unsafe { GetForegroundWindow() };
    (!h.0.is_null()).then_some(WindowId(h.0 as usize as u64))
}

/// Отметка для собственных синтетических нажатий (хук их игнорирует; Alt-тап не должен ничего запускать).
const FOCUS_MAGIC: usize = 0xD21F7;

/// Нажатие неиспользуемой клавиши: считается вводом процесса (даёт право сменить foreground),
/// но, в отличие от Alt, не включает режим меню/подсказок в приложениях.
fn input_tap() {
    const UNASSIGNED_VK: u16 = 0xE8;
    let k = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(UNASSIGNED_VK),
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
/// нейтральное нажатие клавиши (даёт процессу право на смену foreground) и повтор. Возвращает, получилось ли.
pub fn focus(m: &Managed) -> bool {
    let h = hwnd_of(m.id);
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
        if GetForegroundWindow() != h {
            input_tap();
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

const CLIP_PROP: windows::core::PCWSTR = windows::core::w!("DriftWinClip");

/// Снять область обрезки, наложенную прежними версиями DriftWin (признак — свойство окна `DriftWinClip`).
fn clear_clip_hwnd(h: HWND) {
    unsafe {
        if IsWindow(Some(h)).as_bool() && !GetPropW(h, CLIP_PROP).is_invalid() {
            SetWindowRgn(h, None, true);
            let _ = RemovePropW(h, CLIP_PROP);
        }
    }
}

/// Двигать и менять размер окон (видимый кадр -> оконный прямоугольник), асинхронно.
pub fn apply_sized(items: &[(&Managed, ScreenRect)]) {
    let flags =
        SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOSENDCHANGING | SWP_ASYNCWINDOWPOS;
    for (m, r) in items {
        let x = (r.x - m.border.l).clamp(-PARK_LIMIT, PARK_LIMIT);
        let y = (r.y - m.border.t).clamp(-PARK_LIMIT, PARK_LIMIT);
        let (w, h) = (r.w + m.border.l + m.border.r, r.h + m.border.t + m.border.b);
        unsafe {
            let _ = SetWindowPos(hwnd_of(m.id), None, x, y, w, h, flags);
        }
    }
}

/// Позиция курсора на виртуальном рабочем столе.
pub fn cursor_pos() -> (i32, i32) {
    let mut p = windows::Win32::Foundation::POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

/// Окно верхнего уровня под точкой экрана.
pub fn window_at(x: i32, y: i32) -> Option<WindowId> {
    let h = unsafe { WindowFromPoint(windows::Win32::Foundation::POINT { x, y }) };
    if h.0.is_null() {
        return None;
    }
    let root = unsafe { GetAncestor(h, GA_ROOT) };
    let root = if root.0.is_null() { h } else { root };
    Some(WindowId(root.0 as usize as u64))
}

/// Виртуальный рабочий стол (все мониторы): (x, y, ширина, высота).
pub fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
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

/// Монитор рабочего стола.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    /// Имя устройства без префикса: `DISPLAY1`.
    pub name: String,
    /// Прямоугольник: (x, y, ширина, высота) в координатах виртуального рабочего стола.
    pub rect: (i32, i32, i32, i32),
    pub primary: bool,
    /// Частота обновления, Гц (0 — неизвестна).
    pub refresh: u32,
}

unsafe extern "system" fn monitor_cb(
    mon: HMONITOR,
    _: HDC,
    _: *mut RECT,
    lp: LPARAM,
) -> windows::core::BOOL {
    let out = unsafe { &mut *(lp.0 as *mut Vec<MonitorInfo>) };
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(mon, &mut mi as *mut _ as *mut MONITORINFO) }.as_bool() {
        let r = mi.monitorInfo.rcMonitor;
        let n = mi.szDevice.iter().position(|&c| c == 0).unwrap_or(0);
        let name = String::from_utf16_lossy(&mi.szDevice[..n]);
        let mut dm = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        let refresh = if unsafe {
            EnumDisplaySettingsW(
                windows::core::PCWSTR(mi.szDevice.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut dm,
            )
        }
        .as_bool()
        {
            dm.dmDisplayFrequency
        } else {
            0
        };
        out.push(MonitorInfo {
            name: name.trim_start_matches("\\\\.\\").to_string(),
            rect: (r.left, r.top, r.right - r.left, r.bottom - r.top),
            primary: mi.monitorInfo.dwFlags & 1 != 0,
            refresh,
        });
    }
    true.into()
}

/// Все мониторы, по имени устройства (`DISPLAY1`, `DISPLAY2`, …).
pub fn monitors() -> Vec<MonitorInfo> {
    let mut v: Vec<MonitorInfo> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(monitor_cb),
            LPARAM(&mut v as *mut _ as isize),
        );
    }
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// Выбрать мониторы по именам из конфига: `primary`, `DISPLAY2`, `2` (= `DISPLAY2`). Пусто или ничего
/// не найдено — основной монитор.
pub fn select_monitors(all: &[MonitorInfo], names: &[String]) -> Vec<MonitorInfo> {
    let want = |m: &MonitorInfo, n: &str| {
        let n = n.trim().to_lowercase();
        n == "primary" && m.primary
            || n == m.name.to_lowercase()
            || n.parse::<u32>()
                .is_ok_and(|i| m.name.to_lowercase() == format!("display{i}"))
    };
    let mut sel: Vec<MonitorInfo> = all
        .iter()
        .filter(|m| names.iter().any(|n| want(m, n)))
        .cloned()
        .collect();
    if sel.is_empty() {
        sel = all.iter().filter(|m| m.primary).cloned().collect();
    }
    if sel.is_empty() {
        sel = all.first().cloned().into_iter().collect();
    }
    sel
}

/// Таймер кадров с высоким разрешением (около 0,5 мс вместо ~15 мс у `Sleep`).
pub struct FrameTimer(Option<windows::Win32::Foundation::HANDLE>);

impl FrameTimer {
    pub fn new() -> Self {
        let h = unsafe {
            CreateWaitableTimerExW(
                None,
                None,
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        }
        .ok();
        Self(h)
    }

    /// Подождать `d`; без высокого разрешения (старая Windows) — обычный сон.
    pub fn wait(&self, d: std::time::Duration) {
        let Some(h) = self.0 else {
            std::thread::sleep(d);
            return;
        };
        // относительное время в сотнях наносекунд, со знаком «минус»
        let due = -((d.as_nanos() / 100) as i64).max(1);
        unsafe {
            if SetWaitableTimer(h, &due, 0, None, None, false).is_ok() {
                WaitForSingleObject(h, INFINITE);
            } else {
                std::thread::sleep(d);
            }
        }
    }
}

impl Default for FrameTimer {
    fn default() -> Self {
        Self::new()
    }
}

/// Прямоугольник, охватывающий все данные мониторы.
pub fn bounding_rect(mons: &[MonitorInfo]) -> (i32, i32, i32, i32) {
    let x0 = mons.iter().map(|m| m.rect.0).min().unwrap_or(0);
    let y0 = mons.iter().map(|m| m.rect.1).min().unwrap_or(0);
    let x1 = mons.iter().map(|m| m.rect.0 + m.rect.2).max().unwrap_or(0);
    let y1 = mons.iter().map(|m| m.rect.1 + m.rect.3).max().unwrap_or(0);
    (x0, y0, x1 - x0, y1 - y0)
}

#[cfg(test)]
mod monitor_tests {
    use super::*;

    fn m(name: &str, x: i32, y: i32, w: i32, h: i32, primary: bool) -> MonitorInfo {
        MonitorInfo {
            name: name.into(),
            rect: (x, y, w, h),
            primary,
            refresh: 60,
        }
    }

    fn setup() -> Vec<MonitorInfo> {
        vec![
            m("DISPLAY1", 0, 0, 2752, 1152, true),
            m("DISPLAY2", 400, -1080, 1920, 1080, false),
            m("DISPLAY3", 2752, -600, 1080, 1920, false),
        ]
    }

    #[test]
    fn selects_by_name_number_and_primary() {
        let all = setup();
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let n = |s: Vec<MonitorInfo>| s.iter().map(|m| m.name.clone()).collect::<Vec<_>>();
        assert_eq!(n(select_monitors(&all, &[])), ["DISPLAY1"]);
        assert_eq!(n(select_monitors(&all, &names(&["primary"]))), ["DISPLAY1"]);
        assert_eq!(
            n(select_monitors(&all, &names(&["2", "display3"]))),
            ["DISPLAY2", "DISPLAY3"]
        );
        assert_eq!(
            n(select_monitors(&all, &names(&["DISPLAY9"]))),
            ["DISPLAY1"]
        );
    }

    #[test]
    fn bounding_rect_covers_selection() {
        let all = setup();
        assert_eq!(bounding_rect(&all[..1]), (0, 0, 2752, 1152));
        assert_eq!(bounding_rect(&all[..2]), (0, -1080, 2752, 2232));
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
    /// Исходный внешний размер окна, если правило его изменило (чтобы вернуть при выходе).
    #[serde(default)]
    pub resized_from: Option<(i32, i32)>,
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
            resized_from: None,
        }
    }

    /// Запомнить исходный размер окна, изменённого правилом.
    /// Возвращает `true`, если запись появилась впервые (baseline надо сохранить).
    pub fn set_original_size(&mut self, id: u64, pid: u32, w: i32, h: i32) -> bool {
        match self.entries.iter_mut().find(|e| e.id == id && e.pid == pid) {
            Some(e) if e.resized_from.is_none() => {
                e.resized_from = Some((w, h));
                true
            }
            _ => false,
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
            clear_clip_hwnd(h);
            unsafe {
                if !IsWindow(Some(h)).as_bool() || IsIconic(h).as_bool() || IsZoomed(h).as_bool() {
                    continue;
                }
                let mut pid = 0u32;
                GetWindowThreadProcessId(h, Some(&mut pid));
                if pid != e.pid {
                    continue;
                }
                let ok = match e.resized_from {
                    Some((w, hh)) => SetWindowPos(
                        h,
                        None,
                        e.left,
                        e.top,
                        w,
                        hh,
                        SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
                    ),
                    None => SetWindowPos(h, None, e.left, e.top, 0, 0, flags),
                }
                .is_ok();
                if ok {
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

/// Описание любого окна без отбора по фильтру (для собственных окон DriftWin).
pub fn describe(id: WindowId) -> Option<Managed> {
    let h = hwnd_of(id);
    unsafe {
        if !IsWindow(Some(h)).as_bool() {
            return None;
        }
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    let (border, visible) = measure(h)?;
    Some(Managed {
        id,
        pid,
        exe: process_info(pid).0,
        class: class_of(h),
        title: title_of(h),
        border,
        visible,
    })
}

/// Полный путь к exe процесса.
pub fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n)
            .is_ok();
        let _ = CloseHandle(p);
        ok.then(|| String::from_utf16_lossy(&buf[..n as usize]))
    }
}

/// Командная строка процесса (`NtQueryInformationProcess(ProcessCommandLineInformation)`, Windows 8.1+).
/// `None`, если доступа нет.
pub fn command_line(pid: u32) -> Option<String> {
    use windows::Wdk::System::Threading::{NtQueryInformationProcess, PROCESSINFOCLASS};
    unsafe {
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        // UNICODE_STRING + сама строка в одном буфере
        let mut buf = vec![0u8; 8192];
        let mut ret = 0u32;
        let st = NtQueryInformationProcess(
            p,
            PROCESSINFOCLASS(60),
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut ret,
        );
        let _ = CloseHandle(p);
        if st.0 < 0 {
            return None;
        }
        // struct UNICODE_STRING { u16 Length; u16 MaxLength; (pad) *u16 Buffer }
        let len = u16::from_le_bytes([buf[0], buf[1]]) as usize / 2;
        let ptr = usize::from_le_bytes(buf[8..16].try_into().ok()?) as *const u16;
        let base = buf.as_ptr() as usize;
        // буфер строки лежит внутри нашего массива сразу за структурой
        if (ptr as usize) < base || (ptr as usize) + len * 2 > base + buf.len() {
            return None;
        }
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(
            ptr, len,
        )))
    }
}

/// Запустить программу (`cmdline` — командная строка целиком, как у процесса; пусто — только путь).
/// Возвращает pid.
pub fn launch(path: &str, cmdline: &str) -> Result<u32> {
    let app: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let mut cl: Vec<u16> = if cmdline.trim().is_empty() {
        format!("\"{path}\"")
    } else {
        cmdline.to_string()
    }
    .encode_utf16()
    .chain(Some(0))
    .collect();
    let dir: Option<Vec<u16>> = std::path::Path::new(path).parent().map(|d| {
        d.as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect()
    });
    let si = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            windows::core::PCWSTR(app.as_ptr()),
            Some(PWSTR(cl.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(0),
            None,
            dir.as_ref().map_or(windows::core::PCWSTR::null(), |d| {
                windows::core::PCWSTR(d.as_ptr())
            }),
            &si,
            &mut pi,
        )?;
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
    }
    Ok(pi.dwProcessId)
}

/// Прозрачность окна: `Some(0.1..=1.0)` — установить, `None` — вернуть как было.
/// Окна, у которых уже есть слой (`WS_EX_LAYERED`), не трогаем: их слоем управляет само приложение.
/// Возвращает, поменялось ли окно.
pub fn set_opacity(
    id: WindowId,
    value: Option<f64>,
    we_set_layer: &mut bool,
) -> Result<(), &'static str> {
    let h = hwnd_of(id);
    unsafe {
        if !IsWindow(Some(h)).as_bool() {
            return Err("окна больше нет");
        }
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        match value {
            Some(v) => {
                let layered = ex & WS_EX_LAYERED.0 as isize != 0;
                if layered && !*we_set_layer {
                    return Err("окно использует собственный слой (WS_EX_LAYERED)");
                }
                if !layered {
                    SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
                    *we_set_layer = true;
                }
                let a = (v.clamp(0.1, 1.0) * 255.0).round() as u8;
                SetLayeredWindowAttributes(h, windows::Win32::Foundation::COLORREF(0), a, LWA_ALPHA)
                    .map_err(|_| "SetLayeredWindowAttributes отказал")
            }
            None => {
                if *we_set_layer {
                    SetWindowLongPtrW(h, GWL_EXSTYLE, ex & !(WS_EX_LAYERED.0 as isize));
                    *we_set_layer = false;
                }
                Ok(())
            }
        }
    }
}

/// Пиксели выбранного в `mdc` битмапа как RGBA (`w*h*4`).
unsafe fn read_dib(mdc: HDC, bmp: HBITMAP, w: i32, h: i32) -> Option<Vec<u8>> {
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut px = vec![0u8; (w * h * 4) as usize];
    let lines = unsafe {
        GetDIBits(
            mdc,
            bmp,
            0,
            h as u32,
            Some(px.as_mut_ptr().cast()),
            &mut bi,
            DIB_RGB_COLORS,
        )
    };
    if lines == 0 {
        return None;
    }
    for c in px.as_chunks_mut::<4>().0 {
        c.swap(0, 2);
        c[3] = 255;
    }
    Some(px)
}

const MAX_SHOT_PIXELS: i64 = 64_000_000;

/// Снимок прямоугольника экрана: RGBA-пиксели `w*h*4`.
pub fn capture_rect(x: i32, y: i32, w: i32, h: i32) -> Option<Vec<u8>> {
    if w <= 0 || h <= 0 || i64::from(w) * i64::from(h) > MAX_SHOT_PIXELS {
        return None;
    }
    unsafe {
        let sdc = GetDC(None);
        let mdc = CreateCompatibleDC(Some(sdc));
        let bmp = CreateCompatibleBitmap(sdc, w, h);
        let old = SelectObject(mdc, bmp.into());
        let blt = BitBlt(mdc, 0, 0, w, h, Some(sdc), x, y, SRCCOPY | CAPTUREBLT).is_ok();
        let px = if blt { read_dib(mdc, bmp, w, h) } else { None };
        SelectObject(mdc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mdc);
        ReleaseDC(None, sdc);
        px
    }
}

/// Снимок одного окна (видимая рамка) через `PrintWindow`: работает и для окон за краем экрана и под другими.
/// Возвращает пиксели RGBA и размер.
pub fn capture_window(id: WindowId) -> Option<(Vec<u8>, i32, i32)> {
    let h = hwnd_of(id);
    let (border, vis) = measure(h)?;
    let wr = window_rect(h);
    let (fw, fh) = (wr.right - wr.left, wr.bottom - wr.top);
    if fw <= 0 || fh <= 0 || i64::from(fw) * i64::from(fh) > MAX_SHOT_PIXELS {
        return None;
    }
    unsafe {
        let sdc = GetDC(None);
        let mdc = CreateCompatibleDC(Some(sdc));
        let bmp = CreateCompatibleBitmap(sdc, fw, fh);
        let old = SelectObject(mdc, bmp.into());
        // PW_RENDERFULLCONTENT = 2: окна на DirectComposition и UWP
        let ok = windows::Win32::Storage::Xps::PrintWindow(
            h,
            mdc,
            windows::Win32::Storage::Xps::PRINT_WINDOW_FLAGS(2),
        )
        .as_bool();
        let px = if ok { read_dib(mdc, bmp, fw, fh) } else { None };
        SelectObject(mdc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mdc);
        ReleaseDC(None, sdc);
        let px = px?;
        // обрезать невидимую рамку
        let (x0, y0) = (border.l.max(0), border.t.max(0));
        let (w, hh) = (vis.w.min(fw - x0), vis.h.min(fh - y0));
        if w <= 0 || hh <= 0 {
            return None;
        }
        let mut out = Vec::with_capacity((w * hh * 4) as usize);
        for row in 0..hh {
            let start = (((y0 + row) * fw + x0) * 4) as usize;
            out.extend_from_slice(&px[start..start + (w * 4) as usize]);
        }
        Some((out, w, hh))
    }
}

// ---------- запуск приложений без пути (UWP) и родство процессов ----------

/// AUMID окна (`System.AppUserModel.ID`): есть у UWP/упакованных приложений и у окон с явным идентификатором.
pub fn window_aumid(id: WindowId) -> Option<String> {
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
    use windows::Win32::UI::Shell::PropertiesSystem::{
        IPropertyStore, SHGetPropertyStoreForWindow,
    };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd_of(id)).ok()?;
        let v = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let p = PropVariantToStringAlloc(&v).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0.cast()));
        s.filter(|s| !s.is_empty())
    }
}

/// Запустить приложение по AUMID (`IApplicationActivationManager`). Возвращает pid процесса приложения.
pub fn launch_aumid(aumid: &str) -> Result<u32> {
    use windows::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::UI::Shell::{
        AO_NONE, ApplicationActivationManager, IApplicationActivationManager,
    };
    use windows::core::HSTRING;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let mgr: IApplicationActivationManager =
            CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)?;
        Ok(mgr.ActivateApplication(&HSTRING::from(aumid), &HSTRING::new(), AO_NONE)?)
    }
}

/// Предки процесса (родитель, его родитель, …) — до 8 звеньев. Умерший родитель остаётся в цепочке,
/// пока жив потомок.
pub fn ancestors(pid: u32) -> Vec<u32> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    let mut parents: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return vec![];
        };
        let mut e = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                parents.insert(e.th32ProcessID, e.th32ParentProcessID);
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    let mut out = Vec::new();
    let mut cur = pid;
    while out.len() < 8 {
        match parents.get(&cur) {
            Some(&p) if p != 0 && !out.contains(&p) && p != pid => {
                out.push(p);
                cur = p;
            }
            _ => break,
        }
    }
    out
}

// ---------- панель задач ----------

/// Класс окна верхнего уровня под курсором.
pub fn class_at_cursor() -> String {
    let (x, y) = cursor_pos();
    window_at(x, y).map_or_else(String::new, |id| class_of(hwnd_of(id)))
}

/// Нажата ли левая кнопка мыши.
pub fn left_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) as u16 & 0x8000 != 0 }
}

// ---------- раскладки клавиатуры ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KbLayout {
    /// Идентификатор раскладки (HKL) в шестнадцатеричном виде.
    pub id: String,
    /// Название языка: «русский (Россия)».
    pub name: String,
}

fn layout_name(hkl: isize) -> String {
    use windows::Win32::Globalization::GetLocaleInfoW;
    let langid = (hkl & 0xFFFF) as u32;
    let mut buf = [0u16; 128];
    // LOCALE_SLANGUAGE = 2: название языка на языке интерфейса
    let n = unsafe { GetLocaleInfoW(langid, 2, Some(&mut buf)) };
    if n > 1 {
        String::from_utf16_lossy(&buf[..(n - 1) as usize])
    } else {
        format!("{langid:04x}")
    }
}

fn layout_handles() -> Vec<windows::Win32::UI::Input::KeyboardAndMouse::HKL> {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayoutList;
    let mut v = vec![Default::default(); 64];
    let n = unsafe { GetKeyboardLayoutList(Some(&mut v)) }.max(0) as usize;
    v.truncate(n);
    v
}

/// Установленные раскладки.
pub fn keyboard_layouts() -> Vec<KbLayout> {
    layout_handles()
        .iter()
        .map(|h| {
            let raw = h.0 as isize;
            KbLayout {
                id: format!("{:08x}", raw as usize as u32),
                name: layout_name(raw),
            }
        })
        .collect()
}

fn foreground_hwnd() -> Option<HWND> {
    let h = unsafe { GetForegroundWindow() };
    (!h.0.is_null()).then_some(h)
}

/// Индекс раскладки окна, которое сейчас в фокусе (раскладка у каждого потока своя).
pub fn current_layout() -> Option<usize> {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
    let fg = foreground_hwnd()?;
    let tid = unsafe { GetWindowThreadProcessId(fg, None) };
    let cur = unsafe { GetKeyboardLayout(tid) };
    layout_handles().iter().position(|h| h.0 == cur.0)
}

/// Переключить раскладку окна в фокусе на раскладку с номером `index`.
pub fn set_layout(index: usize) -> Result<()> {
    let hs = layout_handles();
    let hkl = *hs
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("раскладки {index} нет (всего {})", hs.len()))?;
    let fg = foreground_hwnd().ok_or_else(|| anyhow::anyhow!("нет окна в фокусе"))?;
    unsafe {
        PostMessageW(
            Some(fg),
            WM_INPUTLANGCHANGEREQUEST,
            Default::default(),
            LPARAM(hkl.0 as isize),
        )?;
    }
    Ok(())
}

// ---------- оформление DWM (Windows 11) ----------

/// Что было у окна до нашего вмешательства: для возврата.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecoSaved {
    corner: Option<i32>,
    border: Option<u32>,
    backdrop: Option<i32>,
}

const DWMWA_CORNER: i32 = 33;
const DWMWA_BORDER: i32 = 34;
const DWMWA_BACKDROP: i32 = 38;
/// `DWMWA_COLOR_NONE`: рамка не рисуется.
pub const BORDER_NONE: u32 = 0xFFFF_FFFE;

/// Цвет рамки в формате COLORREF (`0x00BBGGRR`).
pub fn colorref(rgb: [u8; 3]) -> u32 {
    u32::from(rgb[0]) | u32::from(rgb[1]) << 8 | u32::from(rgb[2]) << 16
}

fn dwm_get(h: HWND, attr: i32) -> Option<u32> {
    let mut v = 0u32;
    unsafe {
        windows::Win32::Graphics::Dwm::DwmGetWindowAttribute(
            h,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(attr),
            &mut v as *mut _ as *mut _,
            4,
        )
        .ok()?;
    }
    Some(v)
}

fn dwm_set(h: HWND, attr: i32, v: u32) -> bool {
    unsafe {
        windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            h,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(attr),
            &v as *const _ as *const _,
            4,
        )
        .is_ok()
    }
}

/// Привести оформление окна к желаемому: `Some` — установить (запомнив прежнее значение), `None` — вернуть прежнее,
/// если мы его меняли. Системы старше Windows 11 атрибуты отвергают — тогда ничего не меняется.
pub fn set_deco(
    id: WindowId,
    corner: Option<i32>,
    border: Option<u32>,
    backdrop: Option<i32>,
    saved: &mut DecoSaved,
) {
    let h = hwnd_of(id);
    // цвет рамки прочитать нельзя: возвращаем системное значение (`DWMWA_COLOR_DEFAULT`)
    let one = |attr: i32, want: Option<u32>, slot: &mut Option<u32>| match want {
        Some(v) => {
            if slot.is_none() {
                *slot = dwm_get(h, attr).or((attr == DWMWA_BORDER).then_some(0xFFFF_FFFF));
            }
            let _ = dwm_set(h, attr, v);
        }
        None => {
            if let Some(old) = slot.take() {
                let _ = dwm_set(h, attr, old);
            }
        }
    };
    let mut c = saved.corner.map(|v| v as u32);
    one(DWMWA_CORNER, corner.map(|v| v as u32), &mut c);
    saved.corner = c.map(|v| v as i32);
    one(DWMWA_BORDER, border, &mut saved.border);
    let mut b = saved.backdrop.map(|v| v as u32);
    one(DWMWA_BACKDROP, backdrop.map(|v| v as u32), &mut b);
    saved.backdrop = b.map(|v| v as i32);
}

// ---------- автозапуск ----------

const RUN_KEY: windows::core::PCWSTR =
    windows::core::w!(r"Software\Microsoft\Windows\CurrentVersion\Run");

/// Включить автозапуск при входе в систему (`HKCU\...\Run`): `command` — командная строка целиком.
pub fn autostart_enable(name: &str, command: &str) -> Result<()> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    let n: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let v: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
    let err = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            windows::core::PCWSTR(n.as_ptr()),
            REG_SZ.0,
            Some(v.as_ptr().cast()),
            (v.len() * 2) as u32,
        )
    };
    err.ok().map_err(Into::into)
}

/// Выключить автозапуск; отсутствие записи — не ошибка.
pub fn autostart_disable(name: &str) -> Result<()> {
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyValueW};
    let n: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let err = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            windows::core::PCWSTR(n.as_ptr()),
        )
    };
    if err == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    err.ok().map_err(Into::into)
}

/// Командная строка автозапуска, если включён.
pub fn autostart_status(name: &str) -> Option<String> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    let n: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            windows::core::PCWSTR(n.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if err.is_err() {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

#[cfg(test)]
mod autostart_tests {
    use super::*;

    #[test]
    fn autostart_roundtrip_under_test_name() {
        let name = "DriftWinUnitTest";
        autostart_disable(name).unwrap();
        assert_eq!(autostart_status(name), None);
        autostart_enable(name, r#""C:\Program Files\DriftWin\driftwin.exe" --all"#).unwrap();
        assert_eq!(
            autostart_status(name).as_deref(),
            Some(r#""C:\Program Files\DriftWin\driftwin.exe" --all"#)
        );
        autostart_disable(name).unwrap();
        assert_eq!(autostart_status(name), None);
    }
}
