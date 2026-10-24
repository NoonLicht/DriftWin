//! Описание настроек и действий для окна настроек: ключи, диапазоны, значения по умолчанию, подписи.
//! Тесты следят, чтобы каталог не расходился с разбором конфига.

use super::{ModKey, keys};

/// Тип значения настройки.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Bool,
    Float {
        min: f64,
        max: f64,
    },
    Int {
        min: i64,
        max: i64,
    },
    Choice(&'static [&'static str]),
    /// Комбинация клавиш (`mod+ctrl+shift+r`).
    Combo,
    /// Действие (как в `[keybindings]`) или пусто.
    Action,
    /// Список строк (в TOML — массив).
    List,
    /// Цвет `#rrggbb`.
    Color,
    /// Выбор мониторов (список имён `DISPLAY1`, … в TOML).
    Monitors,
    /// Произвольная строка (например, путь к файлу).
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Def {
    Bool(bool),
    Num(f64),
    Str(&'static str),
    Empty,
}

#[derive(Debug, Clone, Copy)]
pub struct Setting {
    /// Секция TOML через точку; пусто — корень файла.
    pub section: &'static str,
    pub key: &'static str,
    pub kind: Kind,
    pub default: Def,
    pub label: &'static str,
    pub help: &'static str,
}

const fn s(
    section: &'static str,
    key: &'static str,
    kind: Kind,
    default: Def,
    label: &'static str,
    help: &'static str,
) -> Setting {
    Setting {
        section,
        key,
        kind,
        default,
        label,
        help,
    }
}

const fn f(min: f64, max: f64) -> Kind {
    Kind::Float { min, max }
}

const fn i(min: i64, max: i64) -> Kind {
    Kind::Int { min, max }
}

/// Названия секций для заголовков (в порядке показа).
pub const SECTIONS: &[(&str, &str)] = &[
    ("", "Общие"),
    ("windows", "Окна"),
    ("navigation", "Навигация"),
    ("navigation.edge_pan", "Прокрутка у края экрана"),
    ("snap", "Привязка окон"),
    ("zoom", "Масштаб и подгонка"),
    ("monitors", "Мониторы"),
    ("background", "Фон"),
    ("hot_corners", "Горячие углы"),
];

pub const SETTINGS: &[Setting] = &[
    s(
        "",
        "mod_key",
        Kind::Choice(&["super", "alt"]),
        Def::Str("super"),
        "Клавиша-модификатор (mod)",
        "Какая клавиша заменяет «mod» в привязках: Win (super) или Alt.",
    ),
    s(
        "windows",
        "only",
        Kind::List,
        Def::Empty,
        "Белый список приложений",
        "Имена exe через запятую. Управляются только эти приложения (вместо --only).",
    ),
    s(
        "windows",
        "max_fps",
        i(10, 240),
        Def::Num(60.0),
        "Частота обновления, кадров/с",
        "Как часто демон применяет раскладку окон.",
    ),
    s(
        "windows",
        "panic_key",
        Kind::Combo,
        Def::Str("mod+ctrl+shift+r"),
        "Аварийный возврат окон",
        "Возвращает все окна на исходные места и ставит управление на паузу.",
    ),
    s(
        "navigation",
        "pan_step",
        f(1.0, 5000.0),
        Def::Num(100.0),
        "Шаг прокрутки стрелками, px",
        "На сколько сдвигается вид за одно нажатие pan-viewport.",
    ),
    s(
        "navigation",
        "camera_speed",
        f(0.01, 1.0),
        Def::Num(0.3),
        "Скорость камеры",
        "Доля оставшегося пути, проходимая за кадр: меньше — плавнее, 1 — мгновенно.",
    ),
    s(
        "navigation",
        "mouse_speed",
        f(0.1, 10.0),
        Def::Num(1.0),
        "Скорость перетаскивания холста",
        "Множитель смещения при перетаскивании холста мышью.",
    ),
    s(
        "navigation",
        "auto_navigate",
        Kind::Bool,
        Def::Bool(true),
        "Автопереход к окну",
        "При фокусе на окне вне экрана камера подходит к нему.",
    ),
    s(
        "navigation",
        "nudge_step",
        f(1.0, 2000.0),
        Def::Num(20.0),
        "Шаг сдвига окна, px",
        "Для nudge-window.",
    ),
    s(
        "navigation",
        "resize_step",
        f(1.0, 2000.0),
        Def::Num(20.0),
        "Шаг изменения размера, px",
        "Для grow-window и shrink-window.",
    ),
    s(
        "navigation.edge_pan",
        "zone",
        f(10.0, 500.0),
        Def::Num(100.0),
        "Ширина зоны у края, px",
        "Зона, в которой перетаскивание окна начинает прокручивать вид.",
    ),
    s(
        "navigation.edge_pan",
        "speed_min",
        f(0.5, 100.0),
        Def::Num(4.0),
        "Минимальная скорость, px/кадр",
        "На границе зоны (при 60 кадрах/с).",
    ),
    s(
        "navigation.edge_pan",
        "speed_max",
        f(0.5, 200.0),
        Def::Num(10.0),
        "Максимальная скорость, px/кадр",
        "У самого края экрана. Не меньше минимальной.",
    ),
    s(
        "navigation.edge_pan",
        "cursor_pan",
        Kind::Bool,
        Def::Bool(false),
        "Прокрутка курсором у края",
        "Крутить вид и без перетаскивания (переключается действием toggle-cursor-pan).",
    ),
    s(
        "navigation.edge_pan",
        "cursor_zone",
        f(1.0, 200.0),
        Def::Num(20.0),
        "Зона для курсора, px",
        "Ширина зоны у края при прокрутке курсором.",
    ),
    s(
        "navigation.edge_pan",
        "latency_ms",
        i(0, 2000),
        Def::Num(120.0),
        "Задержка, мс",
        "Сколько курсор должен постоять у края, прежде чем вид поедет.",
    ),
    s(
        "snap",
        "enabled",
        Kind::Bool,
        Def::Bool(true),
        "Привязка включена",
        "Притяжка окон друг к другу при перемещении и изменении размера.",
    ),
    s(
        "snap",
        "gap",
        f(0.0, 200.0),
        Def::Num(12.0),
        "Зазор между окнами, px",
        "Меняется и на лету: Win+] и Win+[ (кластеры пересобираются).",
    ),
    s(
        "snap",
        "gap_step",
        f(1.0, 100.0),
        Def::Num(4.0),
        "Шаг изменения зазора, px",
        "Для snap-gap-increase и snap-gap-decrease.",
    ),
    s(
        "snap",
        "outer_gap",
        f(0.0, 500.0),
        Def::Num(12.0),
        "Отступ от края вида, px",
        "Для fill-window.",
    ),
    s(
        "snap",
        "distance",
        f(0.0, 200.0),
        Def::Num(24.0),
        "Расстояние притяжки, px",
        "На каком расстоянии срабатывает привязка.",
    ),
    s(
        "snap",
        "break_force",
        f(0.0, 500.0),
        Def::Num(32.0),
        "Сила отлипания, px",
        "Насколько надо дёрнуть окно, чтобы оно отлипло.",
    ),
    s(
        "snap",
        "corners",
        Kind::Bool,
        Def::Bool(true),
        "Выравнивать края",
        "Края соседних окон подтягиваются друг к другу.",
    ),
    s(
        "snap",
        "centers",
        Kind::Bool,
        Def::Bool(false),
        "Выравнивать центры",
        "Центры соседних окон подтягиваются друг к другу.",
    ),
    s(
        "zoom",
        "fit_padding",
        f(0.0, 1000.0),
        Def::Num(80.0),
        "Отступ при fit-window, px",
        "Насколько окно меньше вида после подгонки.",
    ),
    s(
        "monitors",
        "use",
        Kind::Monitors,
        Def::Empty,
        "Мониторы под управлением",
        "Пусто — только основной. Отмеченные мониторы образуют один общий холст.",
    ),
    s(
        "monitors",
        "isolate",
        Kind::Bool,
        Def::Bool(true),
        "Изоляция окон",
        "Окна не заходят на неотмеченные мониторы: пока окно перекрывает такой монитор, оно спрятано.",
    ),
    s(
        "zoom",
        "min",
        f(0.02, 0.9),
        Def::Num(0.1),
        "Минимальный масштаб обзора",
        "Насколько далеко можно отдалить вид (1 — обычный масштаб).",
    ),
    s(
        "zoom",
        "step",
        f(1.02, 4.0),
        Def::Num(1.25),
        "Шаг масштаба",
        "Во сколько раз меняют масштаб zoom-in, zoom-out и колесо мыши в обзоре.",
    ),
    s(
        "background",
        "type",
        Kind::Choice(&["default", "grid", "solid", "tile", "wallpaper", "none"]),
        Def::Str("default"),
        "Тип фона",
        "default и grid — сетка; tile — повторяющаяся картинка; wallpaper — одна картинка в начале координат.",
    ),
    s(
        "background",
        "color",
        Kind::Color,
        Def::Str("#181a20"),
        "Цвет фона",
        "",
    ),
    s(
        "background",
        "grid_color",
        Kind::Color,
        Def::Str("#303440"),
        "Цвет линий сетки",
        "",
    ),
    s(
        "background",
        "grid_size",
        f(8.0, 5000.0),
        Def::Num(100.0),
        "Шаг сетки, px",
        "Расстояние между линиями в мире.",
    ),
    s(
        "background",
        "image",
        Kind::Text,
        Def::Empty,
        "Картинка для tile и wallpaper",
        "Путь к PNG, JPEG или BMP. Пусто — обои рабочего стола.",
    ),
    s(
        "background",
        "on_desktop",
        Kind::Bool,
        Def::Bool(false),
        "Фон и на рабочем столе",
        "Рисовать фон под окнами, а не только в обзоре (скрывает значки рабочего стола). Эксперимент.",
    ),
    s(
        "hot_corners",
        "threshold",
        i(1, 100),
        Def::Num(4.0),
        "Размер угла, px",
        "Насколько близко к углу надо подвести курсор.",
    ),
    s(
        "hot_corners",
        "top_left",
        Kind::Action,
        Def::Empty,
        "Левый верхний угол",
        "Действие при входе курсора в угол основного монитора.",
    ),
    s(
        "hot_corners",
        "top_right",
        Kind::Action,
        Def::Empty,
        "Правый верхний угол",
        "",
    ),
    s(
        "hot_corners",
        "bottom_left",
        Kind::Action,
        Def::Empty,
        "Левый нижний угол",
        "",
    ),
    s(
        "hot_corners",
        "bottom_right",
        Kind::Action,
        Def::Empty,
        "Правый нижний угол",
        "",
    ),
];

/// Аргумент действия.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg {
    None,
    Dir,
    Number,
    Order,
}

