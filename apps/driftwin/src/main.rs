//! Демон DriftWin, Этапы 1–4: камера, навигация, конфиг и правила, привязка и кластеры для окон из белого списка.
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
use dw_core::action::{Action, DragKind};
use dw_core::nav::{Inertia, Mru, VelocityTracker};
use dw_core::snap::{self, Edges, SnapDrag, SnapResize};
use dw_core::{Dir, Rect, ScreenRect, Vec2, WindowId, canvas::Canvas};
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

/// Минимальный размер окна при изменении размера мышью и клавишами, мир.
const MIN_WIN: (f64, f64) = (120.0, 80.0);
/// Через сколько после изменения размера проверяем, что приложение его приняло.
const SIZE_CHECK: Duration = Duration::from_millis(350);

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

/// Перетаскивание, которое сейчас идёт.
enum Drag {
    Pan,
    Move {
        ids: Vec<WindowId>,
        /// Прямоугольники `ids` в начале перетаскивания.
        start: Vec<Rect>,
        /// Остальные окна (неподвижные цели привязки).
        others: Vec<Rect>,
        /// Суммарное смещение курсора в мире (вместе с прокруткой камеры у края).
        raw: Vec2,
        snap: SnapDrag,
    },
    Resize {
        id: WindowId,
        edges: Edges,
        start: Rect,
        raw: Vec2,
        snap: SnapResize,
        /// Раскладка в начале: по ней сдвигаются соседи при изменении размера кластера.
        items: Vec<(WindowId, Rect)>,
        others: Vec<Rect>,
        cluster: bool,
    },
}

fn dir_vec(d: Dir) -> Vec2 {
    match d {
        Dir::Right => Vec2::new(1.0, 0.0),
        Dir::Left => Vec2::new(-1.0, 0.0),
        Dir::Up => Vec2::new(0.0, 1.0),
        Dir::Down => Vec2::new(0.0, -1.0),
    }
}

fn same_rect(a: &Rect, b: &Rect) -> bool {
    (a.center.x - b.center.x).abs() < 2.0
        && (a.center.y - b.center.y).abs() < 2.0
        && (a.w - b.w).abs() < 2.0
        && (a.h - b.h).abs() < 2.0
}

