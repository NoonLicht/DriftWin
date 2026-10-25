//! Этап 7: заглушки (`suspend-window`, `suspend_on_close`), сессия (`session.json`), IPC и файл состояния.

use dw_core::session::{self, Entry, Session};
use dw_ipc::{Bus, Call, LayoutOp, Request, Response, Selector, ShotTarget};
use serde_json::json;
use std::collections::HashSet;

use super::*;

/// Как запускать приложение заново.
#[derive(Clone)]
pub(crate) struct Launch {
    pub path: String,
    pub cmdline: String,
    /// AUMID упакованного приложения; пусто — обычная программа.
    pub aumid: String,
}

impl Launch {
    /// Упакованные приложения (UWP, Microsoft Store) нельзя запустить по пути к exe: запускаем по AUMID.
    fn by_aumid(&self) -> bool {
        !self.aumid.is_empty()
            && (self.path.is_empty() || self.path.to_lowercase().contains("\\windowsapps\\"))
    }

    fn can_start(&self) -> bool {
        self.by_aumid() || !self.path.is_empty()
    }

    fn start(&self) -> anyhow::Result<u32> {
        if self.by_aumid() {
            dw_win::launch_aumid(&self.aumid)
        } else {
            dw_win::launch(&self.path, &self.cmdline)
        }
    }
}

/// Заглушка на холсте.
#[derive(Clone)]
pub(crate) struct StubInfo {
    /// Имя exe (`app_id`).
    pub app: String,
    /// Надпись на заглушке.
    pub name: String,
    pub title: String,
    pub launch: Launch,
    /// Запущено: когда и какой pid.
    pub launching: Option<(Instant, u32)>,
}