#[derive(Debug, Clone, Copy)]
pub struct ActionInfo {
    pub name: &'static str,
    pub arg: Arg,
    pub label: &'static str,
}

const fn a(name: &'static str, arg: Arg, label: &'static str) -> ActionInfo {
    ActionInfo { name, arg, label }
}

/// Все реализованные действия.
pub const ACTIONS: &[ActionInfo] = &[
    a("pan-viewport", Arg::Dir, "Сдвинуть вид"),
    a("focus-nearest", Arg::Dir, "Фокус на ближайшее окно"),
    a("center-nearest", Arg::Dir, "Подвести вид к ближайшему окну"),
    a(
        "cycle-windows",
        Arg::Order,
        "Переключить окна (по недавности)",
    ),
    a("center-window", Arg::None, "Центрировать окно"),
    a("focus-center", Arg::None, "Фокус на окно в центре"),
    a("home-toggle", Arg::None, "В начало и обратно"),
    a("go-to-bookmark", Arg::Number, "Перейти к закладке"),
    a("set-bookmark", Arg::Number, "Поставить закладку"),
    a(
        "toggle-pin-to-screen",
        Arg::None,
        "Закрепить окно на экране",
    ),
    a("close-window", Arg::None, "Закрыть окно"),
    a("reload-config", Arg::None, "Перечитать конфиг"),
    a("quit", Arg::None, "Выход из DriftWin"),
    a("nudge-window", Arg::Dir, "Сдвинуть окно на шаг"),
    a(
        "move-to-snap",
        Arg::Dir,
        "Сдвинуть окно к следующей привязке",
    ),
    a("grow-window", Arg::Dir, "Растянуть сторону окна"),
    a("shrink-window", Arg::Dir, "Сжать сторону окна"),
    a("resize-to-snap", Arg::Dir, "Растянуть сторону до соседа"),
    a("toggle-fullscreen", Arg::None, "На весь монитор"),
    a(
        "fit-window-snapped",
        Arg::None,
        "Подогнать под вид (с кластером)",
    ),
    a("fit-window", Arg::None, "Подогнать под вид"),
    a("fill-window", Arg::None, "Занять свободное место"),
    a("toggle-cursor-pan", Arg::None, "Прокрутка курсором у края"),
    a("zoom-to-fit", Arg::None, "Обзор всех окон (повтор — назад)"),
    a("zoom-to-fit-snapped", Arg::None, "Обзор кластера окна"),
    a("zoom-in", Arg::None, "Приблизить"),
    a("zoom-out", Arg::None, "Отдалить"),
    a("zoom-reset", Arg::None, "Масштаб 1:1"),
    a("snap-gap-increase", Arg::None, "Зазор больше"),
    a("snap-gap-decrease", Arg::None, "Зазор меньше"),
];

