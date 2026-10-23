//! Демон DriftWin, Этапы 1–2: камера и навигация для окон из белого списка.
//!
//! Безопасность: без `--only` или `--all` не запускается; при выходе и по аварийной клавише
//! (Win+Ctrl+Shift+R) окна возвращаются на исходные места; страж возвращает их после падения.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use dw_core::nav::{Inertia, Mru, VelocityTracker};
use dw_core::{Dir, Vec2, WindowId, canvas::Canvas};
use dw_input::Input;
use dw_win::{Baseline, Filter, Managed};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler,
};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

const PAN_STEP: f64 = 100.0;
const CAMERA_LERP: f64 = 0.3;
/// Через сколько без нажатий переключатель MRU фиксирует выбор.
const CYCLE_COMMIT: Duration = Duration::from_millis(1500);

static QUIT: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn ctrl_handler(ev: u32) -> windows::core::BOOL {
    if matches!(ev, CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT) {
        QUIT.store(true, SeqCst);
        // дать главному циклу время вернуть окна до завершения процесса
        std::thread::sleep(Duration::from_secs(3));
        return true.into();
    }
    false.into()
}

fn on_desktop(r: &dw_core::ScreenRect, vs: (i32, i32, i32, i32)) -> bool {
    r.x < vs.0 + vs.2 && r.x + r.w > vs.0 && r.y < vs.1 + vs.3 && r.y + r.h > vs.1
}

struct Opts {
    filter: Filter,
    state_dir: PathBuf,
    /// Имена exe (нижний регистр), закреплённые на экране.
    pinned: Vec<String>,
    /// Имена exe, закреплённые и исключённые из навигации.
    widgets: Vec<String>,
    auto_navigate: bool,
}

fn exe_list(v: Option<String>) -> Vec<String> {
    v.unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_args() -> Result<Opts> {
    let mut only = None;
    let mut all = false;
    let mut dir = None;
    let mut pinned = vec![];
    let mut widgets = vec![];
    let mut auto_navigate = true;
    let mut a = std::env::args().skip(1);
    while let Some(x) = a.next() {
        match x.as_str() {
            "--only" => {
                let v = a.next().unwrap_or_default();
                only = Some(
                    v.split(',')
                        .map(|s| s.trim().to_lowercase())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>(),
                );
            }
            "--all" => all = true,
            "--pinned" => pinned = exe_list(a.next()),
            "--widget" => widgets = exe_list(a.next()),
            "--no-auto-navigate" => auto_navigate = false,
            "--state-dir" => dir = a.next().map(PathBuf::from),
            other => bail!("неизвестный аргумент {other}"),
        }
    }
    if only.is_none() && !all {
        bail!("безопасный режим: укажите --only exe1,exe2 (белый список) или явно --all");
    }
    if only.as_ref().is_some_and(|o| o.is_empty()) {
        bail!("--only пуст");
    }
    let state_dir = dir.unwrap_or_else(|| {
        PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into())).join("DriftWin")
    });
    Ok(Opts {
        filter: Filter {
            only: if all { None } else { only },
        },
        state_dir,
        pinned,
        widgets,
        auto_navigate,
    })
}

fn tray_icon() -> Icon {
    let mut px = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for x in 0..16 {
            let edge = x == 0 || y == 0 || x == 15 || y == 15;
            px.extend_from_slice(if edge {
                &[255, 255, 255, 255]
            } else {
                &[40, 120, 220, 255]
            });
        }
    }
    Icon::from_rgba(px, 16, 16).expect("icon")
}

struct App {
    canvas: Canvas,
    managed: HashMap<WindowId, Managed>,
    last: HashMap<WindowId, dw_core::ScreenRect>,
    baseline: Arc<Mutex<Baseline>>,
    baseline_path: PathBuf,
    filter: Filter,
    target: Vec2,
    last_move: Instant,
    pinned: Vec<String>,
    widgets: Vec<String>,
    auto_navigate: bool,
    inertia: Inertia,
    tracker: VelocityTracker,
    mru: Mru,
    bookmarks: HashMap<u8, Vec2>,
    epoch: Instant,
    last_tick: Instant,
    /// Положение камеры, для которого окна были расставлены в последний раз.
    applied: Vec2,
    /// Последнее увиденное окно в фокусе (чтобы реагировать только на смену).
    fg_seen: Option<WindowId>,
    last_cycle: Option<Instant>,
}

