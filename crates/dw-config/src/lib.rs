//! Конфигурация DriftWin: схема TOML, разбор привязок, правила окон, валидация.
//!
//! Неизвестные ключи дают предупреждение, неверные значения — ошибку с номером строки.
//! Ключи из плана, которые ещё не реализованы, принимаются без ошибки (список — `docs/CONFIG.md`);
//! если пользователь задал их явно, выдаётся предупреждение «пока не поддерживается».

mod keys;
mod rules;

use std::collections::BTreeMap;
use std::fmt;

use dw_core::Dir;
use dw_core::action::{Action, Bindings, KeyBinding, Mods};
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
    pub max_fps: u32,
}

#[derive(Debug, Clone)]
pub struct Navigation {
    pub pan_step: f64,
    /// Доля оставшегося пути камеры, проходимая за кадр (0..1].
    pub camera_speed: f64,
    pub mouse_speed: f64,
    pub auto_navigate: bool,
    pub bookmarks: BTreeMap<u8, (f64, f64)>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub mod_key: ModKey,
    pub windows: Windows,
    pub navigation: Navigation,
    pub bindings: Bindings,
    pub rules: Vec<Rule>,
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
    zoom: Option<toml::Value>,
    background: Option<toml::Value>,
    snap: Option<toml::Value>,
    decorations: Option<toml::Value>,
    effects: Option<toml::Value>,
    gestures: Option<toml::Value>,
    touch: Option<toml::Value>,
    outputs: Option<toml::Value>,
    session: Option<toml::Value>,
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
    nudge_step: Option<toml::Value>,
    resize_step: Option<toml::Value>,
    anchors: Option<toml::Value>,
    edge_pan: Option<toml::Value>,
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
    "suspend-window",
    "nudge-window",
    "move-to-snap",
    "grow-window",
    "shrink-window",
    "resize-to-snap",
    "zoom-in",
    "zoom-out",
    "zoom-reset",
    "move-to-bookmark",
    "zoom-to-fit",
    "zoom-to-fit-snapped",
    "toggle-fullscreen",
    "fit-window",
    "fit-window-snapped",
    "fill-window",
    "toggle-cursor-pan",
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

fn compile_mouse(raw: &RawConfig, m: ModKey, cx: &mut Ctx) -> Option<Mods> {
    let mut drag = if raw.bindings.disable_defaults.iter().any(|d| d == "mouse") {
        None
    } else {
        keys::parse_combo("mod+ctrl+left", m).ok().map(|k| k.0)
    };
    for (combo, act) in &raw.mouse.anywhere {
        let k = match keys::parse_mouse_combo(combo, m) {
            Ok(k) => k,
            Err(e) => {
                cx.err_at(combo, format!("привязка мыши «{combo}»: {e}"));
                continue;
            }
        };
        match act.trim() {
            "none" => {
                if drag == Some(k) {
                    drag = None;
                }
            }
            // для мыши `pan-viewport` без направления означает «двигать холст перетаскиванием»
            "pan-viewport" => drag = Some(k),
            other => cx.warn_at(
                combo,
                format!("для мыши пока поддерживается только pan-viewport, «{other}» пропущено"),
            ),
        }
    }
    drag
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
    let drag_pan = compile_mouse(&raw, mod_key, &mut cx);

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
    let max_fps = pos_int(raw.windows.max_fps, "max_fps", 10, 240, &mut cx).unwrap_or(60) as u32;

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
        ("nudge_step", n.nudge_step.is_some()),
        ("resize_step", n.resize_step.is_some()),
        ("anchors", n.anchors.is_some()),
        ("edge_pan", n.edge_pan.is_some()),
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
        ("zoom", raw.zoom.is_some()),
        ("background", raw.background.is_some()),
        ("snap", raw.snap.is_some()),
        ("decorations", raw.decorations.is_some()),
        ("effects", raw.effects.is_some()),
        ("gestures", raw.gestures.is_some()),
        ("touch", raw.touch.is_some()),
        ("outputs", raw.outputs.is_some()),
        ("session", raw.session.is_some()),
    ] {
        if set {
            cx.warn_at(name, format!("секция [{name}] пока не поддерживается"));
        }
    }
    if !raw.autostart.is_empty() {
        cx.warn_at("autostart", "autostart пока не поддерживается");
    }

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
                pan_step,
                camera_speed,
                mouse_speed,
                auto_navigate: n.auto_navigate.unwrap_or(true),
                bookmarks,
            },
            bindings: Bindings {
                keys: keys_v,
                panic,
                drag_pan,
            },
            rules,
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
        assert_eq!(c.bindings.drag_pan, Some(WIN_CTRL));
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
        assert_eq!(c.bindings.drag_pan, None);
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
        let src = "colour = 1\n[navigation]\npan_stp = 5\nnudge_step = 20\n[keybindings]\n\"mod+ctrl+z\" = \"zoom-in\"\n";
        let l = parse(src).unwrap();
        let w = l.warnings.join("\n");
        assert!(w.contains("colour"), "{w}");
        assert!(w.contains("pan_stp"), "{w}");
        assert!(w.contains("nudge_step"), "{w}");
        assert!(w.contains("zoom-in"), "{w}");
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
        assert_eq!(c.bindings.drag_pan, Some(m));
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
}
