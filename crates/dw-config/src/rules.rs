//! Правила окон `[[window_rules]]` (§2.7 плана): сопоставление и слияние.

use std::collections::BTreeMap;

use regex::Regex;
use serde::Deserialize;

/// Шаблон: точная строка, glob (`*`, `?`) или регулярное выражение в `/…/`. Без учёта регистра.
#[derive(Debug, Clone)]
pub enum Matcher {
    Exact(String),
    Glob(String),
    Regex(Regex),
}

impl Matcher {
    pub fn new(p: &str) -> Result<Self, String> {
        if p.len() > 2 && p.starts_with('/') && p.ends_with('/') {
            return Regex::new(&format!("(?i){}", &p[1..p.len() - 1]))
                .map(Matcher::Regex)
                .map_err(|e| format!("некорректное регулярное выражение {p}: {e}"));
        }
        let l = p.to_lowercase();
        Ok(if l.contains('*') || l.contains('?') {
            Matcher::Glob(l)
        } else {
            Matcher::Exact(l)
        })
    }

    pub fn is_match(&self, s: &str) -> bool {
        match self {
            Matcher::Exact(e) => s.to_lowercase() == *e,
            Matcher::Glob(g) => glob(g.as_bytes(), s.to_lowercase().as_bytes()),
            Matcher::Regex(r) => r.is_match(s),
        }
    }
}

/// Glob без рекурсии: `*` — любая последовательность, `?` — один байт.
fn glob(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct RawRule {
    pub app_id: Option<String>,
    pub class: Option<String>,
    pub title: Option<String>,
    position: Option<[f64; 2]>,
    size: Option<[i32; 2]>,
    widget: Option<bool>,
    pinned_to_screen: Option<bool>,
    /// Остальные ключи: известные из плана без реализации или опечатки.
    #[serde(flatten)]
    extra: BTreeMap<String, toml::Value>,
}

/// Поля правил из плана, которые ещё не реализованы.
const PLANNED: &[&str] = &[
    "fullscreen",
    "focus_on_open",
    "suspend_on_close",
    "restore_windows",
    "preserve_aspect_ratio",
    "decoration",
    "blur",
    "opacity",
    "border_width",
    "border_color",
    "border_color_focused",
    "corner_radius",
    "shadow",
    "output",
    "pass_keys",
    "pass_mouse",
    "layer_order",
];

#[derive(Debug, Clone)]
pub struct Rule {
    app_id: Option<Matcher>,
    class: Option<Matcher>,
    title: Option<Matcher>,
    widget: Option<bool>,
    pinned_to_screen: Option<bool>,
    position: Option<(f64, f64)>,
    size: Option<(i32, i32)>,
}

/// Итог применения всех подходящих правил.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resolved {
    pub widget: bool,
    pub pinned_to_screen: bool,
    /// Центр окна на холсте (Y вверх).
    pub position: Option<(f64, f64)>,
    pub size: Option<(i32, i32)>,
}

pub(crate) fn compile(r: &RawRule, idx: usize) -> Result<(Rule, Vec<String>), String> {
    if r.app_id.is_none() && r.class.is_none() && r.title.is_none() {
        return Err(format!(
            "правило №{idx}: нужен хотя бы один критерий (app_id, class или title)"
        ));
    }
    let m = |s: &Option<String>, name: &str| -> Result<Option<Matcher>, String> {
        s.as_deref()
            .map(Matcher::new)
            .transpose()
            .map_err(|e| format!("правило №{idx}, {name}: {e}"))
    };
    if let Some([w, h]) = r.size
        && (w < 50 || h < 50)
    {
        return Err(format!(
            "правило №{idx}: size [{w}, {h}] слишком мал (минимум 50x50)"
        ));
    }
    let mut warns = Vec::new();
    for k in r.extra.keys() {
        if PLANNED.contains(&k.as_str()) {
            warns.push(format!("правило №{idx}: поле «{k}» пока не поддерживается"));
        } else {
            warns.push(format!(
                "правило №{idx}: неизвестное поле «{k}» проигнорировано"
            ));
        }
    }
    Ok((
        Rule {
            app_id: m(&r.app_id, "app_id")?,
            class: m(&r.class, "class")?,
            title: m(&r.title, "title")?,
            widget: r.widget,
            pinned_to_screen: r.pinned_to_screen,
            position: r.position.map(|[x, y]| (x, y)),
            size: r.size.map(|[w, h]| (w, h)),
        },
        warns,
    ))
}

impl Rule {
    fn matches(&self, exe: &str, class: &str, title: &str) -> bool {
        self.app_id.as_ref().is_none_or(|m| m.is_match(exe))
            && self.class.as_ref().is_none_or(|m| m.is_match(class))
            && self.title.as_ref().is_none_or(|m| m.is_match(title))
    }
}

/// Все подходящие правила по порядку: скаляры — last-wins, булевы флаги — sticky-on.
pub(crate) fn resolve(rules: &[Rule], exe: &str, class: &str, title: &str) -> Resolved {
    let mut out = Resolved::default();
    for r in rules.iter().filter(|r| r.matches(exe, class, title)) {
        out.widget |= r.widget == Some(true);
        out.pinned_to_screen |= r.pinned_to_screen == Some(true);
        if r.position.is_some() {
            out.position = r.position;
        }
        if r.size.is_some() {
            out.size = r.size;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_cases() {
        assert!(glob(b"*.exe", b"chrome.exe"));
        assert!(glob(b"c?rome*", b"chrome.exe"));
        assert!(glob(b"*", b""));
        assert!(!glob(b"*.exe", b"chrome.dll"));
        assert!(glob(b"a*b*c", b"axxbyyc"));
        assert!(!glob(b"a*b*c", b"axxbyy"));
    }

    #[test]
    fn matcher_kinds_are_case_insensitive() {
        assert!(Matcher::new("Chrome.EXE").unwrap().is_match("chrome.exe"));
        assert!(
            Matcher::new("*блокнот*")
                .unwrap()
                .is_match("Без имени — Блокнот")
        );
        assert!(Matcher::new("/^draft \\d+$/").unwrap().is_match("Draft 42"));
        assert!(Matcher::new("/(/").is_err());
    }
}