/// Действия, которые применяются к одному окну (в фокусе или под курсором).
fn is_window_action(a: Action) -> bool {
    matches!(
        a,
        Action::NudgeWindow(_)
            | Action::MoveToSnap(_)
            | Action::GrowWindow(_)
            | Action::ShrinkWindow(_)
            | Action::ResizeToSnap(_)
            | Action::FitWindow
            | Action::FitWindowSnapped
            | Action::FillWindow
    )
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
    drag: Option<Drag>,
    /// Окно на весь монитор и его прежний прямоугольник.
    fullscreen: Option<(WindowId, Rect)>,
    /// Для повторного fit/fill: (прямоугольник до, прямоугольник после).
    undo: HashMap<WindowId, (Rect, Rect)>,
    /// Окна, которым меняли размер: проверить, принял ли его процесс.
    size_checks: Vec<(WindowId, Instant)>,
    cursor_pan_on: bool,
    edge_since: Option<Instant>,
    /// Угол, в котором сейчас курсор (0 — левый верхний, по часовой стрелке).
    in_corner: Option<usize>,
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
                    && ((cur.x - prev.x).abs() > 2
                        || (cur.y - prev.y).abs() > 2
                        || (cur.w - prev.w).abs() > 2
                        || (cur.h - prev.h).abs() > 2)
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
            self.undo.remove(&id);
            if self.fullscreen.is_some_and(|f| f.0 == id) {
                self.fullscreen = None;
            }
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
                if b.set_original_size(m.id.0, m.pid, orig.0, orig.1) {
                    let _ = b.save(&self.baseline_path);
                }
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
        if new.navigation.edge_pan.cursor_pan != self.cfg.navigation.edge_pan.cursor_pan {
            self.cursor_pan_on = new.navigation.edge_pan.cursor_pan;
        }
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
        self.drag = None;
        self.fullscreen = None;
        self.undo.clear();
        self.size_checks.clear();
    }

    fn now_secs(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        self.edge_pan(dt);
        self.hot_corners();
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
            // окну, которое уже стоит где надо, команда не нужна (важно при перетаскивании: двигается одно окно)
            .filter(|(id, r)| self.last.get(id) != Some(r))
            .filter_map(|(id, r)| {
                self.managed
                    .get(id)
                    .filter(|m| !dw_win::is_minimized_or_maximized(m))
                    .map(|m| (m, *r))
            })
            .collect();
        let mut moved: Vec<(&Managed, ScreenRect)> = Vec::new();
        let mut sized: Vec<(&Managed, ScreenRect)> = Vec::new();
        for &(m, r) in &items {
            if self
                .last
                .get(&m.id)
                .is_some_and(|p| (p.w, p.h) != (r.w, r.h))
            {
                sized.push((m, r));
            } else {
                moved.push((m, r));
            }
        }
        dw_win::apply(&moved);
        dw_win::apply_sized(&sized);
        for (m, r) in items {
            self.last.insert(m.id, r);
        }
        for (m, _) in sized {
            self.size_checks.retain(|(i, _)| *i != m.id);
            self.size_checks.push((m.id, now + SIZE_CHECK));
        }
    }

    // ---------- перетаскивание, привязка, кластеры ----------

    /// Окно под точкой экрана, если оно под управлением и живёт на холсте.
    fn canvas_window_at(&self, x: i32, y: i32) -> Option<WindowId> {
        dw_win::window_at(x, y).filter(|id| {
            self.managed.contains_key(id)
                && self.canvas.contains(*id)
                && !self.canvas.is_pinned(*id)
                && !self.canvas.is_widget(*id)
        })
    }

    fn begin_drag(&mut self, kind: DragKind, x: i32, y: i32) {
        self.exit_fullscreen();
        self.inertia.stop();
        self.target = self.canvas.viewport.center;
        if kind == DragKind::Pan {
            self.drag = Some(Drag::Pan);
            return;
        }
        let Some(id) = self.canvas_window_at(x, y) else {
            self.drag = None;
            return;
        };
        if dw_win::foreground() != Some(id)
            && let Some(m) = self.managed.get(&id)
        {
            dw_win::focus(m);
        }
        self.fg_seen = Some(id);
        self.mru.touch(id);
        let items = self.canvas.snap_items();
        let gap = self.cfg.snap.gap;
        let Some(rect) = self.canvas.rect(id) else {
            self.drag = None;
            return;
        };
        self.drag = Some(match kind {
            DragKind::MoveWindow | DragKind::MoveCluster => {
                let ids = if kind == DragKind::MoveCluster {
                    snap::cluster_of(&items, id, gap)
                } else {
                    vec![id]
                };
                Drag::Move {
                    start: ids.iter().filter_map(|i| self.canvas.rect(*i)).collect(),
                    others: items
                        .iter()
                        .filter(|(i, _)| !ids.contains(i))
                        .map(|(_, r)| *r)
                        .collect(),
                    ids,
                    raw: Vec2::default(),
                    snap: SnapDrag::default(),
                }
            }
            _ => {
                let cluster = kind == DragKind::ResizeCluster;
                // привязываться к окнам своего же кластера бессмысленно: они сдвигаются вместе с нами
                let mine = if cluster {
                    snap::cluster_of(&items, id, gap)
                } else {
                    vec![id]
                };
                let p = self.canvas.screen_point_to_world(x, y);
                Drag::Resize {
                    id,
                    edges: Edges::nearest_corner(&rect, p),
                    start: rect,
                    raw: Vec2::default(),
                    snap: SnapResize::default(),
                    others: items
                        .iter()
                        .filter(|(i, _)| !mine.contains(i))
                        .map(|(_, r)| *r)
                        .collect(),
                    items,
                    cluster,
                }
            }
        });
    }

    /// Сдвинуть перетаскиваемое на дельту в мире (курсор или прокрутка камеры) и применить привязку.
    fn drag_by(&mut self, dw: Vec2) {
        let cfg = self.cfg.snap;
        let changes = match self.drag.as_mut() {
            Some(Drag::Move {
                ids,
                start,
                others,
                raw,
                snap,
            }) => {
                *raw = *raw + dw;
                let d = snap.update(*raw, start, others, &cfg);
                ids.iter()
                    .zip(start.iter())
                    .map(|(id, r)| {
                        (
                            *id,
                            Rect {
                                center: r.center + d,
                                ..*r
                            },
                        )
                    })
                    .collect()
            }
            Some(Drag::Resize {
                id,
                edges,
                start,
                raw,
                snap,
                items,
                others,
                cluster,
            }) => {
                *raw = *raw + dw;
                let new = snap.update(start, *edges, *raw, others, &cfg, MIN_WIN);
                if *cluster {
                    snap::resize_keep_cluster(items, *id, new, cfg.gap)
                } else {
                    vec![(*id, new)]
                }
            }
            _ => return,
        };
        self.set_rects(changes);
    }

    /// Записать новые прямоугольники окон; изменение размера запоминается в baseline (вернуть при выходе).
    fn set_rects(&mut self, changes: Vec<(WindowId, Rect)>) {
        for (id, r) in changes {
            let Some(old) = self.canvas.rect(id) else {
                continue;
            };
            if old == r {
                continue;
            }
            if (old.w.round(), old.h.round()) != (r.w.round(), r.h.round())
                && let Some(m) = self.managed.get(&id)
            {
                let (w, h) = (
                    old.w as i32 + m.border.l + m.border.r,
                    old.h as i32 + m.border.t + m.border.b,
                );
                let mut b = self.baseline.lock().unwrap();
                if b.set_original_size(m.id.0, m.pid, w, h) {
                    let _ = b.save(&self.baseline_path);
                }
            }
            self.canvas.set_rect(id, r);
        }
        self.force_layout = true;
    }

    fn toggle_fullscreen(&mut self) {
        if self.fullscreen.is_some() {
            self.exit_fullscreen();
            return;
        }
        let Some(id) = self.focused_nav() else {
            return;
        };
        let Some(prev) = self.canvas.rect(id) else {
            return;
        };
        self.inertia.stop();
        self.target = self.canvas.viewport.center;
        self.fullscreen = Some((id, prev));
        let view = self.canvas.view_rect();
        self.set_rects(vec![(id, view)]);
    }

    fn exit_fullscreen(&mut self) {
        if let Some((id, prev)) = self.fullscreen.take() {
            self.set_rects(vec![(id, prev)]);
        }
    }

    /// Первый вызов — применить `fresh`, повторный (окно ещё в этом состоянии) — вернуть прежний.
    fn toggled(&mut self, id: WindowId, cur: Rect, fresh: Rect) -> Rect {
        if let Some((before, after)) = self.undo.get(&id).copied()
            && same_rect(&cur, &after)
        {
            self.undo.remove(&id);
            return before;
        }
        self.undo.insert(id, (cur, fresh));
        fresh
    }

    fn window_action(&mut self, a: Action, id: WindowId) {
        let Some(rect) = self.canvas.rect(id) else {
            return;
        };
        let snap_cfg = self.cfg.snap;
        let nav = &self.cfg.navigation;
        let (nudge, step) = (nav.nudge_step, nav.resize_step);
        let items = self.canvas.snap_items();
        let others: Vec<Rect> = items
            .iter()
            .filter(|(i, _)| *i != id)
            .map(|(_, r)| *r)
            .collect();
        let shifted = |v: Vec2| Rect {
            center: rect.center + v,
            ..rect
        };
        let view = self.canvas.view_rect();
        let changes = match a {
            Action::NudgeWindow(d) => vec![(id, shifted(dir_vec(d) * nudge))],
            Action::MoveToSnap(d) => match snap::next_snap(&rect, &others, d, &snap_cfg) {
                Some(dist) => vec![(id, shifted(dir_vec(d) * dist))],
                None => return,
            },
            Action::GrowWindow(d) | Action::ShrinkWindow(d) => {
                let sign = if matches!(a, Action::GrowWindow(_)) {
                    1.0
                } else {
                    -1.0
                };
                let new = snap::resize_by(
                    &rect,
                    Edges::from_dir(d),
                    dir_vec(d) * (step * sign),
                    MIN_WIN,
                );
                snap::resize_keep_cluster(&items, id, new, snap_cfg.gap)
            }
            Action::ResizeToSnap(d) => match snap::extend_side(&rect, &others, d, &snap_cfg) {
                Some(new) => snap::resize_keep_cluster(&items, id, new, snap_cfg.gap),
                None => return,
            },
            Action::FitWindow | Action::FitWindowSnapped => {
                let fresh = snap::fit_rect(&view, self.cfg.fit_padding);
                let new = self.toggled(id, rect, fresh);
                if a == Action::FitWindowSnapped {
                    snap::resize_keep_cluster(&items, id, new, snap_cfg.gap)
                } else {
                    vec![(id, new)]
                }
            }
            Action::FillWindow => {
                let fresh = snap::fill_rect(&rect, &others, &view, &snap_cfg);
                if same_rect(&fresh, &rect) && !self.undo.contains_key(&id) {
                    return;
                }
                vec![(id, self.toggled(id, rect, fresh))]
            }
            _ => return,
        };
        self.set_rects(changes);
    }

    /// Прокрутка вида у края экрана: при перетаскивании окна и (по желанию) просто курсором.
    fn edge_pan(&mut self, dt: f64) {
        let ep = self.cfg.navigation.edge_pan.clone();
        let dragging = matches!(self.drag, Some(Drag::Move { .. } | Drag::Resize { .. }));
        let by_cursor = self.cursor_pan_on && self.drag.is_none() && self.fullscreen.is_none();
        if !dragging && !by_cursor {
            self.edge_since = None;
            return;
        }
        let zone = if dragging { ep.zone } else { ep.cursor_zone };
        let (cx, cy) = dw_win::cursor_pos();
        let (ox, oy) = self.canvas.origin;
        let (w, h) = (
            self.canvas.viewport.size_px.0 as f64,
            self.canvas.viewport.size_px.1 as f64,
        );
        let (px, py) = ((cx - ox) as f64, (cy - oy) as f64);
        if px < 0.0 || py < 0.0 || px >= w || py >= h {
            self.edge_since = None;
            return;
        }
        let depth = |dist: f64| (1.0 - dist / zone).clamp(0.0, 1.0);
        let sx = depth(w - 1.0 - px) - depth(px);
        let sy = depth(h - 1.0 - py) - depth(py);
        if sx == 0.0 && sy == 0.0 {
            self.edge_since = None;
            return;
        }
        if !dragging {
            let since = *self.edge_since.get_or_insert_with(Instant::now);
            if since.elapsed() < Duration::from_millis(ep.latency_ms) {
                return;
            }
        }
        let speed = |s: f64| {
            if s == 0.0 {
                0.0
            } else {
                s.signum() * (ep.speed_min + (ep.speed_max - ep.speed_min) * s.abs())
            }
        };
        let k = dt * 60.0;
        let (dx, dy) = (speed(sx) * k, speed(sy) * k);
        self.inertia.stop();
        self.canvas.pan_px(dx, dy);
        self.target = self.canvas.viewport.center;
        if dragging {
            // окно едет вместе с камерой и остаётся под курсором
            let z = self.canvas.viewport.zoom;
            self.drag_by(Vec2::new(dx / z, -dy / z));
        }
    }

    /// Горячие углы: действие срабатывает один раз при входе курсора в угол.
    fn hot_corners(&mut self) {
        let hc = self.cfg.hot_corners.clone();
        let acts = [hc.top_left, hc.top_right, hc.bottom_right, hc.bottom_left];
        if acts.iter().all(Option::is_none) || self.drag.is_some() || self.fullscreen.is_some() {
            self.in_corner = None;
            return;
        }
        let (cx, cy) = dw_win::cursor_pos();
        let (ox, oy) = self.canvas.origin;
        let (w, h) = self.canvas.viewport.size_px;
        let (px, py) = (cx - ox, cy - oy);
        let inside = (0..w as i32).contains(&px) && (0..h as i32).contains(&py);
        let (l, r) = (px <= hc.threshold, px >= w as i32 - 1 - hc.threshold);
        let (t, b) = (py <= hc.threshold, py >= h as i32 - 1 - hc.threshold);
        let now = match (inside, l, r, t, b) {
            (true, true, _, true, _) => Some(0),
            (true, _, true, true, _) => Some(1),
            (true, _, true, _, true) => Some(2),
            (true, true, _, _, true) => Some(3),
            _ => None,
        };
        if now != self.in_corner {
            self.in_corner = now;
            if let Some(a) = now.and_then(|i| acts[i]) {
                self.run_action(a);
            }
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
            && self.drag.is_none()
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
            Input::DragStart { kind, x, y } => self.begin_drag(kind, x, y),
            Input::Drag { dx, dy } => match self.drag {
                Some(Drag::Pan) => {
                    let k = self.cfg.navigation.mouse_speed;
                    let (dx, dy) = (dx as f64 * k, dy as f64 * k);
                    self.inertia.stop();
                    self.canvas.pan_px(-dx, -dy);
                    self.target = self.canvas.viewport.center;
                    self.tracker.push(self.now_secs(), Vec2::new(-dx, dy));
                }
                Some(_) => {
                    let z = self.canvas.viewport.zoom;
                    self.drag_by(Vec2::new(dx as f64 / z, -(dy as f64) / z));
                }
                None => {}
            },
            Input::DragEnd => {
                if matches!(self.drag.take(), Some(Drag::Pan)) {
                    let v = self.tracker.velocity(self.now_secs());
                    self.tracker.clear();
                    self.inertia.fling(v);
                }
            }
            Input::Click { action, x, y } => {
                if is_window_action(action) {
                    self.exit_fullscreen();
                    if let Some(id) = self.canvas_window_at(x, y) {
                        self.window_action(action, id);
                    }
                } else {
                    self.run_action(action);
                }
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
        // любое действие с холстом выводит окно из полноэкранного режима
        if !matches!(
            a,
            Action::ToggleFullscreen
                | Action::CloseWindow
                | Action::Quit
                | Action::ReloadConfig
                | Action::ToggleCursorPan
                | Action::GapIncrease
                | Action::GapDecrease
        ) {
            self.exit_fullscreen();
        }
        match a {
            a if is_window_action(a) => {
                if let Some(id) = self.focused_nav() {
                    self.window_action(a, id);
                }
            }
            Action::ToggleFullscreen => self.toggle_fullscreen(),
            Action::GapIncrease | Action::GapDecrease => {
                let s = &mut self.cfg.snap;
                let d = if a == Action::GapIncrease {
                    s.gap_step
                } else {
                    -s.gap_step
                };
                let old = s.gap;
                s.gap = (old + d).clamp(0.0, 200.0);
                let new = s.gap;
                tracing::info!("зазор привязки: {new} px");
                if new != old {
                    let items = self.canvas.snap_items();
                    let changes = snap::regap(&items, old, new);
                    self.set_rects(changes);
                }
            }
            Action::ToggleCursorPan => {
                self.cursor_pan_on = !self.cursor_pan_on;
                tracing::info!(
                    "прокрутка вида курсором у края: {}",
                    if self.cursor_pan_on {
                        "вкл"
                    } else {
                        "выкл"
                    }
                );
            }
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
            // остальные варианты обработаны выше (действия над окном)
            _ => {}
        }
    }

    /// Периодические дела: фиксация выбора MRU-переключателя.
    fn housekeeping(&mut self) {
        // приложение могло не принять размер (минимум, фиксированный размер): берём фактический
        if self.drag.is_none() && !self.size_checks.is_empty() {
            let now = Instant::now();
            let due: Vec<_> = self
                .size_checks
                .iter()
                .filter(|(_, t)| *t <= now)
                .map(|(i, _)| *i)
                .collect();
            self.size_checks.retain(|(_, t)| *t > now);
            for id in due {
                let (Some(m), Some(r)) = (self.managed.get(&id), self.canvas.rect(id)) else {
                    continue;
                };
                if let Some(cur) = dw_win::current_visible(m)
                    && ((cur.w - r.w.round() as i32).abs() > 2
                        || (cur.h - r.h.round() as i32).abs() > 2)
                {
                    tracing::debug!(
                        "«{}» не приняло размер {}x{}, фактический {}x{}",
                        m.title,
                        r.w,
                        r.h,
                        cur.w,
                        cur.h
                    );
                    self.canvas.adopt(id, cur);
                    self.last.insert(id, cur);
                }
            }
        }
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
        drag: None,
        fullscreen: None,
        undo: HashMap::new(),
        size_checks: Vec::new(),
        cursor_pan_on: false,
        edge_since: None,
        in_corner: None,
    };
    app.cursor_pan_on = app.cfg.navigation.edge_pan.cursor_pan;
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