impl StubInfo {
    fn new(app: &str, title: &str, launch: Launch) -> Self {
        Self {
            app: app.to_string(),
            name: stub_name(app, title),
            title: title.to_string(),
            launch,
            launching: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct Counters {
    pub ticks: u64,
    pub layouts: u64,
    pub rescans: u64,
    pub ipc_calls: u64,
    pub session_saves: u64,
    pub state_publishes: u64,
    pub stubs_created: u64,
    pub stubs_launched: u64,
}

pub(crate) struct Ext {
    /// Как запустить окно заново; снимается при появлении окна, пока процесс ещё жив.
    pub launch: HashMap<WindowId, Launch>,
    pub stubs: HashMap<WindowId, StubInfo>,
    /// Окна, которые закрываем сами (`close-window`, `msg close`): закрытие не превращается в заглушку.
    pub closing: HashMap<WindowId, Instant>,
    /// Окна, которым послали закрытие ради заглушки.
    pub suspending: Vec<(WindowId, Instant)>,
    pub modes: HashMap<WindowId, &'static str>,
    /// Окна, которым мы включили слой прозрачности.
    pub opacity: HashSet<WindowId>,
    pub session_path: PathBuf,
    pub state_path: PathBuf,
    pub saved_wins: String,
    pub saved_cam: (f64, f64),
    pub session_due: Option<Instant>,
    pub session_check: Instant,
    pub ipc_rx: Option<crossbeam_channel::Receiver<Call>>,
    pub bus: Arc<Bus>,
    pub paused: Arc<AtomicBool>,
    pub state_next: Instant,
    pub state_txt: String,
    pub counters: Counters,
    /// Когда последний раз левая кнопка была нажата на панели задач или её миниатюрах.
    pub taskbar_click: Option<Instant>,
    pub deco: crate::deco::DecoState,
}

impl Ext {
    pub(crate) fn new(state_dir: &Path, paused: Arc<AtomicBool>) -> Self {
        Self {
            launch: HashMap::new(),
            stubs: HashMap::new(),
            closing: HashMap::new(),
            suspending: Vec::new(),
            modes: HashMap::new(),
            opacity: HashSet::new(),
            session_path: state_dir.join("session.json"),
            state_path: state_dir.join("state.txt"),
            saved_wins: String::new(),
            saved_cam: (0.0, 0.0),
            session_due: None,
            session_check: Instant::now(),
            ipc_rx: None,
            bus: Arc::new(Bus::default()),
            paused,
            state_next: Instant::now(),
            state_txt: String::new(),
            counters: Counters::default(),
            taskbar_click: None,
            deco: Default::default(),
        }
    }
}

/// Окнами этих процессов владеет оболочка, а не само приложение: имя заглушки берём из заголовка окна.
const FRAME_HOSTS: &[&str] = &["applicationframehost.exe"];
/// Классы окон панели задач и её миниатюр: клик по ним — закрытие пользователем, а не приложением.
const TASKBAR_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "TaskListThumbnailWnd",
    "TaskListOverlayWnd",
    "XamlExplorerHostIslandWindow",
    "TopLevelWindowForOverflowXamlIsland",
];
/// Сколько после клика по панели задач закрытие окна считается сделанным оттуда.
const TASKBAR_CLOSE_WINDOW: Duration = Duration::from_millis(2500);

/// Сколько ждём, что приложение закроется само после `WM_CLOSE`, прежде чем отказаться от заглушки.
const SUSPEND_WAIT: Duration = Duration::from_secs(8);
/// Окно запущенной заглушки сопоставляется по pid столько времени…
const LAUNCH_PID_WINDOW: Duration = Duration::from_secs(30);
/// …а по имени приложения — столько (запасной вариант).
const LAUNCH_NAME_WINDOW: Duration = Duration::from_secs(5);

fn display_name(exe: &str) -> String {
    let base = exe
        .strip_suffix(".exe")
        .or(exe.strip_suffix(".EXE"))
        .unwrap_or(exe);
    let mut c = base.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// Надпись на заглушке: для окон-оболочек (UWP) — заголовок окна, иначе имя exe.
fn stub_name(app: &str, title: &str) -> String {
    if FRAME_HOSTS.contains(&app.to_lowercase().as_str()) && !title.trim().is_empty() {
        title.trim().to_string()
    } else {
        display_name(app)
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl App {
    // ---------- заглушки ----------

    /// Запомнить, как запускать приложение окна (после его смерти спросить уже не у кого).
    pub(crate) fn capture_launch(&mut self, m: &Managed) {
        if m.class == dw_win::STUB_CLASS {
            return;
        }
        let aumid = dw_win::window_aumid(m.id).unwrap_or_default();
        // окно-оболочка (UWP): процесс владельца — не приложение, путь и командная строка бесполезны
        let framed = FRAME_HOSTS.contains(&m.exe.to_lowercase().as_str());
        let (path, cmdline) = if framed {
            (String::new(), String::new())
        } else {
            (
                dw_win::image_path(m.pid).unwrap_or_default(),
                dw_win::command_line(m.pid).unwrap_or_default(),
            )
        };
        let launch = Launch {
            path,
            cmdline,
            aumid,
        };
        if launch.can_start() {
            self.ext.launch.insert(m.id, launch);
        }
    }

    fn suspend_on_close_for(&self, m: &Managed) -> bool {
        self.resolve(&self.cfg, m)
            .suspend_on_close
            .unwrap_or(self.cfg.session.suspend_on_close)
    }

    /// Закрыть окно и оставить на его месте заглушку (она появится, когда окно исчезнет).
    pub(crate) fn suspend_window(&mut self, id: WindowId) -> Result<(), String> {
        let m = self.managed.get(&id).ok_or("окно не под управлением")?;
        if self.ext.stubs.contains_key(&id) {
            return Err("это уже заглушка".into());
        }
        if self.canvas.is_pinned(id) || self.fullscreen.is_some_and(|f| f.0 == id) {
            return Err("закреплённое или полноэкранное окно нельзя приостановить".into());
        }
        if !self.ext.launch.contains_key(&id) {
            tracing::warn!(
                "«{}»: не знаю, как запустить заново — окно закрывается без заглушки",
                m.title
            );
        }
        self.ext.closing.insert(id, Instant::now());
        self.ext.suspending.push((id, Instant::now()));
        dw_win::close(m);
        Ok(())
    }

    /// Создать заглушку по прямоугольнику в мире.
    pub(crate) fn make_stub(&mut self, info: StubInfo, rect: Rect) -> Option<WindowId> {
        let vp = self.canvas.viewport;
        let c = vp.world_to_screen(rect.center);
        let (w, h) = (rect.w.round() as i32, rect.h.round() as i32);
        let w = w.max(dw_render::STUB_MIN_SIDE);
        let h = h.max(dw_render::STUB_MIN_SIDE);
        let x = (self.canvas.origin.0 + (c.x - f64::from(w) / 2.0).round() as i32)
            .clamp(-dw_win::PARK_LIMIT, dw_win::PARK_LIMIT);
        let y = (self.canvas.origin.1 + (c.y - f64::from(h) / 2.0).round() as i32)
            .clamp(-dw_win::PARK_LIMIT, dw_win::PARK_LIMIT);
        let key = dw_render::create_stub(&info.name, (x, y, w, h))?;
        let id = WindowId(key);
        let Some(mut m) = dw_win::describe(id) else {
            dw_render::destroy_stub(key);
            return None;
        };
        // DWM уточняет невидимые рамки не сразу: ждём, пока два замера подряд совпадут
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(10));
            match dw_win::describe(id) {
                Some(n) if n.border == m.border => break,
                Some(n) => m = n,
                None => break,
            }
        }
        let want = ScreenRect { x, y, w, h };
        tracing::info!(
            "заглушка: want {want:?}, измерено {:?} рамка {:?}",
            m.visible,
            m.border
        );
        if m.visible != want {
            dw_win::apply_sized(&[(&m, want)]);
            m.visible = want;
        }
        self.canvas.adopt(id, want);
        self.canvas.set_rect(
            id,
            Rect {
                center: rect.center,
                w: f64::from(w),
                h: f64::from(h),
            },
        );
        self.last.insert(id, want);
        self.managed.insert(id, m);
        self.ext.stubs.insert(id, info);
        self.ext.counters.stubs_created += 1;
        self.force_layout = true;
        Some(id)
    }

    /// Убрать заглушку со всех структур.
    fn drop_stub(&mut self, id: WindowId) {
        self.ext.stubs.remove(&id);
        dw_render::destroy_stub(id.0);
        self.managed.remove(&id);
        self.canvas.forget(id);
        self.mru.remove(id);
        self.last.remove(&id);
        self.undo.remove(&id);
        self.ext.modes.remove(&id);
    }

    /// Окно исчезло: заглушка, если это `suspend-window` или закрытие клиентом при `suspend_on_close`.
    /// Вызывается до того, как окно забыто холстом.
    pub(crate) fn on_gone(&mut self, id: WindowId) {
        if self.ext.stubs.contains_key(&id) {
            // заглушку убили снаружи
            self.ext.stubs.remove(&id);
            return;
        }
        let (Some(m), Some(rect)) = (self.managed.get(&id).cloned(), self.canvas.rect(id)) else {
            return;
        };
        // клик по панели задач (кнопка «Закрыть окно» на миниатюре) — пользователь закрыл окно сам
        let from_taskbar = self
            .ext
            .taskbar_click
            .is_some_and(|t| t.elapsed() < TASKBAR_CLOSE_WINDOW);
        let by_us = self.ext.closing.remove(&id).is_some() || from_taskbar;
        let asked = self.ext.suspending.iter().any(|s| s.0 == id);
        self.ext.suspending.retain(|s| s.0 != id);
        let want = asked
            || (!by_us
                && self.suspend_on_close_for(&m)
                && !self.canvas.is_widget(id)
                && !self.canvas.is_pinned(id));
        let launch = self.ext.launch.remove(&id);
        if !want {
            return;
        }
        let Some(launch) = launch else { return };
        let info = StubInfo::new(&m.exe, &m.title, launch);
        if self.make_stub(info, rect).is_some() {
            tracing::info!("«{}» закрыто — на его месте заглушка", m.title);
        }
    }

    /// Запустить приложение заглушки.
    pub(crate) fn launch_stub(&mut self, id: WindowId) -> Result<(), String> {
        let info = self.ext.stubs.get_mut(&id).ok_or("это не заглушка")?;
        if info.launching.is_some() {
            return Ok(());
        }
        match info.launch.start() {
            Ok(pid) => {
                info.launching = Some((Instant::now(), pid));
                dw_render::set_stub_launching(id.0, true);
                self.ext.counters.stubs_launched += 1;
                tracing::info!("запуск «{}» (pid {pid})", info.name);
                Ok(())
            }
            Err(e) => {
                tracing::warn!("не удалось запустить «{}»: {e}", info.name);
                Err(format!("не удалось запустить: {e}"))
            }
        }
    }

    pub(crate) fn close_stub(&mut self, id: WindowId) {
        if self.ext.stubs.contains_key(&id) {
            self.drop_stub(id);
            self.force_layout = true;
        }
    }

    /// Новое окно приложения, которое мы только что запустили из заглушки, занимает её место.
    /// Возвращает мировой прямоугольник заглушки.
    ///
    /// Окно признаётся «своим», если (по убыванию надёжности): тот же AUMID; его процесс — тот, что мы запустили,
    /// или его потомок (программа-пускатель вроде `code.exe` запускает настоящий процесс и выходит); либо,
    /// как запасной вариант, то же имя exe в первые секунды после запуска.
    pub(crate) fn claim_stub(&mut self, m: &Managed) -> Option<Rect> {
        if !self.ext.stubs.values().any(|s| s.launching.is_some()) {
            return None;
        }
        let exe = m.exe.to_lowercase();
        let aumid = dw_win::window_aumid(m.id).unwrap_or_default();
        let chain = dw_win::ancestors(m.pid);
        let id = self.ext.stubs.iter().find_map(|(id, s)| {
            let (since, pid) = s.launching?;
            let age = since.elapsed();
            let by_aumid = !aumid.is_empty() && s.launch.aumid == aumid && age < LAUNCH_PID_WINDOW;
            let by_pid = age < LAUNCH_PID_WINDOW && (pid == m.pid || chain.contains(&pid));
            let by_name = s.app.to_lowercase() == exe && age < LAUNCH_NAME_WINDOW;
            (by_aumid || by_pid || by_name).then_some(*id)
        })?;
        let rect = self.canvas.rect(id)?;
        self.drop_stub(id);
        tracing::info!("окно «{}» заняло место заглушки", m.title);
        Some(rect)
    }

    /// Периодически: просроченные закрытия и запуски.
    pub(crate) fn stub_tick(&mut self) {
        let now = Instant::now();
        // клик по панели задач запоминаем, чтобы отличить закрытие оттуда от закрытия самим приложением
        if dw_win::left_button_down()
            && TASKBAR_CLASSES.contains(&dw_win::class_at_cursor().as_str())
        {
            self.ext.taskbar_click = Some(now);
        }
        self.ext.suspending.retain(|(id, t)| {
            let keep = now.duration_since(*t) < SUSPEND_WAIT;
            if !keep {
                tracing::warn!(
                    "окно {} не закрылось за {SUSPEND_WAIT:?} — без заглушки",
                    id.0
                );
            }
            keep
        });
        self.ext
            .closing
            .retain(|_, t| now.duration_since(*t) < SUSPEND_WAIT);
        for (id, s) in &mut self.ext.stubs {
            if let Some((since, _)) = s.launching
                && since.elapsed() > LAUNCH_PID_WINDOW
            {
                s.launching = None;
                dw_render::set_stub_launching(id.0, false);
                tracing::info!("«{}» не появилось за 30 с — заглушка снова спит", s.name);
            }
        }
    }

    pub(crate) fn toggle_pin(&mut self, id: WindowId) {
        if self.canvas.is_pinned(id) {
            self.canvas.unpin(id);
            self.force_layout = true;
        } else if let Some(r) = self.canvas.screen_rect(id) {
            self.canvas.pin(id, r);
        }
    }

    /// Запомнить режим окна (Fit/Fill) для `State`.
    pub(crate) fn note_mode(&mut self, id: WindowId, a: Action) {
        let mode = match a {
            Action::FitWindow | Action::FitWindowSnapped => "Fit",
            Action::FillWindow => "Fill",
            _ => return,
        };
        if self.undo.contains_key(&id) {
            self.ext.modes.insert(id, mode);
        } else {
            self.ext.modes.remove(&id);
        }
    }

    // ---------- сессия ----------

    fn entries(&self) -> Vec<Entry> {
        let mut v = Vec::new();
        for (id, m) in &self.managed {
            if !self.canvas.contains(*id) || self.canvas.is_pinned(*id) {
                continue;
            }
            let Some(r) = self.canvas.rect(*id) else {
                continue;
            };
            let (launch, suspended) = if let Some(s) = self.ext.stubs.get(id) {
                (s.launch.clone(), true)
            } else if let Some(l) = self.ext.launch.get(id) {
                if self.resolve(&self.cfg, m).restore_windows == Some(false) {
                    continue;
                }
                (l.clone(), false)
            } else {
                continue;
            };
            let (app, title) = self
                .ext
                .stubs
                .get(id)
                .map_or((m.exe.clone(), m.title.clone()), |s| {
                    (s.app.clone(), s.title.clone())
                });
            v.push(Entry {
                app,
                title,
                path: launch.path,
                cmdline: launch.cmdline,
                aumid: launch.aumid,
                cx: r.center.x,
                cy: r.center.y,
                w: r.w,
                h: r.h,
                suspended,
            });
        }
        v.sort_by(|a, b| {
            (&a.app, &a.title, a.cx as i64, a.cy as i64).cmp(&(
                &b.app,
                &b.title,
                b.cx as i64,
                b.cy as i64,
            ))
        });
        v
    }

    /// Закладки, поставленные пользователем (отличающиеся от записанных в конфиге).
    fn user_bookmarks(&self) -> std::collections::BTreeMap<String, (f64, f64)> {
        self.bookmarks
            .iter()
            .filter(|(n, p)| {
                self.cfg
                    .navigation
                    .bookmarks
                    .get(n)
                    .is_none_or(|(x, y)| (x - p.x).abs() > 0.5 || (y - p.y).abs() > 0.5)
            })
            .map(|(n, p)| (n.to_string(), (p.x, p.y)))
            .collect()
    }

    pub(crate) fn save_session(&mut self) {
        let wins = self.entries();
        let c = self.canvas.viewport.center;
        let s = Session {
            version: session::VERSION,
            saved_at: unix_now(),
            camera: Some((c.x, c.y)),
            bookmarks: self.user_bookmarks(),
            windows: wins,
        };
        match session::save(&self.ext.session_path, &s) {
            Ok(()) => {
                self.ext.counters.session_saves += 1;
                self.ext.saved_wins = serde_json::to_string(&s.windows).unwrap_or_default();
                self.ext.saved_cam = (c.x, c.y);
            }
            Err(e) => tracing::error!("session.json не записан: {e}"),
        }
        self.ext.session_due = None;
    }

    /// Раз в полсекунды проверить, что изменилось, и назначить запись: окна — через ~1 с, камера — через ~5 с.
    pub(crate) fn session_poll(&mut self) {
        let now = Instant::now();
        if now >= self.ext.session_check {
            self.ext.session_check = now + Duration::from_millis(500);
            let wins = serde_json::to_string(&self.entries()).unwrap_or_default();
            let c = self.canvas.viewport.center;
            let mut due = self.ext.session_due;
            if wins != self.ext.saved_wins {
                due = Some(due.map_or(now + Duration::from_secs(1), |d| {
                    d.min(now + Duration::from_secs(1))
                }));
            }
            if (c.x - self.ext.saved_cam.0).abs() > 0.5 || (c.y - self.ext.saved_cam.1).abs() > 0.5
            {
                due = Some(due.unwrap_or(now + Duration::from_secs(5)));
            }
            self.ext.session_due = due;
        }
        if self.ext.session_due.is_some_and(|d| now >= d) {
            self.save_session();
        }
    }

    /// Прочитать сессию до первого сканирования окон: камера и закладки.
    pub(crate) fn restore_camera_and_bookmarks(&mut self) -> Option<Session> {
        let s = match session::load(&self.ext.session_path, unix_now()) {
            session::Loaded::Missing => return None,
            session::Loaded::Ok(s) => s,
            session::Loaded::Corrupt { reason, quarantine } => {
                tracing::error!(
                    "session.json повреждён ({reason}); перенесён в {}, старт с пустой сессией",
                    quarantine.display()
                );
                return None;
            }
        };
        if self.cfg.session.restore_camera
            && let Some((x, y)) = s.camera
        {
            self.canvas.viewport.center = Vec2::new(x, y);
            self.target = Vec2::new(x, y);
            self.applied = Vec2::new(x, y);
        }
        if self.cfg.session.restore_bookmarks {
            for (n, (x, y)) in &s.bookmarks {
                if let Ok(n) = n.parse::<u8>() {
                    self.bookmarks.insert(n, Vec2::new(*x, *y));
                }
            }
        }
        Some(s)
    }

    /// После первого сканирования: открытым окнам вернуть места, остальное поднять заглушками.
    pub(crate) fn restore_windows(&mut self, s: &Session) {
        let mut taken: HashSet<WindowId> = HashSet::new();
        let mut rest: Vec<&Entry> = Vec::new();
        for e in &s.windows {
            let rect = Rect {
                center: Vec2::new(e.cx, e.cy),
                w: e.w,
                h: e.h,
            };
            // открытое окно с тем же приложением и заголовком — на сохранённое место; потом — любое с тем же приложением
            let pick = |this: &Self, by_title: bool| {
                this.managed
                    .iter()
                    .filter(|(id, m)| {
                        !taken.contains(id)
                            && !this.ext.stubs.contains_key(id)
                            && m.exe.eq_ignore_ascii_case(&e.app)
                            && (!by_title || m.title == e.title)
                    })
                    .map(|(id, _)| *id)
                    .min_by_key(|id| id.0)
            };
            if !e.suspended
                && let Some(id) = pick(self, true).or_else(|| pick(self, false))
            {
                taken.insert(id);
                self.canvas.set_rect(id, rect);
                self.force_layout = true;
                continue;
            }
            rest.push(e);
        }
        for e in rest {
            let launch = Launch {
                path: e.path.clone(),
                cmdline: e.cmdline.clone(),
                aumid: e.aumid.clone(),
            };
            if !launch.can_start() || !(e.suspended || self.cfg.session.restore_windows) {
                continue;
            }
            let info = StubInfo::new(&e.app, &e.title, launch);
            self.make_stub(
                info,
                Rect {
                    center: Vec2::new(e.cx, e.cy),
                    w: e.w,
                    h: e.h,
                },
            );
        }
        // сразу запомнить, что записано: иначе первая же проверка запишет файл заново
        self.ext.saved_wins = serde_json::to_string(&self.entries()).unwrap_or_default();
        let c = self.canvas.viewport.center;
        self.ext.saved_cam = (c.x, c.y);
    }

    /// Пересоздать заглушки после `reset` (пауза/продолжение).
    pub(crate) fn rebuild_stubs(&mut self, keep: Vec<(StubInfo, Rect)>) {
        for (mut info, rect) in keep {
            info.launching = None;
            self.make_stub(info, rect);
        }
    }

    /// Заглушки с мировыми прямоугольниками; сами окна уничтожаются.
    pub(crate) fn take_stubs(&mut self) -> Vec<(StubInfo, Rect)> {
        let ids: Vec<_> = self.ext.stubs.keys().copied().collect();
        let mut out = Vec::new();
        for id in ids {
            if let (Some(i), Some(r)) = (self.ext.stubs.get(&id).cloned(), self.canvas.rect(id)) {
                out.push((i, r));
            }
            dw_render::destroy_stub(id.0);
        }
        self.ext.stubs.clear();
        out
    }

    /// Выход: заглушки не оставляем, прозрачность возвращаем.
    pub(crate) fn shutdown_ext(&mut self) {
        self.save_session();
        self.deco_restore_all();
        for id in self.ext.opacity.clone() {
            let mut set = true;
            let _ = dw_win::set_opacity(id, None, &mut set);
        }
        self.ext.opacity.clear();
        for id in self.ext.stubs.keys().copied().collect::<Vec<_>>() {
            dw_render::destroy_stub(id.0);
        }
        self.ext.stubs.clear();
    }

    // ---------- IPC ----------

    fn find_window(&self, sel: &Selector) -> Result<WindowId, String> {
        let fg = dw_win::foreground();
        let mut hits: Vec<WindowId> = self
            .managed
            .iter()
            .filter(|(id, m)| {
                self.canvas.contains(**id)
                    && match sel {
                        Selector::Id(n) => id.0 == *n,
                        Selector::App(a) => m.exe.to_lowercase().contains(&a.to_lowercase()),
                    }
            })
            .map(|(id, _)| *id)
            .collect();
        hits.sort_by_key(|id| (Some(*id) != fg, id.0));
        hits.first()
            .copied()
            .ok_or_else(|| format!("окно {sel:?} не найдено"))
    }

    fn find_output(&self, name: &str) -> Result<dw_win::MonitorInfo, String> {
        let n = name.trim().to_lowercase();
        self.geo
            .all
            .iter()
            .find(|m| {
                m.name.to_lowercase() == n
                    || n == "primary" && m.primary
                    || n.parse::<u32>()
                        .is_ok_and(|k| m.name.to_lowercase() == format!("display{k}"))
            })
            .cloned()
            .ok_or_else(|| format!("монитор «{name}» не найден"))
    }

    /// Снимок: пиксели RGBA и размер.
    fn take_screenshot(&self, target: &ShotTarget) -> Result<(Vec<u8>, i32, i32), String> {
        let rect = |(x, y, w, h): (i32, i32, i32, i32)| {
            dw_win::capture_rect(x, y, w, h)
                .map(|px| (px, w, h))
                .ok_or_else(|| "снимок экрана не получился".to_string())
        };
        match target {
            ShotTarget::Screen => rect(self.geo.bounds),
            ShotTarget::All => rect(dw_win::virtual_screen()),
            ShotTarget::Output(n) => rect(self.find_output(n)?.rect),
            ShotTarget::Region { x, y, w, h } => rect((*x, *y, *w, *h)),
            ShotTarget::Canvas { scale } => self.canvas_shot(*scale),
            ShotTarget::Window(sel) => {
                let id = self.find_window(sel)?;
                if let Some(shot) = dw_win::capture_window(id) {
                    return Ok(shot);
                }
                // PrintWindow отказал: берём то, что видно на экране
                let v = self
                    .managed
                    .get(&id)
                    .ok_or("окно не под управлением")?
                    .visible;
                rect((v.x, v.y, v.w, v.h))
            }
        }
    }

    /// Снять экран сейчас, а PNG закодировать и ответить в отдельном потоке (большие снимки кодируются секунды).
    fn screenshot_async(
        &self,
        path: &str,
        target: &ShotTarget,
        reply: crossbeam_channel::Sender<Response>,
    ) {
        if self.ext.paused.load(SeqCst) {
            let _ = reply.send(Response::Err("DriftWin на паузе".into()));
            return;
        }
        if !path.to_lowercase().ends_with(".png") {
            let _ = reply.send(Response::Err("путь должен оканчиваться на .png".into()));
            return;
        }
        let (px, w, h) = match self.take_screenshot(target) {
            Ok(s) => s,
            Err(e) => {
                let _ = reply.send(Response::Err(e));
                return;
            }
        };
        let path = path.to_string();
        std::thread::spawn(move || {
            let resp = match dw_render::save_png(Path::new(&path), w as u32, h as u32, px) {
                Ok(()) => Response::Ok(json!({"path": path, "size": [w, h]})),
                Err(e) => Response::Err(e),
            };
            let _ = reply.send(resp);
        });
    }

    /// Весь холст: окна снимаются по одному и ставятся на картинку по мировым координатам.
    fn canvas_shot(&self, scale: f64) -> Result<(Vec<u8>, i32, i32), String> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err("scale должен быть положительным числом".into());
        }
        let fg = dw_win::foreground();
        let mut ids: Vec<WindowId> = self
            .managed
            .keys()
            .filter(|id| self.canvas.contains(**id) && !self.canvas.is_pinned(**id))
            .copied()
            .collect();
        // окно в фокусе — сверху
        ids.sort_by_key(|id| (Some(*id) == fg, id.0));
        let pieces: Vec<crate::shot::Piece> = ids
            .iter()
            .filter_map(|id| {
                let r = self.canvas.rect(*id)?;
                let (px, w, h) = dw_win::capture_window(*id)?;
                Some(crate::shot::Piece {
                    x: r.center.x - r.w / 2.0,
                    y: -(r.center.y + r.h / 2.0),
                    w,
                    h,
                    px,
                })
            })
            .collect();
        crate::shot::compose(&pieces, self.cfg.background.color, scale)
            .ok_or_else(|| "на холсте нет окон, которые удалось снять".to_string())
    }

    fn bookmark_at_camera(&self) -> Option<u8> {
        let c = self.canvas.viewport.center;
        self.bookmarks
            .iter()
            .filter(|(_, p)| (p.x - c.x).abs() < 1.0 && (p.y - c.y).abs() < 1.0)
            .map(|(n, _)| *n)
            .min()
    }

    fn state_json(&self) -> serde_json::Value {
        let expose = self.cfg.ipc.expose_titles;
        let fg = dw_win::foreground();
        let c = self.canvas.viewport.center;
        let mut ids: Vec<_> = self
            .managed
            .keys()
            .filter(|id| self.canvas.contains(**id))
            .copied()
            .collect();
        ids.sort_by_key(|id| id.0);
        let windows: Vec<_> = ids
            .iter()
            .filter_map(|id| {
                let m = &self.managed[id];
                let r = self.canvas.rect(*id)?;
                Some(json!({
                    "id": id.0,
                    "app_id": if self.ext.stubs.contains_key(id) { self.ext.stubs[id].app.clone() } else { m.exe.clone() },
                    "title": if !expose { String::new() } else { self.ext.stubs.get(id).map_or(m.title.clone(), |s| s.title.clone()) },
                    "position": [r.center.x, r.center.y],
                    "size": [r.w, r.h],
                    "is_focused": Some(*id) == fg,
                    "is_widget": self.canvas.is_widget(*id),
                    "suspended": self.ext.stubs.contains_key(id),
                    "mode": if self.undo.contains_key(id) { self.ext.modes.get(id).copied().unwrap_or("Normal") } else { "Normal" },
                    "pid": m.pid,
                }))
            })
            .collect();
        let pinned: Vec<_> = ids
            .iter()
            .filter(|id| self.canvas.is_pinned(**id))
            .map(|id| id.0)
            .collect();
        let (cx, cy) = dw_win::cursor_pos();
        let active_bm = self.bookmark_at_camera();
        // холст общий для всех выбранных мониторов, поэтому у каждого монитора «камера» — точка мира в его центре
        let outputs: Vec<_> = self
            .geo
            .sel
            .iter()
            .map(|mo| {
                let r = mo.rect;
                let at = self
                    .canvas
                    .screen_point_to_world(r.0 + r.2 / 2, r.1 + r.3 / 2);
                json!({
                    "name": mo.name,
                    "camera": [at.x, at.y],
                    "shared_camera": [c.x, c.y],
                    "position": [r.0, r.1],
                    "zoom": self.zoom,
                    "size": [r.2, r.3],
                    "active": cx >= r.0 && cx < r.0 + r.2 && cy >= r.1 && cy < r.1 + r.3,
                    "active_bookmark": active_bm,
                })
            })
            .collect();
        json!({
            "camera": [c.x, c.y],
            "zoom": self.zoom,
            "layout": dw_win::current_layout()
                .and_then(|i| dw_win::keyboard_layouts().into_iter().nth(i))
                .map_or_else(String::new, |l| l.name),
            "windows": windows,
            "fullscreen": self.fullscreen.map(|f| f.0.0),
            "pinned": pinned,
            "outputs": outputs,
            "active_bookmark": active_bm,
            "paused": self.ext.paused.load(SeqCst),
        })
    }

    /// Обновить снимок для подписчиков и файл `state.txt` (~10 раз в секунду).
    pub(crate) fn publish_state(&mut self) {
        let now = Instant::now();
        if now < self.ext.state_next {
            return;
        }
        self.ext.state_next = now + Duration::from_millis(100);
        let st = self.state_json();
        self.ext.bus.publish(st.to_string());
        let fg = dw_win::foreground().filter(|id| self.managed.contains_key(id));
        let c = self.canvas.viewport.center;
        let txt = format!(
            "camera_x={:.1}\ncamera_y={:.1}\nzoom={:.3}\nwindows={}\nsuspended={}\nfocused={}\nfocused_app={}\nactive_bookmark={}\npaused={}\n",
            c.x,
            c.y,
            self.zoom,
            self.canvas.len() - self.ext.stubs.len().min(self.canvas.len()),
            self.ext.stubs.len(),
            fg.map_or(0, |i| i.0),
            fg.and_then(|i| self.managed.get(&i))
                .map_or("", |m| m.exe.as_str()),
            self.bookmark_at_camera()
                .map_or(String::new(), |n| n.to_string()),
            u8::from(self.ext.paused.load(SeqCst)),
        );
        if txt != self.ext.state_txt {
            let tmp = self.ext.state_path.with_extension("txt.tmp");
            if std::fs::write(&tmp, &txt).is_ok() {
                let _ = std::fs::rename(&tmp, &self.ext.state_path);
            }
            self.ext.state_txt = txt;
        }
        self.ext.counters.state_publishes += 1;
    }

    /// Обработать накопившиеся запросы IPC.
    pub(crate) fn serve_ipc(&mut self) {
        let Some(rx) = self.ext.ipc_rx.clone() else {
            return;
        };
        while let Ok(call) = rx.try_recv() {
            self.ext.counters.ipc_calls += 1;
            if let Request::Screenshot { path, target } = call.req {
                self.screenshot_async(&path, &target, call.reply);
                continue;
            }
            let resp = match self.ipc_call(call.req) {
                Ok(v) => Response::Ok(v),
                Err(e) => Response::Err(e),
            };
            let _ = call.reply.send(resp);
        }
    }

    fn ipc_call(&mut self, req: Request) -> Result<serde_json::Value, String> {
        let paused = self.ext.paused.load(SeqCst);
        if paused
            && !matches!(
                req,
                Request::State | Request::DebugCounters | Request::Cursor { to: None }
            )
        {
            return Err("DriftWin на паузе".into());
        }
        let done = |_: ()| Ok(json!(null));
        match req {
            Request::State => Ok(self.state_json()),
            Request::DebugCounters => {
                let c = &self.ext.counters;
                Ok(json!({
                    "ticks": c.ticks, "layouts": c.layouts, "rescans": c.rescans,
                    "ipc_calls": c.ipc_calls, "session_saves": c.session_saves,
                    "state_publishes": c.state_publishes,
                    "stubs_created": c.stubs_created, "stubs_launched": c.stubs_launched,
                    "windows": self.canvas.len(), "stubs": self.ext.stubs.len(),
                }))
            }
            Request::Camera { x, y, relative } => {
                self.inertia.stop();
                self.target = if relative {
                    Vec2::new(self.target.x + x, self.target.y + y)
                } else {
                    Vec2::new(x, y)
                };
                done(())
            }
            Request::Zoom { level } => {
                if !(level.is_finite() && level > 0.0) {
                    return Err("level должен быть положительным числом".into());
                }
                self.overview_prev = None;
                self.set_zoom_target(level.min(1.0));
                done(())
            }
            Request::Focus { window } => {
                let id = self.find_window(&window)?;
                self.mru.touch(id);
                self.go_to(id);
                done(())
            }
            Request::Move {
                window,
                x,
                y,
                relative,
            } => {
                let id = self.find_window(&window)?;
                let r = self.canvas.rect(id).ok_or("нет окна на холсте")?;
                let center = if relative {
                    Vec2::new(r.center.x + x, r.center.y + y)
                } else {
                    Vec2::new(x, y)
                };
                self.set_rects(vec![(id, Rect { center, ..r })]);
                done(())
            }
            Request::Resize { window, w, h } => {
                let id = self.find_window(&window)?;
                if !(w.is_finite()
                    && h.is_finite()
                    && w >= MIN_WIN.0
                    && h >= MIN_WIN.1
                    && w < 20000.0
                    && h < 20000.0)
                {
                    return Err(format!(
                        "размер должен быть от {}x{} до 20000",
                        MIN_WIN.0, MIN_WIN.1
                    ));
                }
                let r = self.canvas.rect(id).ok_or("нет окна на холсте")?;
                // верхний левый угол остаётся на месте
                let center = Vec2::new(
                    r.center.x - r.w / 2.0 + w / 2.0,
                    r.center.y + r.h / 2.0 - h / 2.0,
                );
                self.set_rects(vec![(id, Rect { center, w, h })]);
                done(())
            }
            Request::Opacity { window, value } => {
                let id = self.find_window(&window)?;
                if self.ext.stubs.contains_key(&id) {
                    return Err("у заглушки нет прозрачности".into());
                }
                let mut ours = self.ext.opacity.contains(&id);
                let v = if value >= 1.0 { None } else { Some(value) };
                dw_win::set_opacity(id, v, &mut ours).map_err(String::from)?;
                if ours {
                    self.ext.opacity.insert(id);
                } else {
                    self.ext.opacity.remove(&id);
                }
                done(())
            }
            Request::Pin { window } => {
                let id = self.find_window(&window)?;
                self.toggle_pin(id);
                done(())
            }
            Request::Close { window } => {
                let id = self.find_window(&window)?;
                if self.ext.stubs.contains_key(&id) {
                    self.close_stub(id);
                } else if let Some(m) = self.managed.get(&id) {
                    self.ext.closing.insert(id, Instant::now());
                    dw_win::close(m);
                }
                done(())
            }
            Request::Suspend { window } => {
                let id = self.find_window(&window)?;
                self.suspend_window(id)?;
                done(())
            }
            Request::Relaunch { window } => {
                let id = self.find_window(&window)?;
                self.launch_stub(id)?;
                done(())
            }
            Request::Layout { op } => {
                let list = dw_win::keyboard_layouts();
                let mut cur = dw_win::current_layout();
                let n = list.len();
                let step = |from: Option<usize>, back: bool| match (from, back) {
                    (Some(i), false) => (i + 1) % n,
                    (Some(i), true) => (i + n - 1) % n,
                    (None, _) => 0,
                };
                let to = match op {
                    LayoutOp::List => None,
                    LayoutOp::Next => Some(step(cur, false)),
                    LayoutOp::Prev => Some(step(cur, true)),
                    LayoutOp::Set(i) => Some(i),
                };
                if let Some(i) = to {
                    if n == 0 {
                        return Err("раскладок не найдено".into());
                    }
                    dw_win::set_layout(i).map_err(|e| e.to_string())?;
                    cur = Some(i);
                }
                Ok(json!({"current": cur, "layouts": list}))
            }
            Request::Cursor { to } => {
                if let Some((x, y)) = to {
                    unsafe {
                        SetCursorPos(x, y).map_err(|e| e.to_string())?;
                    }
                }
                let (x, y) = dw_win::cursor_pos();
                let w = self.canvas.screen_point_to_world(x, y);
                Ok(json!({"screen": [x, y], "world": [w.x, w.y]}))
            }
            Request::Action { name } => {
                let a = dw_config::parse_action_str(&name)?;
                self.run_action(a);
                done(())
            }
            Request::Bookmark { n, set } => {
                if set {
                    self.bookmarks.insert(n, self.canvas.viewport.center);
                } else {
                    let p = *self.bookmarks.get(&n).ok_or(format!("закладки {n} нет"))?;
                    self.inertia.stop();
                    self.target = p;
                }
                done(())
            }
            // снимок обрабатывает `serve_ipc`: кодирование PNG не должно стопорить главный цикл
            Request::Screenshot { .. } => Err("внутренняя ошибка: снимок обработан не там".into()),
            Request::Subscribe => Err("Subscribe — потоковый запрос".into()),
        }
    }
}

/// Канал IPC: сервер в своём потоке, запросы приходят в главный цикл.
pub(crate) fn start_ipc(ext: &mut Ext) {
    let (tx, rx) = crossbeam_channel::unbounded();
    match dw_ipc::serve(tx, ext.bus.clone()) {
        Ok(()) => {
            ext.ipc_rx = Some(rx);
            tracing::info!("IPC: {}", dw_ipc::pipe_name());
        }
        Err(e) => tracing::warn!("IPC недоступен: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_strips_exe_and_capitalizes() {
        assert_eq!(display_name("notepad.exe"), "Notepad");
        assert_eq!(display_name("Code.EXE"), "Code");
        assert_eq!(display_name("яндекс.exe"), "Яндекс");
        assert_eq!(display_name(""), "");
    }

    #[test]
    fn frame_host_stub_is_named_by_title() {
        assert_eq!(
            stub_name("ApplicationFrameHost.exe", " Калькулятор "),
            "Калькулятор"
        );
        assert_eq!(
            stub_name("ApplicationFrameHost.exe", ""),
            "ApplicationFrameHost"
        );
        assert_eq!(stub_name("notepad.exe", "Безымянный"), "Notepad");
    }

    #[test]
    fn packaged_apps_launch_by_aumid() {
        let l = |path: &str, aumid: &str| Launch {
            path: path.into(),
            cmdline: String::new(),
            aumid: aumid.into(),
        };
        assert!(l("", "Pkg!App").by_aumid());
        assert!(l(r"C:\Program Files\WindowsApps\X\x.exe", "Pkg!App").by_aumid());
        assert!(!l(r"C:\tools\a.exe", "Pkg!App").by_aumid());
        assert!(!l(r"C:\tools\a.exe", "").by_aumid());
        assert!(!l("", "").can_start());
    }
}
