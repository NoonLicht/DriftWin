//! Демон DriftWin, Этапы 1–3: камера, навигация, конфиг и правила для окон из белого списка.
//!
//! Безопасность: без `--only`/`[windows] only` или явного `--all` не запускается; при выходе и по аварийной клавише
//! (Win+Ctrl+Shift+R) окна возвращаются на исходные места; страж возвращает их после падения.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, bail};
use dw_config::Resolved;
use dw_core::action::Action;
use dw_core::nav::{Inertia, Mru, VelocityTracker};
use dw_core::{Dir, ScreenRect, Vec2, WindowId, canvas::Canvas};
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
    only_cli: Option<Vec<String>>,
    all: bool,
    state_dir: PathBuf,
    config_path: PathBuf,
    check_config: bool,
    /// Имена exe (нижний регистр): закрепить на экране.
    pinned: Vec<String>,
    /// Имена exe (нижний регистр): виджеты (остаются на холсте, вне навигации).
    widgets: Vec<String>,
    no_auto_navigate: bool,
}

fn exe_list(v: Option<String>) -> Vec<String> {
    v.unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn env_dir(var: &str) -> PathBuf {
    PathBuf::from(std::env::var(var).unwrap_or_else(|_| ".".into())).join("DriftWin")
}

fn parse_args() -> Result<Opts> {
    let mut o = Opts {
        only_cli: None,
        all: false,
        state_dir: env_dir("LOCALAPPDATA"),
        config_path: env_dir("APPDATA").join("config.toml"),
        check_config: false,
        pinned: vec![],
        widgets: vec![],
        no_auto_navigate: false,
    };
    let mut a = std::env::args().skip(1);
    while let Some(x) = a.next() {
        match x.as_str() {
            "--only" => {
                let v = exe_list(a.next());
                if v.is_empty() {
                    bail!("--only пуст");
                }
                o.only_cli = Some(v);
            }
            "--all" => o.all = true,
            "--pinned" => o.pinned = exe_list(a.next()),
            "--widget" => o.widgets = exe_list(a.next()),
            "--no-auto-navigate" => o.no_auto_navigate = true,
            "--state-dir" => o.state_dir = a.next().map(PathBuf::from).unwrap_or(o.state_dir),
            "--config" => match a.next() {
                Some(p) => o.config_path = PathBuf::from(p),
                None => bail!("--config требует путь"),
            },
            "--check-config" => o.check_config = true,
            other => bail!("неизвестный аргумент {other}"),
        }
    }
    Ok(o)
}

/// Прочитать и проверить конфиг. Нет файла — стандартные настройки.
fn read_config(path: &Path) -> std::result::Result<dw_config::Loaded, Vec<String>> {
    match std::fs::read_to_string(path) {
        Ok(src) => dw_config::parse(&src).map_err(|es| es.iter().map(|e| e.to_string()).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(dw_config::Loaded {
            config: dw_config::Config::default(),
            warnings: vec![],
        }),
        Err(e) => Err(vec![format!(
            "не удалось прочитать {}: {e}",
            path.display()
        )]),
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn filter_from(cfg: &dw_config::Config, only: Option<Vec<String>>) -> Filter {
    Filter {
        only,
        exclude_exe: cfg
            .windows
            .exclude
            .iter()
            .filter_map(|e| e.exe.clone())
            .collect(),
        exclude_class: cfg
            .windows
            .exclude
            .iter()
            .filter_map(|e| e.class.clone())
            .collect(),
    }
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
    cfg: dw_config::Config,
    cfg_path: PathBuf,
    cfg_mtime: Option<SystemTime>,
    cli_pinned: Vec<String>,
    cli_widgets: Vec<String>,
    no_auto_navigate: bool,
    /// Положение камеры до `home-toggle`, чтобы вернуться обратно.
    home_prev: Option<Vec2>,
    force_layout: bool,
    target: Vec2,
    last_move: Instant,
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
            self.adopt_rules(&m);
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

    fn resolve(&self, cfg: &dw_config::Config, m: &Managed) -> Resolved {
        let mut r = cfg.resolve(&m.exe, &m.class, &m.title);
        let exe = m.exe.to_lowercase();
        r.widget |= self.cli_widgets.contains(&exe);
        r.pinned_to_screen |= self.cli_pinned.contains(&exe);
        r
    }

    /// Правила окна при появлении: размер, положение, виджет, закрепление.
    fn adopt_rules(&mut self, m: &Managed) {
        let r = self.resolve(&self.cfg, m);
        let mut vis = m.visible;
        if let Some((w, h)) = r.size {
            dw_win::resize_visible(m, w, h);
            let orig = (
                vis.w + m.border.l + m.border.r,
                vis.h + m.border.t + m.border.b,
            );
            {
                let mut b = self.baseline.lock().unwrap();
                b.set_original_size(m.id.0, m.pid, orig.0, orig.1);
                let _ = b.save(&self.baseline_path);
            }
            vis.w = w;
            vis.h = h;
            self.canvas.adopt(m.id, vis);
            self.last.insert(m.id, vis);
        }
        self.canvas.set_widget(m.id, r.widget);
        if r.pinned_to_screen {
            let rect = match r.position {
                // позиция закреплённого окна: от центра монитора, Y вверх
                Some((x, y)) => {
                    let (ox, oy) = self.canvas.origin;
                    let (mw, mh) = self.canvas.viewport.size_px;
                    ScreenRect {
                        x: ox + (mw as f64 / 2.0 + x - vis.w as f64 / 2.0).round() as i32,
                        y: oy + (mh as f64 / 2.0 - y - vis.h as f64 / 2.0).round() as i32,
                        w: vis.w,
                        h: vis.h,
                    }
                }
                None => vis,
            };
            self.canvas.pin(m.id, rect);
            if rect != vis {
                dw_win::apply(&[(m, rect)]);
            }
            self.last.insert(m.id, rect);
        } else if let Some((x, y)) = r.position {
            self.canvas.set_world_center(m.id, Vec2::new(x, y));
            self.force_layout = true;
        }
    }

    /// Перечитать конфиг. При ошибке остаётся прежний.
    fn reload_config(&mut self) {
        self.cfg_mtime = mtime(&self.cfg_path);
        let loaded = match read_config(&self.cfg_path) {
            Ok(l) => l,
            Err(es) => {
                for e in es {
                    tracing::error!("конфиг не применён: {e}");
                }
                return;
            }
        };
        for w in &loaded.warnings {
            tracing::warn!("конфиг: {w}");
        }
        let new = loaded.config;
        dw_input::set_bindings(new.bindings.clone());
        self.filter.exclude_exe = filter_from(&new, None).exclude_exe;
        self.filter.exclude_class = filter_from(&new, None).exclude_class;
        // правила «постоянно проверяемые»: применяем к открытым окнам только то, что изменилось
        let ids: Vec<_> = self.managed.keys().copied().collect();
        for id in ids {
            let m = self.managed[&id].clone();
            let (old_r, new_r) = (self.resolve(&self.cfg, &m), self.resolve(&new, &m));
            if old_r == new_r {
                continue;
            }
            self.canvas.set_widget(id, new_r.widget);
            if new_r.pinned_to_screen && !self.canvas.is_pinned(id) {
                if let Some(r) = self.canvas.screen_rect(id) {
                    self.canvas.pin(id, r);
                }
            } else if !new_r.pinned_to_screen && old_r.pinned_to_screen {
                self.canvas.unpin(id);
                self.force_layout = true;
            }
        }
        for (n, (x, y)) in &new.navigation.bookmarks {
            self.bookmarks.entry(*n).or_insert(Vec2::new(*x, *y));
        }
        self.cfg = new;
        tracing::info!("конфиг применён");
    }

    fn check_config_file(&mut self) {
        let m = mtime(&self.cfg_path);
        if m != self.cfg_mtime {
            self.reload_config();
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
                    let k = self.cfg.navigation.camera_speed;
                    self.canvas.viewport.center = Vec2::new(c.x + d.x * k, c.y + d.y * k);
                }
            }
        }
        if !self.force_layout && self.canvas.viewport.center == self.applied {
            return;
        }
        self.force_layout = false;
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
        if self.cfg.navigation.auto_navigate
            && !self.no_auto_navigate
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
            Input::Action(a) => self.run_action(a),
            Input::Drag { dx, dy } => {
                let k = self.cfg.navigation.mouse_speed;
                let (dx, dy) = (dx as f64 * k, dy as f64 * k);
                self.inertia.stop();
                self.canvas.pan_px(-dx, -dy);
                self.target = self.canvas.viewport.center;
                self.tracker.push(self.now_secs(), Vec2::new(-dx, dy));
            }
            Input::DragEnd => {
                let v = self.tracker.velocity(self.now_secs());
                self.tracker.clear();
                self.inertia.fling(v);
            }
            Input::Panic => {}
        }
    }

    /// Ближайшее к точке окно в направлении (от фокусного окна или центра камеры).
    fn nearest_from_focus(&self, d: Dir) -> Option<WindowId> {
        let skip = self.focused_nav();
        let from = skip
            .and_then(|id| self.canvas.window_center(id))
            .unwrap_or(self.canvas.viewport.center);
        self.canvas.nearest(from, skip, d)
    }

    fn run_action(&mut self, a: Action) {
        match a {
            Action::PanViewport(d) => {
                self.inertia.stop();
                let step = self.cfg.navigation.pan_step;
                match d {
                    Dir::Left => self.target.x -= step,
                    Dir::Right => self.target.x += step,
                    Dir::Up => self.target.y += step,
                    Dir::Down => self.target.y -= step,
                }
            }
            Action::FocusNearest(d) => {
                if let Some(id) = self.nearest_from_focus(d) {
                    self.mru.touch(id);
                    self.go_to(id);
                }
            }
            Action::CenterNearest(d) => {
                if let Some(id) = self.nearest_from_focus(d) {
                    self.center_on(id);
                }
            }
            Action::CycleWindows(fwd) => {
                if let Some(id) = self.mru.cycle(fwd) {
                    self.last_cycle = Some(Instant::now());
                    self.go_to(id);
                }
            }
            Action::CenterWindow => {
                if let Some(id) = self.focused_nav() {
                    self.center_on(id);
                }
            }
            Action::FocusCenter => {
                let c = self.canvas.viewport.center;
                let best = self.canvas.navigable().min_by(|a, b| {
                    let d = |id: &WindowId| {
                        self.canvas
                            .window_center(*id)
                            .map_or(f64::MAX, |w| (w.x - c.x).hypot(w.y - c.y))
                    };
                    d(a).total_cmp(&d(b))
                });
                if let Some(id) = best {
                    self.mru.touch(id);
                    self.go_to(id);
                }
            }
            Action::HomeToggle => {
                self.inertia.stop();
                let (c, h) = (self.canvas.viewport.center, self.canvas.home());
                if (c.x - h.x).hypot(c.y - h.y) > 1.0 {
                    self.home_prev = Some(c);
                    self.target = h;
                } else if let Some(p) = self.home_prev.take() {
                    self.target = p;
                }
            }
            Action::SetBookmark(n) => {
                self.bookmarks.insert(n, self.canvas.viewport.center);
                tracing::info!("закладка {n} сохранена");
            }
            Action::GoToBookmark(n) => {
                if let Some(p) = self.bookmarks.get(&n).copied() {
                    self.inertia.stop();
                    self.target = p;
                }
            }
            Action::TogglePinToScreen => {
                let Some(id) = dw_win::foreground().filter(|id| self.managed.contains_key(id))
                else {
                    return;
                };
                if self.canvas.is_pinned(id) {
                    self.canvas.unpin(id);
                    self.force_layout = true;
                } else if let Some(r) = self.canvas.screen_rect(id) {
                    self.canvas.pin(id, r);
                }
            }
            Action::CloseWindow => {
                if let Some(m) = dw_win::foreground().and_then(|id| self.managed.get(&id)) {
                    dw_win::close(m);
                }
            }
            Action::ReloadConfig => self.reload_config(),
            Action::Quit => QUIT.store(true, SeqCst),
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
            eprintln!(
                "driftwin: {e}\nпример: driftwin --only notepad.exe,fixture.exe [--config путь] | --check-config"
            );
            std::process::exit(2);
        }
    };
    let loaded = read_config(&opts.config_path);
    if opts.check_config {
        match loaded {
            Ok(l) => {
                for w in &l.warnings {
                    println!("предупреждение: {w}");
                }
                if opts.config_path.exists() {
                    println!("конфиг {} корректен", opts.config_path.display());
                } else {
                    println!(
                        "файла {} нет — будут стандартные настройки",
                        opts.config_path.display()
                    );
                }
                std::process::exit(0);
            }
            Err(es) => {
                for e in es {
                    println!("ошибка: {e}");
                }
                std::process::exit(1);
            }
        }
    }
    let loaded = loaded.unwrap_or_else(|es| {
        for e in es {
            eprintln!("driftwin: конфиг {}: {e}", opts.config_path.display());
        }
        std::process::exit(2);
    });
    for w in &loaded.warnings {
        tracing::warn!("конфиг: {w}");
    }
    let cfg = loaded.config;
    let only = if opts.all {
        None
    } else if let Some(o) = opts.only_cli.clone() {
        Some(o)
    } else if !cfg.windows.only.is_empty() {
        Some(cfg.windows.only.clone())
    } else {
        eprintln!(
            "driftwin: безопасный режим: укажите --only exe1,exe2, [windows] only в конфиге или явно --all"
        );
        std::process::exit(2);
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
    dw_input::set_bindings(cfg.bindings.clone());
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
    let i_reload = MenuItem::new("Перечитать конфиг", true, None);
    let i_quit = MenuItem::new("Выход", true, None);
    menu.append_items(&[&i_pause, &i_back, &i_reload, &i_quit])?;
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
        filter: filter_from(&cfg, only),
        cfg_mtime: mtime(&opts.config_path),
        cfg_path: opts.config_path.clone(),
        bookmarks: cfg
            .navigation
            .bookmarks
            .iter()
            .map(|(n, (x, y))| (*n, Vec2::new(*x, *y)))
            .collect(),
        cfg,
        cli_pinned: opts.pinned,
        cli_widgets: opts.widgets,
        no_auto_navigate: opts.no_auto_navigate,
        home_prev: None,
        force_layout: false,
        target,
        last_move: Instant::now(),
        inertia: Inertia::default(),
        tracker: VelocityTracker::default(),
        mru: Mru::default(),
        epoch: Instant::now(),
        last_tick: Instant::now(),
        applied: target,
        fg_seen: dw_win::foreground(),
        last_cycle: None,
    };
    app.rescan();
    tracing::info!(
        "готово: окон под управлением {}, монитор {w}x{h}, конфиг {} (по умолчанию аварийный возврат — Win+Ctrl+Shift+R)",
        app.canvas.len(),
        opts.config_path.display()
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
            } else if ev.id == *i_reload.id() {
                app.reload_config();
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
                app.check_config_file();
                next_scan = Instant::now() + Duration::from_secs(1);
            }
        }
        std::thread::sleep(Duration::from_millis(
            1000 / u64::from(app.cfg.windows.max_fps),
        ));
    }

    let n = baseline.lock().unwrap().restore();
    tracing::info!("выход: возвращено окон {n}");
    std::fs::write(&clean, b"ok")?;
    Ok(())
}