/// Непрерывные действия мыши (только для `[mouse.anywhere]`).
pub const MOUSE_DRAGS: &[(&str, &str)] = &[
    ("pan-viewport", "Двигать холст"),
    ("move-window", "Двигать окно"),
    ("move-snapped-windows", "Двигать окно с кластером"),
    ("resize-window", "Менять размер окна"),
    (
        "resize-window-snapped",
        "Менять размер окна (кластер сохраняется)",
    ),
];

fn mod_key(name: &str) -> ModKey {
    if name == "alt" {
        ModKey::Alt
    } else {
        ModKey::Super
    }
}

/// Стандартные привязки клавиатуры: (комбинация, действие).
pub fn default_keys(mod_key_name: &str) -> Vec<(String, String)> {
    super::default_keybindings(mod_key(mod_key_name))
}

/// Стандартные привязки мыши: (комбинация, действие).
pub fn default_mouse_bindings(mod_key_name: &str) -> Vec<(String, String)> {
    let fill = if mod_key(mod_key_name) == ModKey::Alt {
        "mod+ctrl+middle"
    } else {
        "mod+middle"
    };
    [
        ("mod+ctrl+left", "pan-viewport"),
        ("alt+left", "move-window"),
        ("alt+shift+left", "move-snapped-windows"),
        ("alt+right", "resize-window-snapped"),
        ("alt+shift+right", "resize-window"),
        ("alt+middle", "fit-window-snapped"),
        ("alt+shift+middle", "fit-window"),
        (fill, "fill-window"),
    ]
    .into_iter()
    .map(|(c, a)| (c.to_string(), a.to_string()))
    .collect()
}