impl App {
    fn rescan(&mut self) {
        let (found, skipped) = dw_win::scan(&self.filter);
        // после последнего сдвига даём асинхронным SetWindowPos примениться, иначе примем отставание за ручное перемещение
        let idle = self.target == self.canvas.viewport.center
            && self.last_move.elapsed() > Duration::from_millis(500);
        for m in found {
            if self.canvas.contains(m.id) {
                // пользователь сам подвинул окно -> новое положение на холсте
                if idle
                    && let (Some(cur), Some(prev)) =
                        (dw_win::current_visible(&m), self.last.get(&m.id))
                    && ((cur.x - prev.x).abs() > 2 || (cur.y - prev.y).abs() > 2)
                {
                    self.canvas.adopt(m.id, cur);
                    self.last.insert(m.id, cur);
                }
                continue;
            }
            self.canvas.adopt(m.id, m.visible);
            self.last.insert(m.id, m.visible);
            let home = self.canvas.home_rect(m.id).unwrap_or(m.visible);
            let exe = m.exe.to_lowercase();
            if self.widgets.contains(&exe) {
                self.canvas.pin(m.id, m.visible, true);
            } else if self.pinned.contains(&exe) {
                self.canvas.pin(m.id, m.visible, false);
            }
            tracing::info!("взято под управление: {} «{}»", m.exe, m.title);
            {
                let mut b = self.baseline.lock().unwrap();
                if !b.entries.iter().any(|e| e.id == m.id.0 && e.pid == m.pid) {
                    b.entries.push(Baseline::entry_for(&m, home));
                    if let Err(e) = b.save(&self.baseline_path) {
                        tracing::error!("baseline не записан: {e}");
                    }
                }
            }
            self.managed.insert(m.id, m);
        }
        let dead: Vec<_> = self
            .managed
            .values()
            .filter(|m| !dw_win::is_alive(m))
            .map(|m| m.id)
            .collect();
        for id in dead {
            self.managed.remove(&id);
            self.canvas.forget(id);
            self.mru.remove(id);
            self.last.remove(&id);
        }
        for s in skipped {
            tracing::debug!("не управляется: {} «{}» — {}", s.exe, s.title, s.reason);
        }
    }

    fn reset(&mut self) {
        let (ox, oy, w, h) = dw_win::primary_monitor();
        self.canvas = Canvas::new((ox, oy), (w, h));
        self.target = self.canvas.viewport.center;
        self.managed.clear();
        self.last.clear();
        self.inertia.stop();
        self.mru = Mru::default();
        self.applied = self.canvas.viewport.center;
    }

    fn now_secs(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        if self.inertia.active() {
            let d = self.inertia.step(dt);
            let c = self.canvas.viewport.center;
            self.canvas.viewport.center = Vec2::new(c.x + d.x, c.y + d.y);
            self.target = self.canvas.viewport.center;
        } else {
            let c = self.canvas.viewport.center;
            let d = self.target - c;
            if d.x != 0.0 || d.y != 0.0 {
                if d.x.abs() < 0.5 && d.y.abs() < 0.5 {
                    self.canvas.viewport.center = self.target;
                } else {
                    self.canvas.viewport.center =
                        Vec2::new(c.x + d.x * CAMERA_LERP, c.y + d.y * CAMERA_LERP);
                }
            }
        }
        if self.canvas.viewport.center == self.applied {
            return;
        }
        self.applied = self.canvas.viewport.center;
        self.last_move = now;
        let vs = dw_win::virtual_screen();
        let layout = self.canvas.layout();
        let items: Vec<_> = layout
            .iter()
            .filter(|(id, _)| !self.canvas.is_pinned(*id))
            // окно было и остаётся за пределами экранов — не трогаем (меньше работы для DWM и приложений)
            .filter(|(id, r)| {
                self.last
                    .get(id)
                    .is_none_or(|p| on_desktop(p, vs) || on_desktop(r, vs))
            })
            .filter_map(|(id, r)| {
                self.managed
                    .get(id)
                    .filter(|m| !dw_win::is_minimized_or_maximized(m))
                    .map(|m| (m, *r))
            })
            .collect();
        dw_win::apply(&items);
        for (m, r) in items {
            self.last.insert(m.id, r);
        }
    }

