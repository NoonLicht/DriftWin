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
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW,
    HDC, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW, MonitorFromPoint,
    SetWindowRgn,
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