/// Проверить комбинацию клавиатуры или мыши.
pub fn check_combo(combo: &str, mod_key_name: &str, mouse: bool) -> Result<(), String> {
    let m = mod_key(mod_key_name);
    if mouse {
        keys::parse_mouse_combo(combo, m).map(|_| ())
    } else {
        keys::parse_combo(combo, m).map(|_| ())
    }
}

/// Каноническая запись комбинации: разные написания одной комбинации дают один и тот же идентификатор.
pub fn combo_id(combo: &str, mod_key_name: &str, mouse: bool) -> Option<String> {
    let m = mod_key(mod_key_name);
    if mouse {
        keys::parse_mouse_combo(combo, m)
            .ok()
            .map(|(mods, b)| format!("{mods:?}/{b:?}"))
    } else {
        keys::parse_combo(combo, m)
            .ok()
            .map(|(mods, vk)| format!("{mods:?}/{vk}"))
    }
}

/// Названия клавиш, принимаемые в комбинациях (кроме букв, цифр и F1..F24).
pub const NAMED_KEYS: &[&str] = &[
    "left",
    "up",
    "right",
    "down",
    "tab",
    "space",
    "enter",
    "escape",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "pageup",
    "pagedown",
    "lbracket",
    "rbracket",
    "minus",
    "equals",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn toml_value(st: &Setting) -> String {
        match (st.kind, st.default) {
            (_, Def::Bool(b)) => b.to_string(),
            (Kind::Int { .. }, Def::Num(n)) => format!("{}", n as i64),
            (_, Def::Num(n)) => format!("{n:?}"),
            (Kind::List | Kind::Monitors, _) => "[]".into(),
            (_, Def::Str(s)) => format!("\"{s}\""),
            (_, Def::Empty) => "\"\"".into(),
        }
    }

    #[test]
    fn defaults_are_accepted_by_parser() {
        let mut src = String::new();
        for (sec, _) in SECTIONS {
            if sec.is_empty() {
                for st in SETTINGS.iter().filter(|s| s.section.is_empty()) {
                    src += &format!("{} = {}\n", st.key, toml_value(st));
                }
            }
        }
        for (sec, _) in SECTIONS.iter().filter(|(s, _)| !s.is_empty()) {
            src += &format!("[{sec}]\n");
            for st in SETTINGS.iter().filter(|s| s.section == *sec) {
                if st.kind == Kind::Action {
                    continue;
                }
                src += &format!("{} = {}\n", st.key, toml_value(st));
            }
        }
        let l = parse(&src).unwrap_or_else(|e| panic!("{e:?}\n{src}"));
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
    }

    #[test]
    fn ranges_match_parser() {
        for st in SETTINGS {
            let (bad_lo, bad_hi) = match st.kind {
                Kind::Float { min, max } => (min - 1.0, max + 1.0),
                Kind::Int { min, max } => (min as f64 - 1.0, max as f64 + 1.0),
                _ => continue,
            };
            // 0.01 - 1 < 0 для camera_speed: берём значения, заведомо вне диапазона
            for v in [bad_lo.min(st.min_f() - 0.5), bad_hi] {
                let sec = if st.section.is_empty() {
                    String::new()
                } else {
                    format!("[{}]\n", st.section)
                };
                let src = format!("{sec}{} = {v:?}\n", st.key);
                assert!(parse(&src).is_err(), "{} = {v} должно быть ошибкой", st.key);
            }
        }
    }

    impl Setting {
        fn min_f(&self) -> f64 {
            match self.kind {
                Kind::Float { min, .. } => min,
                Kind::Int { min, .. } => min as f64,
                _ => 0.0,
            }
        }
    }

    #[test]
    fn action_catalog_parses() {
        for ai in ACTIONS {
            let arg = match ai.arg {
                Arg::None => "",
                Arg::Dir => " up",
                Arg::Number => " 1",
                Arg::Order => " forward",
            };
            let src = format!("[keybindings]\n\"mod+ctrl+x\" = \"{}{arg}\"\n", ai.name);
            let l = parse(&src).unwrap_or_else(|e| panic!("{}: {e:?}", ai.name));
            assert!(l.warnings.is_empty(), "{}: {:?}", ai.name, l.warnings);
        }
        for (n, _) in MOUSE_DRAGS {
            let src = format!("[mouse.anywhere]\n\"alt+left\" = \"{n}\"\n");
            parse(&src).unwrap_or_else(|e| panic!("{n}: {e:?}"));
        }
    }

    #[test]
    fn default_mouse_strings_match_real_defaults() {
        for m in ["super", "alt"] {
            let mk = mod_key(m);
            let mut a: Vec<_> = super::super::default_mouse(mk)
                .iter()
                .map(|b| (format!("{:?}", b.mods), format!("{:?}", b.button)))
                .collect();
            let mut b: Vec<_> = default_mouse_bindings(m)
                .iter()
                .map(|(c, _)| {
                    let (mods, btn) = keys::parse_mouse_combo(c, mk).unwrap();
                    (format!("{mods:?}"), format!("{btn:?}"))
                })
                .collect();
            a.sort();
            b.sort();
            assert_eq!(a, b, "{m}");
        }
    }

    #[test]
    fn named_keys_are_known() {
        for k in NAMED_KEYS {
            assert!(
                check_combo(&format!("mod+{k}"), "super", false).is_ok(),
                "{k}"
            );
        }
    }
}