    /// Окно в фокусе, если оно под управлением и участвует в навигации.
    fn focused_nav(&self) -> Option<WindowId> {
        let id = dw_win::foreground()?;
        (self.managed.contains_key(&id) && !self.canvas.is_pinned(id)).then_some(id)
    }

    /// Подвести камеру к окну.
    fn center_on(&mut self, id: WindowId) {
        if let Some(c) = self.canvas.window_center(id) {
            self.inertia.stop();
            self.target = c;
        }
    }

    /// Дать фокус окну и подвести к нему камеру.
    fn go_to(&mut self, id: WindowId) {
        if let Some(m) = self.managed.get(&id) {
            if !dw_win::focus(m) {
                tracing::warn!("фокус на «{}» не получен (ОС отказала)", m.title);
            }
            self.fg_seen = Some(id);
        }
        self.center_on(id);
    }

    /// Реакция на смену окна в фокусе: MRU и автонавигация к окну вне экрана.
    fn track_focus(&mut self) {
        let fg = dw_win::foreground();
        if fg == self.fg_seen {
            return;
        }
        self.fg_seen = fg;
        let Some(id) = fg.filter(|id| self.managed.contains_key(id)) else {
            return;
        };
        if self.canvas.is_pinned(id) {
            return;
        }
        self.mru.touch(id);
        if self.auto_navigate
            && let Some(r) = self.canvas.screen_rect(id)
        {
            let (ox, oy) = self.canvas.origin;
            let (w, h) = self.canvas.viewport.size_px;
            let inside =
                r.x >= ox && r.y >= oy && r.x + r.w <= ox + w as i32 && r.y + r.h <= oy + h as i32;
            if !inside {
                self.center_on(id);
            }
        }
    }

    fn handle(&mut self, ev: Input) {
        match ev {
            Input::Pan(d) => {
                self.inertia.stop();
                match d {
                    Dir::Left => self.target.x -= PAN_STEP,
                    Dir::Right => self.target.x += PAN_STEP,
                    Dir::Up => self.target.y += PAN_STEP,
                    Dir::Down => self.target.y -= PAN_STEP,
                }
            }
            Input::Drag { dx, dy } => {
                self.inertia.stop();
                self.canvas.pan_px(-(dx as f64), -(dy as f64));
                self.target = self.canvas.viewport.center;
                self.tracker
                    .push(self.now_secs(), Vec2::new(-(dx as f64), dy as f64));
            }
            Input::DragEnd => {
                let v = self.tracker.velocity(self.now_secs());
                self.tracker.clear();
                self.inertia.fling(v);
            }
            Input::Focus(d) => {
                let skip = self.focused_nav();
                let from = skip
                    .and_then(|id| self.canvas.window_center(id))
                    .unwrap_or(self.canvas.viewport.center);
                if let Some(id) = self.canvas.nearest(from, skip, d) {
                    self.mru.touch(id);
                    self.go_to(id);
                }
            }
            Input::Cycle(fwd) => {
                if let Some(id) = self.mru.cycle(fwd) {
                    self.last_cycle = Some(Instant::now());
                    self.go_to(id);
                }
            }
            Input::Center => {
                if let Some(id) = self.focused_nav() {
                    self.center_on(id);
                }
            }
            Input::Home => {
                self.inertia.stop();
                self.target = self.canvas.home();
            }
            Input::BookmarkSet(n) => {
                self.bookmarks.insert(n, self.canvas.viewport.center);
                tracing::info!("закладка {n} сохранена");
            }
            Input::BookmarkGo(n) => {
                if let Some(p) = self.bookmarks.get(&n).copied() {
                    self.inertia.stop();
                    self.target = p;
                }
            }
            Input::Panic => {}
        }
    }

    /// Периодические дела: фиксация выбора MRU-переключателя.
    fn housekeeping(&mut self) {
        if let Some(t) = self.last_cycle
            && t.elapsed() > CYCLE_COMMIT
        {
            self.mru.commit();
            self.last_cycle = None;
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("driftwin: {e}\nпример: driftwin --only notepad.exe,fixture.exe");
            std::process::exit(2);
        }
    };
    dw_win::init_dpi();

