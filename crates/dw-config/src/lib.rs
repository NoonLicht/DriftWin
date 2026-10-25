//! Конфигурация DriftWin: схема TOML, разбор привязок, правила окон, валидация.
//!
//! Неизвестные ключи дают предупреждение, неверные значения — ошибку с номером строки.
//! Ключи из плана, которые ещё не реализованы, принимаются без ошибки (список — `docs/CONFIG.md`);
//! если пользователь задал их явно, выдаётся предупреждение «пока не поддерживается».

pub mod catalog;
mod keys;
mod rules;

use std::collections::BTreeMap;
use std::fmt;

use dw_core::Dir;
use dw_core::action::{
    Action, Bindings, DragKind, KeyBinding, Mods, MouseAction, MouseBinding, MouseButton,
};
use dw_core::snap::SnapCfg;
use serde::Deserialize;

pub use rules::{Matcher, Resolved, Rule};

/// Какая клавиша играет роль `mod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModKey {
    Super,
    Alt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnExit {
    RestoreBaseline,
    BringToView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exclude {
    pub exe: Option<String>,
    pub class: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Windows {
    /// Белый список exe из конфига (нижний регистр); пуст — решает командная строка.
    pub only: Vec<String>,
    pub exclude: Vec<Exclude>,
    pub on_exit: OnExit,
    /// Кадров в секунду; 0 — по частоте обновления мониторов.
    pub max_fps: u32,
}

/// Прокрутка вида у края экрана (`[navigation.edge_pan]`).
#[derive(Debug, Clone, PartialEq)]
pub struct EdgePan {
    /// Ширина зоны у края при перетаскивании окна, пиксели.
    pub zone: f64,
    /// Скорость на границе зоны и у самого края, пикселей за кадр при 60 кадрах/с.
    pub speed_min: f64,
    pub speed_max: f64,
    /// Прокручивать вид курсором без перетаскивания (переключается `toggle-cursor-pan`).
    pub cursor_pan: bool,
    pub cursor_zone: f64,
    /// Сколько курсор должен постоять у края, мс.
    pub latency_ms: u64,
}

/// Горячие углы основного монитора (`[hot_corners]`).
#[derive(Debug, Clone, PartialEq)]
pub struct HotCorners {
    pub threshold: i32,
    pub top_left: Option<Action>,
    pub top_right: Option<Action>,
    pub bottom_left: Option<Action>,
    pub bottom_right: Option<Action>,
}

#[derive(Debug, Clone)]
pub struct Navigation {
    pub nudge_step: f64,
    pub resize_step: f64,
    pub edge_pan: EdgePan,
    pub pan_step: f64,
    /// Доля оставшегося пути камеры, проходимая за кадр (0..1].
    pub camera_speed: f64,
    pub mouse_speed: f64,
    pub auto_navigate: bool,
    pub bookmarks: BTreeMap<u8, (f64, f64)>,
}

/// Масштаб и обзор (`[zoom]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Zoom {
    /// Нижняя граница масштаба; верхняя всегда 1.0.
    pub min: f64,
    /// Во сколько раз меняют масштаб `zoom-in` / `zoom-out`.
    pub step: f64,
    /// Клик по миниатюре в обзоре сразу возвращает масштаб 1:1 (иначе только фокус и центровка; двойной клик выходит).
    pub click_exits: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgType {
    None,
    Solid,
    Grid,
    Tile,
    Wallpaper,
}

/// Фон холста (`[background]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Background {
    pub kind: BgType,
    pub color: [u8; 3],
    pub grid_color: [u8; 3],
    pub grid_size: f64,
    /// Путь к картинке для `tile` и `wallpaper`; пусто — обои рабочего стола.
    pub image: Option<String>,
    /// Рисовать фон и на рабочем столе под окнами, а не только в обзоре.
    pub on_desktop: bool,
}

/// На каких мониторах работает DriftWin (`[monitors]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Monitors {
    /// Имена из `use` (нижний регистр); пусто — основной монитор.
    pub names: Vec<String>,
    /// Окна не заходят на мониторы вне списка (прячутся, пока перекрывают их).
    pub isolate: bool,
}

/// Сессия и заглушки (`[session]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// Закрытие окна клиентом превращается в заглушку.
    pub suspend_on_close: bool,
    /// Открытые окна возвращаются после перезапуска заглушками.
    pub restore_windows: bool,
    pub restore_camera: bool,
    pub restore_bookmarks: bool,
}

/// Настройки IPC (`[ipc]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Ipc {
    /// Показывать заголовки окон в ответах и подписке.
    pub expose_titles: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub session: Session,
    pub ipc: Ipc,
    pub monitors: Monitors,
    pub zoom: Zoom,
    pub background: Background,
    pub mod_key: ModKey,
    pub windows: Windows,
    pub navigation: Navigation,
    pub bindings: Bindings,
    pub rules: Vec<Rule>,
    pub snap: SnapCfg,
    /// Отступ окна от края вида при fit-window (`[zoom] fit_padding`).
    pub fit_padding: f64,
    pub hot_corners: HotCorners,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub line: Option<usize>,
    pub col: Option<usize>,
    pub msg: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.col) {
            (Some(l), Some(c)) => write!(f, "строка {l}, столбец {c}: {}", self.msg),
            (Some(l), None) => write!(f, "строка {l}: {}", self.msg),
            _ => f.write_str(&self.msg),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<String>,
}