    unsafe {
        let _mutex = CreateMutexW(None, true, w!("Local\\DriftWin-single-instance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            eprintln!("driftwin уже запущен");
            std::process::exit(1);
        }
        SetConsoleCtrlHandler(Some(ctrl_handler), true)?;
    }

    std::fs::create_dir_all(&opts.state_dir)?;
    let baseline_path = opts.state_dir.join("baseline.json");
    let clean = opts.state_dir.join("clean_exit");
    let _ = std::fs::remove_file(&clean);

    let baseline = Arc::new(Mutex::new(Baseline {
        version: 1,
        entries: vec![],
    }));
    let paused = Arc::new(AtomicBool::new(false));

    // страж
    if let Ok(me) = std::env::current_exe()
        && let Some(guard) = me
            .parent()
            .map(|p| p.join("driftwin-guard.exe"))
            .filter(|p| p.exists())
    {
        let _ = std::process::Command::new(guard)
            .arg(unsafe { GetCurrentProcessId() }.to_string())
            .arg(&opts.state_dir)
            .spawn();
    } else {
        tracing::warn!("driftwin-guard.exe не найден рядом с демоном — защиты от падения нет");
    }

    // ввод
    let (tx, rx) = crossbeam_channel::unbounded();
    let (b2, p2) = (baseline.clone(), paused.clone());
    let _hook = dw_input::spawn(
        tx,
        Arc::new(move || {
            p2.store(true, SeqCst);
            let n = b2.lock().map(|b| b.restore()).unwrap_or(0);
            tracing::warn!("АВАРИЙНЫЙ ВОЗВРАТ: окон возвращено {n}, управление на паузе");
        }),
    );

    // трей
    let menu = Menu::new();
    let i_pause = MenuItem::new("Пауза / продолжить", true, None);
    let i_back = MenuItem::new("Вернуть окна на места", true, None);
    let i_quit = MenuItem::new("Выход", true, None);
    menu.append_items(&[&i_pause, &i_back, &i_quit])?;
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("DriftWin")
        .with_icon(tray_icon())
        .build()?;

    let (ox, oy, w, h) = dw_win::primary_monitor();
    let canvas = Canvas::new((ox, oy), (w, h));
    let target = canvas.viewport.center;
    let mut app = App {
        canvas,
        managed: HashMap::new(),
        last: HashMap::new(),
        baseline: baseline.clone(),
        baseline_path,
        filter: opts.filter,
        target,
        last_move: Instant::now(),
        pinned: opts.pinned,
        widgets: opts.widgets,
        auto_navigate: opts.auto_navigate,
        inertia: Inertia::default(),
        tracker: VelocityTracker::default(),
        mru: Mru::default(),
        bookmarks: HashMap::new(),
        epoch: Instant::now(),
        last_tick: Instant::now(),
        applied: target,
        fg_seen: dw_win::foreground(),
        last_cycle: None,
    };
    app.rescan();
    tracing::info!(
        "готово: окон под управлением {}, монитор {w}x{h}. Win+Ctrl+стрелки/мышь — камера, Win+Alt+стрелки — фокус, Win+Ctrl+Tab — MRU, Win+Ctrl+C/H — центр/дом, Win+Ctrl+[Shift+]1..9 — закладки, Win+Ctrl+Shift+R — аварийный возврат",
        app.canvas.len()
    );

    let mut next_scan = Instant::now() + Duration::from_secs(1);
    let mut was_paused = false;
    while !QUIT.load(SeqCst) {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if ev.id == *i_quit.id() {
                QUIT.store(true, SeqCst);
            } else if ev.id == *i_pause.id() {
                paused.fetch_xor(true, SeqCst);
                if paused.load(SeqCst) {
                    baseline.lock().unwrap().restore();
                }
            } else if ev.id == *i_back.id() {
                baseline.lock().unwrap().restore();
            }
        }
        let is_paused = paused.load(SeqCst);
        dw_input::set_enabled(!is_paused);
        if is_paused != was_paused {
            was_paused = is_paused;
            if !is_paused {
                app.reset();
                app.rescan();
                tracing::info!("продолжение: камера сброшена в начало");
            }
        }
        while let Ok(ev) = rx.try_recv() {
            if !is_paused {
                app.handle(ev);
            }
        }
        if !is_paused {
            app.track_focus();
            app.tick();
            app.housekeeping();
            if Instant::now() >= next_scan {
                app.rescan();
                next_scan = Instant::now() + Duration::from_secs(1);
            }
        }
        std::thread::sleep(Duration::from_millis(16));
    }

    let n = baseline.lock().unwrap().restore();
    tracing::info!("выход: возвращено окон {n}");
    std::fs::write(&clean, b"ok")?;
    Ok(())
}