// ---------- сырая схема ----------

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawConfig {
    mod_key: Option<String>,
    autostart: Vec<String>,
    windows: RawWindows,
    navigation: RawNavigation,
    bindings: RawBindings,
    keybindings: BTreeMap<String, String>,
    mouse: RawMouse,
    window_rules: Vec<rules::RawRule>,
    // секции плана, которые появятся в следующих этапах
    zoom: RawZoom,
    monitors: RawMonitors,
    background: RawBackground,
    snap: RawSnap,
    hot_corners: RawHotCorners,
    decorations: Option<toml::Value>,
    effects: Option<toml::Value>,
    gestures: Option<toml::Value>,
    touch: Option<toml::Value>,
    outputs: Option<toml::Value>,
    session: RawSession,
    ipc: RawIpc,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSession {
    suspend_on_close: Option<bool>,
    restore_windows: Option<bool>,
    restore_camera: Option<bool>,
    restore_bookmarks: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawIpc {
    expose_titles: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSnap {
    enabled: Option<bool>,
    gap: Option<f64>,
    gap_step: Option<f64>,
    outer_gap: Option<f64>,
    distance: Option<f64>,
    break_force: Option<f64>,
    corners: Option<bool>,
    centers: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawZoom {
    fit_padding: Option<f64>,
    min: Option<f64>,
    step: Option<f64>,
    click: Option<String>,
    backend: Option<String>,
    /// Остальные ключи [zoom] ещё не реализованы.
    #[serde(flatten)]
    rest: BTreeMap<String, toml::Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMonitors {
    #[serde(rename = "use")]
    names: Vec<String>,
    isolate: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawBackground {
    #[serde(rename = "type")]
    kind: Option<String>,
    color: Option<String>,
    grid_color: Option<String>,
    grid_size: Option<f64>,
    image: Option<String>,
    on_desktop: Option<bool>,
    /// Остальные ключи [background] ещё не реализованы.
    #[serde(flatten)]
    rest: BTreeMap<String, toml::Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEdgePan {
    zone: Option<f64>,
    speed_min: Option<f64>,
    speed_max: Option<f64>,
    cursor_pan: Option<bool>,
    cursor_zone: Option<f64>,
    latency_ms: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawHotCorners {
    threshold: Option<i64>,
    top_left: Option<String>,
    top_right: Option<String>,
    bottom_left: Option<String>,
    bottom_right: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawWindows {
    only: Vec<String>,
    exclude: Vec<RawExclude>,
    on_exit: Option<String>,
    elevated: Option<String>,
    park_limit: Option<i64>,
    max_fps: Option<i64>,
    disable_when_fullscreen: Option<bool>,
    panic_key: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawExclude {
    exe: Option<String>,
    class: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawNavigation {
    pan_step: Option<f64>,
    camera_speed: Option<f64>,
    mouse_speed: Option<f64>,
    auto_navigate: Option<bool>,
    bookmarks: BTreeMap<String, [f64; 2]>,
    // ключи плана без реализации
    trackpad_speed: Option<toml::Value>,
    touch_speed: Option<toml::Value>,
    drift: Option<toml::Value>,
    auto_navigate_on_close: Option<toml::Value>,
    auto_navigate_on_click: Option<toml::Value>,
    nudge_step: Option<f64>,
    resize_step: Option<f64>,
    anchors: Option<toml::Value>,
    edge_pan: RawEdgePan,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawBindings {
    disable_defaults: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMouse {
    anywhere: BTreeMap<String, String>,
    #[serde(rename = "on-window")]
    on_window: Option<toml::Value>,
    #[serde(rename = "on-canvas")]
    on_canvas: Option<toml::Value>,
    resize_on_border: Option<toml::Value>,
    decoration_resize_snapped: Option<toml::Value>,
    decoration_fit_snapped: Option<toml::Value>,
}

// ---------- привязки ----------

const DIRS: [(&str, Dir); 4] = [
    ("up", Dir::Up),
    ("down", Dir::Down),
    ("left", Dir::Left),
    ("right", Dir::Right),
];

/// Действия из плана (§2.12), которые ещё не реализованы.
const UNSUPPORTED_ACTIONS: &[&str] = &[
    "exec",
    "exec-terminal",
    "exec-launcher",
    "spawn",
    "move-to-bookmark",
    "set-trackpad",
    "send-to-output",
    "send-cursor-to-output",
    "switch-layout",
];

#[derive(Debug, PartialEq, Eq)]
enum ParsedAction {
    Action(Action),
    /// «none» — отключить привязку.
    None,
    Unsupported(String),
}

fn parse_dir(s: Option<&str>) -> Result<Dir, String> {
    let s = s.ok_or("не указано направление (up, down, left, right)")?;
    DIRS.iter()
        .find(|(n, _)| *n == s)
        .map(|(_, d)| *d)
        .ok_or_else(|| format!("неизвестное направление «{s}» (up, down, left, right)"))
}

/// Разобрать действие из строки (как значение в `[keybindings]`), например для IPC.
pub fn parse_action_str(src: &str) -> Result<Action, String> {
    match parse_action(src)? {
        ParsedAction::Action(a) => Ok(a),
        ParsedAction::None => Err("«none» — не действие".into()),
        ParsedAction::Unsupported(n) => Err(format!("действие «{n}» пока не поддерживается")),
    }
}

fn parse_action(src: &str) -> Result<ParsedAction, String> {
    let mut it = src.split_whitespace();
    let name = it.next().ok_or("пустое действие")?;
    let arg = it.next();
    if it.next().is_some() {
        return Err(format!("лишние аргументы у действия «{name}»"));
    }
    let a = match name {
        "none" => return Ok(ParsedAction::None),
        "pan-viewport" => Action::PanViewport(parse_dir(arg)?),
        "focus-nearest" => Action::FocusNearest(parse_dir(arg)?),
        "center-nearest" => Action::CenterNearest(parse_dir(arg)?),
        "cycle-windows" => match arg {
            Some("forward") => Action::CycleWindows(true),
            Some("backward") => Action::CycleWindows(false),
            _ => return Err("cycle-windows принимает forward или backward".into()),
        },
        "center-window" => Action::CenterWindow,
        "focus-center" => Action::FocusCenter,
        "home-toggle" => Action::HomeToggle,
        "go-to-bookmark" | "set-bookmark" => {
            let n: u8 = arg
                .and_then(|a| a.parse().ok())
                .filter(|n| (1..=9).contains(n))
                .ok_or_else(|| format!("{name} принимает номер закладки 1..9"))?;
            if name == "go-to-bookmark" {
                Action::GoToBookmark(n)
            } else {
                Action::SetBookmark(n)
            }
        }
        "toggle-pin-to-screen" => Action::TogglePinToScreen,
        "close-window" => Action::CloseWindow,
        "reload-config" => Action::ReloadConfig,
        "quit" => Action::Quit,
        "nudge-window" => Action::NudgeWindow(parse_dir(arg)?),
        "move-to-snap" => Action::MoveToSnap(parse_dir(arg)?),
        "grow-window" => Action::GrowWindow(parse_dir(arg)?),
        "shrink-window" => Action::ShrinkWindow(parse_dir(arg)?),
        "resize-to-snap" => Action::ResizeToSnap(parse_dir(arg)?),
        "toggle-fullscreen" => Action::ToggleFullscreen,
        "fit-window-snapped" => Action::FitWindowSnapped,
        "fit-window" => Action::FitWindow,
        "fill-window" => Action::FillWindow,
        "toggle-cursor-pan" => Action::ToggleCursorPan,
        "snap-gap-increase" => Action::GapIncrease,
        "snap-gap-decrease" => Action::GapDecrease,
        "zoom-to-fit" => Action::ZoomToFit,
        "zoom-to-fit-snapped" => Action::ZoomToFitSnapped,
        "zoom-in" => Action::ZoomIn,
        "zoom-out" => Action::ZoomOut,
        "zoom-reset" => Action::ZoomReset,
        "suspend-window" => Action::SuspendWindow,
        n if UNSUPPORTED_ACTIONS.contains(&n) => return Ok(ParsedAction::Unsupported(n.into())),
        n => return Err(format!("неизвестное действие «{n}»")),
    };
    Ok(ParsedAction::Action(a))
}

/// Стандартные привязки клавиатуры. Для `mod_key = "alt"` дополнительный Alt заменён на Shift.
fn default_keybindings(m: ModKey) -> Vec<(String, String)> {
    let focus = match m {
        ModKey::Super => "mod+alt",
        ModKey::Alt => "mod+ctrl+shift",
    };
    let mut v: Vec<(String, String)> = Vec::new();
    for (n, _) in DIRS {
        v.push((format!("mod+ctrl+{n}"), format!("pan-viewport {n}")));
        v.push((format!("{focus}+{n}"), format!("focus-nearest {n}")));
    }
    v.push(("mod+ctrl+tab".into(), "cycle-windows forward".into()));
    v.push(("mod+ctrl+shift+tab".into(), "cycle-windows backward".into()));
    v.push(("mod+ctrl+c".into(), "center-window".into()));
    v.push(("mod+ctrl+h".into(), "home-toggle".into()));
    v.push(("mod+ctrl+t".into(), "toggle-pin-to-screen".into()));
    for n in 1..=9 {
        v.push((format!("mod+ctrl+{n}"), format!("go-to-bookmark {n}")));
        v.push((format!("mod+ctrl+shift+{n}"), format!("set-bookmark {n}")));
    }
    // с Alt в роли mod буквенные комбинации «alt+буква» открывали бы меню приложений, поэтому добавляем Ctrl
    let letter = match m {
        ModKey::Super => "mod",
        ModKey::Alt => "mod+ctrl",
    };
    v.push((format!("{letter}+f"), "toggle-fullscreen".into()));
    v.push((format!("{letter}+m"), "fit-window-snapped".into()));
    v.push((format!("{letter}+shift+m"), "fit-window".into()));
    v.push((format!("{letter}+shift+f"), "fill-window".into()));
    v.push((format!("{letter}+w"), "zoom-to-fit".into()));
    v.push((format!("{letter}+shift+w"), "zoom-to-fit-snapped".into()));
    v.push(("mod+ctrl+equals".into(), "zoom-in".into()));
    v.push(("mod+ctrl+minus".into(), "zoom-out".into()));
    v.push(("mod+ctrl+0".into(), "zoom-reset".into()));
    v.push((format!("{letter}+shift+q"), "suspend-window".into()));
    v.push((format!("{letter}+rbracket"), "snap-gap-increase".into()));
    v.push((format!("{letter}+lbracket"), "snap-gap-decrease".into()));
    if m == ModKey::Super {
        for (n, _) in DIRS {
            v.push((format!("mod+shift+{n}"), format!("move-to-snap {n}")));
        }
    }
    v
}

struct Ctx<'a> {
    src: &'a str,
    errors: Vec<ConfigError>,
    warnings: Vec<String>,
}

impl Ctx<'_> {
    fn err_at(&mut self, needle: &str, msg: impl Into<String>) {
        let (line, col) = keys::locate(self.src, needle).unzip();
        self.errors.push(ConfigError {
            line,
            col,
            msg: msg.into(),
        });
    }

    fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }

    fn warn_at(&mut self, needle: &str, msg: impl Into<String>) {
        let msg = msg.into();
        match keys::locate(self.src, needle) {
            Some((l, _)) => self.warnings.push(format!("строка {l}: {msg}")),
            None => self.warnings.push(msg),
        }
    }
}

fn compile_keys(raw: &RawConfig, m: ModKey, cx: &mut Ctx) -> Vec<KeyBinding> {
    let mut table: Vec<((Mods, u16), Option<Action>)> = Vec::new();
    let mut put = |k: (Mods, u16), a: Option<Action>| {
        table.retain(|(x, _)| *x != k);
        table.push((k, a));
    };
    if !raw.bindings.disable_defaults.iter().any(|d| d == "keys") {
        for (c, a) in default_keybindings(m) {
            let k = keys::parse_combo(&c, m).expect("стандартная привязка корректна");
            if let Ok(ParsedAction::Action(a)) = parse_action(&a) {
                put(k, Some(a));
            }
        }
    }
    for (combo, act) in &raw.keybindings {
        let k = match keys::parse_combo(combo, m) {
            Ok(k) => k,
            Err(e) => {
                cx.err_at(combo, format!("привязка «{combo}»: {e}"));
                continue;
            }
        };
        match parse_action(act) {
            Ok(ParsedAction::Action(a)) => put(k, Some(a)),
            Ok(ParsedAction::None) => put(k, None),
            Ok(ParsedAction::Unsupported(n)) => {
                cx.warn_at(
                    combo,
                    format!("действие «{n}» пока не поддерживается, привязка «{combo}» пропущена"),
                );
            }
            Err(e) => cx.err_at(combo, format!("привязка «{combo}»: {e}")),
        }
    }
    table
        .into_iter()
        .filter_map(|((mods, vk), a)| a.map(|action| KeyBinding { mods, vk, action }))
        .collect()
}

fn mouse_drag_kind(name: &str) -> Option<DragKind> {
    Some(match name {
        "pan-viewport" => DragKind::Pan,
        "move-window" => DragKind::MoveWindow,
        "move-snapped-windows" => DragKind::MoveCluster,
        "resize-window" => DragKind::ResizeWindow,
        "resize-window-snapped" => DragKind::ResizeCluster,
        _ => return None,
    })
}

/// Стандартные привязки мыши (§2.4 плана).
fn default_mouse(m: ModKey) -> Vec<MouseBinding> {
    let mods = |win, ctrl, shift, alt| Mods {
        win,
        ctrl,
        shift,
        alt,
    };
    let mod_only = match m {
        ModKey::Super => mods(true, false, false, false),
        ModKey::Alt => mods(false, false, false, true),
    };
    let alt = mods(false, false, false, true);
    let alt_shift = mods(false, false, true, true);
    let drag = |mods, button, k| MouseBinding {
        mods,
        button,
        action: MouseAction::Drag(k),
    };
    let click = |mods, button, a| MouseBinding {
        mods,
        button,
        action: MouseAction::Click(a),
    };
    use MouseButton::{Left, Middle, Right};
    // fill-window на mod+СКМ; когда mod = Alt, это совпало бы с fit-window-snapped, поэтому добавляем Ctrl
    let fill_mods = Mods {
        ctrl: m == ModKey::Alt,
        ..mod_only
    };
    vec![
        drag(
            Mods {
                ctrl: true,
                ..mod_only
            },
            Left,
            DragKind::Pan,
        ),
        drag(alt, Left, DragKind::MoveWindow),
        drag(alt_shift, Left, DragKind::MoveCluster),
        drag(alt, Right, DragKind::ResizeCluster),
        drag(alt_shift, Right, DragKind::ResizeWindow),
        click(alt, Middle, Action::FitWindowSnapped),
        click(alt_shift, Middle, Action::FitWindow),
        click(fill_mods, Middle, Action::FillWindow),
    ]
}

fn compile_mouse(raw: &RawConfig, m: ModKey, cx: &mut Ctx) -> Vec<MouseBinding> {
    let mut table: Vec<((Mods, MouseButton), Option<MouseAction>)> = Vec::new();
    let mut put = |k: (Mods, MouseButton), a: Option<MouseAction>| {
        table.retain(|(x, _)| *x != k);
        table.push((k, a));
    };
    if !raw.bindings.disable_defaults.iter().any(|d| d == "mouse") {
        for b in default_mouse(m) {
            put((b.mods, b.button), Some(b.action));
        }
    }
    for (combo, act) in &raw.mouse.anywhere {
        let k = match keys::parse_mouse_combo(combo, m) {
            Ok(k) => k,
            Err(e) => {
                cx.err_at(combo, format!("привязка мыши «{combo}»: {e}"));
                continue;
            }
        };
        let name = act.trim();
        if let Some(kind) = mouse_drag_kind(name) {
            put(k, Some(MouseAction::Drag(kind)));
            continue;
        }
        match parse_action(name) {
            Ok(ParsedAction::None) => put(k, None),
            Ok(ParsedAction::Action(a)) => put(k, Some(MouseAction::Click(a))),
            Ok(ParsedAction::Unsupported(n)) => cx.warn_at(
                combo,
                format!("действие «{n}» пока не поддерживается, привязка «{combo}» пропущена"),
            ),
            Err(e) => cx.err_at(combo, format!("привязка мыши «{combo}»: {e}")),
        }
    }
    table
        .into_iter()
        .filter_map(|((mods, button), a)| {
            a.map(|action| MouseBinding {
                mods,
                button,
                action,
            })
        })
        .collect()
}

/// Цвет `#rrggbb` или `#rgb`.
fn parse_color(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().strip_prefix('#')?;
    let v = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok();
    match h.len() {
        6 if h.is_ascii() => Some([v(0, 2)?, v(2, 2)?, v(4, 2)?]),
        3 if h.is_ascii() => Some([v(0, 1)? * 17, v(1, 1)? * 17, v(2, 1)? * 17]),
        _ => None,
    }
}

fn compile_background(b: &RawBackground, cx: &mut Ctx) -> Background {
    let kind = match b.kind.as_deref().unwrap_or("default") {
        "default" | "grid" => BgType::Grid,
        "solid" => BgType::Solid,
        "none" => BgType::None,
        "tile" => BgType::Tile,
        "wallpaper" => BgType::Wallpaper,
        "shader" => {
            cx.warn_at(
                "type",
                "background.type = \"shader\" пока не поддерживается, используется сетка",
            );
            BgType::Grid
        }
        o => {
            cx.err_at(
                "type",
                format!("background.type = «{o}»: допустимо default, grid, solid, tile, wallpaper, none, shader"),
            );
            BgType::Grid
        }
    };
    let mut color = |v: &Option<String>, name: &str, def: [u8; 3]| match v {
        None => def,
        Some(s) => parse_color(s).unwrap_or_else(|| {
            cx.err_at(name, format!("{name} = «{s}»: ожидается цвет вида #rrggbb"));
            def
        }),
    };
    let def = dw_core_bg_default();
    let c = color(&b.color, "color", def.0);
    let gc = color(&b.grid_color, "grid_color", def.1);
    for k in b.rest.keys() {
        cx.warn_at(k, format!("background.{k} пока не поддерживается"));
    }
    Background {
        kind,
        color: c,
        grid_color: gc,
        grid_size: ranged(b.grid_size, "grid_size", 100.0, 8.0, 5000.0, cx),
        image: b.image.clone().filter(|s| !s.trim().is_empty()),
        on_desktop: b.on_desktop.unwrap_or(false),
    }
}

/// Цвета фона по умолчанию: (фон, линии сетки).
fn dw_core_bg_default() -> ([u8; 3], [u8; 3]) {
    ([24, 26, 32], [48, 52, 64])
}

fn ranged(v: Option<f64>, name: &str, def: f64, min: f64, max: f64, cx: &mut Ctx) -> f64 {
    let v = v.unwrap_or(def);
    if !(min..=max).contains(&v) {
        cx.err_at(name, format!("{name} = {v} вне диапазона {min}..{max}"));
    }
    v
}

fn pos_int(v: Option<i64>, name: &str, min: i64, max: i64, cx: &mut Ctx) -> Option<i64> {
    let v = v?;
    if (min..=max).contains(&v) {
        Some(v)
    } else {
        cx.err_at(name, format!("{name} = {v} вне диапазона {min}..{max}"));
        None
    }
}

/// Разобрать и проверить конфиг. Ошибки собираются все сразу.
pub fn parse(src: &str) -> Result<Loaded, Vec<ConfigError>> {
    let mut unknown: Vec<String> = Vec::new();
    let de = toml::Deserializer::parse(src).map_err(|e| vec![toml_error(src, &e)])?;
    let raw: RawConfig = match serde_ignored::deserialize(de, |p| unknown.push(p.to_string())) {
        Ok(r) => r,
        Err(e) => return Err(vec![toml_error(src, &e)]),
    };
    let mut cx = Ctx {
        src,
        errors: vec![],
        warnings: vec![],
    };
    for u in unknown {
        let last = u.rsplit('.').next().unwrap_or(&u).to_string();
        cx.warn_at(&last, format!("неизвестный ключ «{u}» проигнорирован"));
    }

    let mod_key = match raw.mod_key.as_deref().unwrap_or("super") {
        "super" => ModKey::Super,
        "alt" => ModKey::Alt,
        "ctrl" => {
            cx.err_at(
                "mod_key",
                "mod_key = \"ctrl\" не поддерживается: конфликтует с горячими клавишами приложений (используйте super или alt)",
            );
            ModKey::Super
        }
        other => {
            cx.err_at(
                "mod_key",
                format!("mod_key = «{other}»: допустимо super или alt"),
            );
            ModKey::Super
        }
    };

    let keys_v = compile_keys(&raw, mod_key, &mut cx);
    let mouse = compile_mouse(&raw, mod_key, &mut cx);

    let panic_src = raw
        .windows
        .panic_key
        .as_deref()
        .unwrap_or("mod+ctrl+shift+r");
    let panic = match keys::parse_combo(panic_src, mod_key) {
        Ok(k) => k,
        Err(e) => {
            cx.err_at("panic_key", format!("panic_key «{panic_src}»: {e}"));
            keys::parse_combo("mod+ctrl+shift+r", ModKey::Super).expect("стандартная")
        }
    };

    let on_exit = match raw.windows.on_exit.as_deref().unwrap_or("restore_baseline") {
        "restore_baseline" => OnExit::RestoreBaseline,
        "bring_to_view" => {
            cx.warn_at(
                "on_exit",
                "on_exit = \"bring_to_view\" пока не поддерживается, используется restore_baseline",
            );
            OnExit::RestoreBaseline
        }
        other => {
            cx.err_at(
                "on_exit",
                format!("on_exit = «{other}»: допустимо restore_baseline или bring_to_view"),
            );
            OnExit::RestoreBaseline
        }
    };
    match raw.windows.elevated.as_deref() {
        None | Some("skip") => {}
        Some("notify") => cx.warn_at(
            "elevated",
            "elevated = \"notify\" пока не поддерживается, окна с повышенными правами пропускаются",
        ),
        Some(o) => cx.err_at(
            "elevated",
            format!("elevated = «{o}»: допустимо skip или notify"),
        ),
    }
    if raw.windows.park_limit.is_some_and(|p| p != 30000) {
        cx.warn_at(
            "park_limit",
            "park_limit пока не настраивается (фиксировано 30000)",
        );
    }
    let max_fps = pos_int(raw.windows.max_fps, "max_fps", 0, 240, &mut cx).unwrap_or(0) as u32;
    if (1..10).contains(&max_fps) {
        cx.err_at(
            "max_fps",
            "max_fps: 0 (по частоте мониторов) или от 10 до 240",
        );
    }

    let n = &raw.navigation;
    let pan_step = n.pan_step.unwrap_or(100.0);
    if !(1.0..=5000.0).contains(&pan_step) {
        cx.err_at(
            "pan_step",
            format!("pan_step = {pan_step} вне диапазона 1..5000"),
        );
    }
    let camera_speed = n.camera_speed.unwrap_or(0.3);
    if !(0.01..=1.0).contains(&camera_speed) {
        cx.err_at(
            "camera_speed",
            format!("camera_speed = {camera_speed} вне диапазона 0.01..1"),
        );
    }
    let mouse_speed = n.mouse_speed.unwrap_or(1.0);
    if !(0.1..=10.0).contains(&mouse_speed) {
        cx.err_at(
            "mouse_speed",
            format!("mouse_speed = {mouse_speed} вне диапазона 0.1..10"),
        );
    }
    let mut bookmarks = BTreeMap::new();
    for (k, [x, y]) in &n.bookmarks {
        match k.parse::<u8>() {
            Ok(i) if (1..=9).contains(&i) => {
                bookmarks.insert(i, (*x, *y));
            }
            _ => cx.err_at(k, format!("закладка «{k}»: номер должен быть 1..9")),
        }
    }
    for (name, set) in [
        ("trackpad_speed", n.trackpad_speed.is_some()),
        ("touch_speed", n.touch_speed.is_some()),
        ("drift", n.drift.is_some()),
        ("auto_navigate_on_close", n.auto_navigate_on_close.is_some()),
        ("auto_navigate_on_click", n.auto_navigate_on_click.is_some()),
        ("anchors", n.anchors.is_some()),
    ] {
        if set {
            cx.warn_at(name, format!("navigation.{name} пока не поддерживается"));
        }
    }
    for (name, set) in [
        ("on-window", raw.mouse.on_window.is_some()),
        ("on-canvas", raw.mouse.on_canvas.is_some()),
        ("resize_on_border", raw.mouse.resize_on_border.is_some()),
        (
            "decoration_resize_snapped",
            raw.mouse.decoration_resize_snapped.is_some(),
        ),
        (
            "decoration_fit_snapped",
            raw.mouse.decoration_fit_snapped.is_some(),
        ),
    ] {
        if set {
            cx.warn_at(name, format!("mouse.{name} пока не поддерживается"));
        }
    }
    for (name, set) in [
        ("decorations", raw.decorations.is_some()),
        ("effects", raw.effects.is_some()),
        ("gestures", raw.gestures.is_some()),
        ("touch", raw.touch.is_some()),
        ("outputs", raw.outputs.is_some()),
    ] {
        if set {
            cx.warn_at(name, format!("секция [{name}] пока не поддерживается"));
        }
    }
    if !raw.autostart.is_empty() {
        cx.warn_at("autostart", "autostart пока не поддерживается");
    }

    let nudge_step = ranged(n.nudge_step, "nudge_step", 20.0, 1.0, 2000.0, &mut cx);
    let resize_step = ranged(n.resize_step, "resize_step", 20.0, 1.0, 2000.0, &mut cx);
    let ep = &n.edge_pan;
    let edge_pan = EdgePan {
        zone: ranged(ep.zone, "zone", 100.0, 10.0, 500.0, &mut cx),
        speed_min: ranged(ep.speed_min, "speed_min", 4.0, 0.5, 100.0, &mut cx),
        speed_max: ranged(ep.speed_max, "speed_max", 10.0, 0.5, 200.0, &mut cx),
        cursor_pan: ep.cursor_pan.unwrap_or(false),
        cursor_zone: ranged(ep.cursor_zone, "cursor_zone", 20.0, 1.0, 200.0, &mut cx),
        latency_ms: pos_int(ep.latency_ms, "latency_ms", 0, 2000, &mut cx).unwrap_or(120) as u64,
    };
    if edge_pan.speed_min > edge_pan.speed_max {
        cx.err_at("speed_min", "speed_min не может быть больше speed_max");
    }
    let sn = &raw.snap;
    let snap = SnapCfg {
        enabled: sn.enabled.unwrap_or(true),
        gap: ranged(sn.gap, "gap", 12.0, 0.0, 200.0, &mut cx),
        gap_step: ranged(sn.gap_step, "gap_step", 4.0, 1.0, 100.0, &mut cx),
        outer_gap: ranged(sn.outer_gap, "outer_gap", 12.0, 0.0, 500.0, &mut cx),
        distance: ranged(sn.distance, "distance", 24.0, 0.0, 200.0, &mut cx),
        break_force: ranged(sn.break_force, "break_force", 32.0, 0.0, 500.0, &mut cx),
        corners: sn.corners.unwrap_or(true),
        centers: sn.centers.unwrap_or(false),
    };
    let fit_padding = ranged(
        raw.zoom.fit_padding,
        "fit_padding",
        80.0,
        0.0,
        1000.0,
        &mut cx,
    );
    for k in raw.zoom.rest.keys() {
        cx.warn_at(k, format!("zoom.{k} пока не поддерживается"));
    }
    let zoom = Zoom {
        min: ranged(raw.zoom.min, "min", 0.1, 0.02, 0.9, &mut cx),
        step: ranged(raw.zoom.step, "step", 1.25, 1.02, 4.0, &mut cx),
        click_exits: match raw.zoom.click.as_deref() {
            None | Some("focus") => false,
            Some("exit") => true,
            Some(o) => {
                cx.err_at(
                    "click",
                    format!("zoom.click = «{o}»: допустимо focus или exit"),
                );
                false
            }
        },
    };
    match raw.zoom.backend.as_deref() {
        None | Some("thumbnails") => {}
        Some("capture") => cx.warn_at(
            "backend",
            "zoom.backend = \"capture\" пока не поддерживается, используется thumbnails",
        ),
        Some(o) => cx.err_at(
            "backend",
            format!("zoom.backend = «{o}»: допустимо thumbnails или capture"),
        ),
    }
    let background = compile_background(&raw.background, &mut cx);
    let monitors = Monitors {
        names: raw
            .monitors
            .names
            .iter()
            .map(|n| n.trim().trim_start_matches(r"\\.\").to_lowercase())
            .filter(|n| !n.is_empty())
            .collect(),
        isolate: raw.monitors.isolate.unwrap_or(true),
    };
    let session = Session {
        suspend_on_close: raw.session.suspend_on_close.unwrap_or(false),
        restore_windows: raw.session.restore_windows.unwrap_or(true),
        restore_camera: raw.session.restore_camera.unwrap_or(true),
        restore_bookmarks: raw.session.restore_bookmarks.unwrap_or(true),
    };
    let ipc = Ipc {
        expose_titles: raw.ipc.expose_titles.unwrap_or(true),
    };
    let hc = &raw.hot_corners;
    let corner = |v: &Option<String>, name: &str, cx: &mut Ctx| -> Option<Action> {
        let src = v.as_deref()?;
        match parse_action(src) {
            Ok(ParsedAction::Action(a)) => Some(a),
            Ok(ParsedAction::None) => None,
            Ok(ParsedAction::Unsupported(n)) => {
                cx.warn_at(
                    name,
                    format!("действие «{n}» пока не поддерживается, угол {name} пропущен"),
                );
                None
            }
            Err(e) => {
                cx.err_at(name, format!("угол {name}: {e}"));
                None
            }
        }
    };
    let hot_corners = HotCorners {
        threshold: pos_int(hc.threshold, "threshold", 1, 50, &mut cx).unwrap_or(4) as i32,
        top_left: corner(&hc.top_left, "top_left", &mut cx),
        top_right: corner(&hc.top_right, "top_right", &mut cx),
        bottom_left: corner(&hc.bottom_left, "bottom_left", &mut cx),
        bottom_right: corner(&hc.bottom_right, "bottom_right", &mut cx),
    };

    let mut rules = Vec::new();
    for (i, r) in raw.window_rules.iter().enumerate() {
        match rules::compile(r, i + 1) {
            Ok((rule, ws)) => {
                rules.push(rule);
                for w in ws {
                    cx.warn(w);
                }
            }
            Err(e) => {
                let needle = r
                    .app_id
                    .as_deref()
                    .or(r.class.as_deref())
                    .or(r.title.as_deref())
                    .unwrap_or("window_rules");
                cx.err_at(needle, e);
            }
        }
    }

    if !cx.errors.is_empty() {
        return Err(cx.errors);
    }
    let windows = Windows {
        only: raw
            .windows
            .only
            .iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect(),
        exclude: raw
            .windows
            .exclude
            .iter()
            .map(|e| Exclude {
                exe: e.exe.as_ref().map(|s| s.to_lowercase()),
                class: e.class.clone(),
            })
            .collect(),
        on_exit,
        max_fps,
    };
    Ok(Loaded {
        config: Config {
            mod_key,
            windows,
            navigation: Navigation {
                nudge_step,
                resize_step,
                edge_pan,
                pan_step,
                camera_speed,
                mouse_speed,
                auto_navigate: n.auto_navigate.unwrap_or(true),
                bookmarks,
            },
            bindings: Bindings {
                keys: keys_v,
                panic,
                mouse,
            },
            rules,
            snap,
            fit_padding,
            hot_corners,
            zoom,
            background,
            monitors,
            session,
            ipc,
        },
        warnings: cx.warnings,
    })
}

fn toml_error(src: &str, e: &toml::de::Error) -> ConfigError {
    let (line, col) = match e.span() {
        Some(s) => {
            let before = &src[..s.start.min(src.len())];
            let line = before.matches('\n').count() + 1;
            let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            (Some(line), Some(col))
        }
        None => (None, None),
    };
    ConfigError {
        line,
        col,
        msg: e.message().to_string(),
    }
}

impl Default for Config {
    fn default() -> Self {
        parse("").expect("пустой конфиг корректен").config
    }
}

impl Config {
    /// Применить все подходящие правила окна (см. §2.7 плана).
    pub fn resolve(&self, exe: &str, class: &str, title: &str) -> Resolved {
        rules::resolve(&self.rules, exe, class, title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vk(c: char) -> u16 {
        c.to_ascii_uppercase() as u16
    }

    fn find(c: &Config, mods: Mods, vk: u16) -> Option<Action> {
        c.bindings
            .keys
            .iter()
            .find(|k| k.mods == mods && k.vk == vk)
            .map(|k| k.action)
    }

    const WIN_CTRL: Mods = Mods {
        win: true,
        ctrl: true,
        shift: false,
        alt: false,
    };

    #[test]
    fn empty_config_has_defaults() {
        let l = parse("").unwrap();
        assert!(l.warnings.is_empty());
        let c = l.config;
        assert_eq!(
            find(&c, WIN_CTRL, 0x27),
            Some(Action::PanViewport(Dir::Right))
        );
        assert_eq!(find(&c, WIN_CTRL, vk('c')), Some(Action::CenterWindow));
        assert_eq!(find(&c, WIN_CTRL, 0x33), Some(Action::GoToBookmark(3)));
        assert!(c.bindings.mouse.iter().any(|b| b.mods == WIN_CTRL
            && b.button == MouseButton::Left
            && b.action == MouseAction::Drag(DragKind::Pan)));
        assert_eq!(c.snap, SnapCfg::default());
        assert_eq!(
            find(
                &c,
                Mods {
                    win: true,
                    ..Default::default()
                },
                vk('f')
            ),
            Some(Action::ToggleFullscreen)
        );
        assert_eq!(c.bindings.panic.1, vk('r'));
        assert_eq!(c.navigation.pan_step, 100.0);
    }

    #[test]
    fn user_binding_overrides_and_none_removes() {
        let src = r#"
[keybindings]
"mod+ctrl+right" = "pan-viewport left"
"mod+ctrl+h" = "none"
"mod+ctrl+q" = "quit"
"#;
        let c = parse(src).unwrap().config;
        assert_eq!(
            find(&c, WIN_CTRL, 0x27),
            Some(Action::PanViewport(Dir::Left))
        );
        assert_eq!(find(&c, WIN_CTRL, vk('h')), None);
        assert_eq!(find(&c, WIN_CTRL, vk('q')), Some(Action::Quit));
    }

    #[test]
    fn disable_defaults_keys_and_mouse() {
        let src = "[bindings]\ndisable_defaults = [\"keys\", \"mouse\"]\n[keybindings]\n\"mod+ctrl+z\" = \"home-toggle\"\n";
        let c = parse(src).unwrap().config;
        assert_eq!(c.bindings.keys.len(), 1);
        assert!(c.bindings.mouse.is_empty());
    }

    #[test]
    fn bad_value_reports_line() {
        let src = "mod_key = \"super\"\n\n[keybindings]\n\"mod+ctrl+right\" = \"pan-viewport sideways\"\n";
        let e = parse(src).unwrap_err();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].line, Some(4), "{}", e[0]);
        assert!(e[0].msg.contains("sideways"));
    }

    #[test]
    fn toml_syntax_error_has_line_and_col() {
        let e = parse("a = \nb = 1\n").unwrap_err();
        assert!(e[0].line.is_some() && e[0].col.is_some());
        let e = parse("[windows]\nmax_fps = \"fast\"\n").unwrap_err();
        assert_eq!(e[0].line, Some(2));
    }

    #[test]
    fn unknown_keys_warn_and_unsupported_warn() {
        let src = "colour = 1\n[navigation]\npan_stp = 5\nanchors = []\n[keybindings]\n\"mod+ctrl+z\" = \"exec\"\n";
        let l = parse(src).unwrap();
        let w = l.warnings.join("\n");
        assert!(w.contains("colour"), "{w}");
        assert!(w.contains("pan_stp"), "{w}");
        assert!(w.contains("anchors"), "{w}");
        assert!(w.contains("exec"), "{w}");
    }

    #[test]
    fn dangerous_combos_are_rejected() {
        for combo in [
            "ctrl+c",
            "alt+f4",
            "ctrl+shift+t",
            "shift+a",
            "a",
            "ctrl+alt+delete",
            "win+l",
        ] {
            let src = format!("[keybindings]\n\"{combo}\" = \"quit\"\n");
            assert!(parse(&src).is_err(), "{combo} должна быть отклонена");
        }
        assert!(parse("[keybindings]\n\"ctrl+alt+q\" = \"quit\"\n").is_ok());
        assert!(parse("[keybindings]\n\"mod+q\" = \"quit\"\n").is_ok());
    }

    #[test]
    fn mod_key_alt_changes_defaults_and_ctrl_is_rejected() {
        let c = parse("mod_key = \"alt\"").unwrap().config;
        let alt_ctrl = Mods {
            win: false,
            ctrl: true,
            shift: false,
            alt: true,
        };
        assert_eq!(
            find(&c, alt_ctrl, 0x27),
            Some(Action::PanViewport(Dir::Right))
        );
        let alt_ctrl_shift = Mods {
            shift: true,
            ..alt_ctrl
        };
        assert_eq!(
            find(&c, alt_ctrl_shift, 0x25),
            Some(Action::FocusNearest(Dir::Left))
        );
        assert!(parse("mod_key = \"ctrl\"").is_err());
        assert!(parse("mod_key = \"hyper\"").is_err());
    }

    #[test]
    fn layout_independent_letters() {
        let c = parse("[keybindings]\n\"mod+ctrl+Q\" = \"quit\"\n")
            .unwrap()
            .config;
        assert_eq!(find(&c, WIN_CTRL, vk('q')), Some(Action::Quit));
    }

    #[test]
    fn mouse_binding_override() {
        let c = parse("[mouse.anywhere]\n\"mod+alt+left\" = \"pan-viewport\"\n")
            .unwrap()
            .config;
        let m = Mods {
            win: true,
            alt: true,
            ..Default::default()
        };
        assert!(c.bindings.mouse.iter().any(|b| b.mods == m
            && b.button == MouseButton::Left
            && b.action == MouseAction::Drag(DragKind::Pan)));
    }

    #[test]
    fn default_mouse_has_move_resize_fit_fill() {
        let c = Config::default();
        let find_m = |mods: Mods, b: MouseButton| {
            c.bindings
                .mouse
                .iter()
                .find(|x| x.mods == mods && x.button == b)
                .map(|x| x.action)
        };
        let alt = Mods {
            alt: true,
            ..Default::default()
        };
        assert_eq!(
            find_m(alt, MouseButton::Left),
            Some(MouseAction::Drag(DragKind::MoveWindow))
        );
        assert_eq!(
            find_m(Mods { shift: true, ..alt }, MouseButton::Left),
            Some(MouseAction::Drag(DragKind::MoveCluster))
        );
        assert_eq!(
            find_m(alt, MouseButton::Right),
            Some(MouseAction::Drag(DragKind::ResizeCluster))
        );
        assert_eq!(
            find_m(alt, MouseButton::Middle),
            Some(MouseAction::Click(Action::FitWindowSnapped))
        );
        let win = Mods {
            win: true,
            ..Default::default()
        };
        assert_eq!(
            find_m(win, MouseButton::Middle),
            Some(MouseAction::Click(Action::FillWindow))
        );
    }

    #[test]
    fn mouse_combo_needs_win_or_alt_and_known_button() {
        assert!(parse("[mouse.anywhere]\n\"ctrl+left\" = \"move-window\"\n").is_err());
        assert!(parse("[mouse.anywhere]\n\"alt+back\" = \"move-window\"\n").is_err());
        assert!(parse("[mouse.anywhere]\n\"alt+right\" = \"resize-window\"\n").is_ok());
    }

    #[test]
    fn snap_edge_pan_and_hot_corners_are_parsed_and_validated() {
        let src = r#"
[snap]
gap = 20
distance = 30
centers = true
[navigation]
nudge_step = 50
[navigation.edge_pan]
zone = 150
cursor_pan = true
[zoom]
fit_padding = 40
[hot_corners]
threshold = 6
top_left = "home-toggle"
bottom_right = "none"
"#;
        let l = parse(src).unwrap();
        let c = l.config;
        assert_eq!(c.snap.gap, 20.0);
        assert!(c.snap.centers && c.snap.corners);
        assert_eq!(c.navigation.nudge_step, 50.0);
        assert_eq!(c.navigation.edge_pan.zone, 150.0);
        assert!(c.navigation.edge_pan.cursor_pan);
        assert_eq!(c.fit_padding, 40.0);
        assert_eq!(c.hot_corners.threshold, 6);
        assert_eq!(c.hot_corners.top_left, Some(Action::HomeToggle));
        assert_eq!(c.hot_corners.bottom_right, None);
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        assert!(parse("[snap]\ngap = -1\n").is_err());
        assert!(parse("[navigation.edge_pan]\nspeed_min = 20\nspeed_max = 5\n").is_err());
        assert!(parse("[hot_corners]\ntop_left = \"fly\"\n").is_err());
        let w = parse("[zoom]\nspeed = 1.2\n").unwrap().warnings.join("\n");
        assert!(w.contains("zoom.speed"), "{w}");
        assert_eq!(parse("").unwrap().config.zoom.min, 0.1);
        let c = parse("").unwrap().config;
        assert!(!c.zoom.click_exits && c.windows.max_fps == 0);
        assert!(
            parse(
                "[zoom]
click = \"exit\"
"
            )
            .unwrap()
            .config
            .zoom
            .click_exits
        );
        assert!(
            parse(
                "[zoom]
click = \"x\"
"
            )
            .is_err()
        );
        assert!(
            parse(
                "[windows]
max_fps = 5
"
            )
            .is_err()
        );
        assert_eq!(
            parse(
                "[windows]
max_fps = 144
"
            )
            .unwrap()
            .config
            .windows
            .max_fps,
            144
        );
        let m = parse("").unwrap().config.monitors;
        assert!(m.names.is_empty() && m.isolate);
        let m = parse(
            "[monitors]
use = [\"DISPLAY2\", \" Primary \"]
isolate = false
",
        )
        .unwrap()
        .config
        .monitors;
        assert_eq!(m.names, ["display2", "primary"]);
        assert!(!m.isolate);
        assert!(parse("[zoom]\nmin = 1.5\n").is_err());
        assert!(parse("[background]\ncolor = \"red\"\n").is_err());
        let src = "[background]\ntype = \"tile\"\ncolor = \"#102030\"\non_desktop = true\n";
        let b = parse(src).unwrap().config.background;
        assert_eq!(
            (b.kind, b.color, b.on_desktop),
            (BgType::Tile, [16, 32, 48], true)
        );
    }

    #[test]
    fn new_actions_parse() {
        assert_eq!(
            parse_action("move-to-snap left"),
            Ok(ParsedAction::Action(Action::MoveToSnap(Dir::Left)))
        );
        assert_eq!(
            parse_action("fill-window"),
            Ok(ParsedAction::Action(Action::FillWindow))
        );
        assert!(parse_action("grow-window").is_err());
    }

    #[test]
    fn rules_match_and_merge() {
        let src = r#"
[[window_rules]]
app_id = "clock.exe"
widget = true
position = [10, 20]

[[window_rules]]
app_id = "*.exe"
title = "/^Ча[сй]/"
size = [300, 200]
widget = false

[[window_rules]]
class = "Chrome_WidgetWin_1"
pinned_to_screen = true
"#;
        let c = parse(src).unwrap().config;
        let r = c.resolve("Clock.EXE", "X", "Часы");
        assert!(r.widget, "sticky-on: false позже не снимает");
        assert_eq!(r.position, Some((10.0, 20.0)));
        assert_eq!(r.size, Some((300, 200)));
        assert!(!r.pinned_to_screen);
        let r = c.resolve("a.exe", "Chrome_WidgetWin_1", "Чай");
        assert!(!r.widget);
        assert!(r.pinned_to_screen);
        assert_eq!(r.size, Some((300, 200)));
        assert_eq!(c.resolve("a.exe", "X", "Другое"), Resolved::default());
    }

    #[test]
    fn rule_needs_a_criterion_and_valid_regex() {
        assert!(parse("[[window_rules]]\nwidget = true\n").is_err());
        assert!(parse("[[window_rules]]\ntitle = \"/(/\"\n").is_err());
    }

    #[test]
    fn range_errors() {
        assert!(parse("[navigation]\npan_step = 0\n").is_err());
        assert!(parse("[navigation]\ncamera_speed = 2.0\n").is_err());
        assert!(parse("[windows]\nmax_fps = 1000\n").is_err());
        assert!(parse("[navigation.bookmarks]\n\"0\" = [1, 2]\n").is_err());
        let c = parse("[navigation.bookmarks]\n\"2\" = [100, -50]\n")
            .unwrap()
            .config;
        assert_eq!(c.navigation.bookmarks.get(&2), Some(&(100.0, -50.0)));
    }

    #[test]
    fn session_ipc_and_suspend_rules() {
        let d = parse("").unwrap();
        assert!(d.warnings.is_empty());
        let c = d.config;
        assert_eq!(
            c.session,
            Session {
                suspend_on_close: false,
                restore_windows: true,
                restore_camera: true,
                restore_bookmarks: true
            }
        );
        assert!(c.ipc.expose_titles);
        assert!(
            c.bindings
                .keys
                .iter()
                .any(|k| k.action == Action::SuspendWindow)
        );
        let l = parse(
            "[session]
suspend_on_close = true
restore_windows = false
[ipc]
expose_titles = false
             [[window_rules]]
app_id = \"notepad.exe\"
suspend_on_close = false
             [[window_rules]]
app_id = \"*\"
restore_windows = true
",
        )
        .unwrap();
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        assert!(l.config.session.suspend_on_close && !l.config.session.restore_windows);
        assert!(!l.config.ipc.expose_titles);
        let r = l.config.resolve("notepad.exe", "", "");
        assert_eq!(r.suspend_on_close, Some(false));
        assert_eq!(r.restore_windows, Some(true));
        assert_eq!(l.config.resolve("a.exe", "", "").suspend_on_close, None);
        assert_eq!(
            parse_action_str("suspend-window"),
            Ok(Action::SuspendWindow)
        );
        assert!(parse_action_str("spawn x").is_err());
    }
}
