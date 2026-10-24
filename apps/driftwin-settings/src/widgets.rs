//! Поля ввода: комбинация клавиш/мыши и действие.

use dw_config::catalog::{ACTIONS, Arg, MOUSE_DRAGS};
use eframe::egui::{self, Event, Key, Ui};

const MODS: [&str; 5] = ["mod", "win", "ctrl", "alt", "shift"];
const DIRS: [&str; 4] = ["up", "down", "left", "right"];

/// Комбинация, разобранная на модификаторы и клавишу (или кнопку мыши).
#[derive(Clone, Default, PartialEq)]
pub struct ComboFields {
    mods: [bool; 5],
    key: String,
    capturing: bool,
}

impl ComboFields {
    pub fn parse(s: &str) -> Self {
        let mut f = Self::default();
        for p in s.split('+').map(|p| p.trim().to_lowercase()) {
            let p = match p.as_str() {
                "super" | "meta" => "win".to_string(),
                "control" => "ctrl".to_string(),
                _ => p,
            };
            match MODS.iter().position(|m| *m == p) {
                Some(i) => f.mods[i] = true,
                None => f.key = p,
            }
        }
        f
    }

    pub fn with_mod() -> Self {
        let mut f = Self::default();
        f.mods[0] = true;
        f
    }

    pub fn compose(&self) -> String {
        let mut parts: Vec<&str> = MODS
            .iter()
            .zip(self.mods)
            .filter_map(|(m, on)| on.then_some(*m))
            .collect();
        if !self.key.is_empty() {
            parts.push(&self.key);
        }
        parts.join("+")
    }

    /// Нарисовать поля; `true`, если значение изменилось.
    pub fn ui(&mut self, ui: &mut Ui, mouse: bool) -> bool {
        let before = self.clone();
        ui.horizontal(|ui| {
            for (i, name) in MODS.iter().enumerate() {
                let label = match *name {
                    "mod" => "mod",
                    "win" => "Win",
                    "ctrl" => "Ctrl",
                    "alt" => "Alt",
                    _ => "Shift",
                };
                ui.toggle_value(&mut self.mods[i], label);
            }
            ui.label("+");
            if mouse {
                if !["left", "right", "middle"].contains(&self.key.as_str()) {
                    self.key = "left".into();
                }
                egui::ComboBox::from_id_salt(ui.next_auto_id())
                    .selected_text(match self.key.as_str() {
                        "left" => "левая кнопка",
                        "right" => "правая кнопка",
                        _ => "средняя кнопка",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.key, "left".into(), "левая кнопка");
                        ui.selectable_value(&mut self.key, "right".into(), "правая кнопка");
                        ui.selectable_value(&mut self.key, "middle".into(), "средняя кнопка");
                    });
            } else {
                ui.add(
                    egui::TextEdit::singleline(&mut self.key)
                        .desired_width(80.0)
                        .hint_text("клавиша"),
                );
                let label = if self.capturing {
                    "нажмите клавишу… (Esc — отмена)"
                } else {
                    "Записать клавишу"
                };
                if ui.selectable_label(self.capturing, label).clicked() {
                    self.capturing = !self.capturing;
                }
                if self.capturing {
                    self.capture(ui);
                }
            }
        });
        *self != before
    }

    fn capture(&mut self, ui: &Ui) {
        ui.ctx().request_repaint();
        let events = ui.ctx().input(|i| i.events.clone());
        for ev in events {
            if let Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = ev
            {
                if key == Key::Escape {
                    self.capturing = false;
                    return;
                }
                if let Some(name) = key_name(key) {
                    self.key = name;
                    // Win система не передаёт окну, поэтому «mod» и «Win» остаются как выставлены
                    self.mods[2] = modifiers.ctrl;
                    self.mods[3] = modifiers.alt;
                    self.mods[4] = modifiers.shift;
                    self.capturing = false;
                    return;
                }
            }
        }
    }
}

fn key_name(k: Key) -> Option<String> {
    Some(
        match k {
            Key::ArrowLeft => "left",
            Key::ArrowRight => "right",
            Key::ArrowUp => "up",
            Key::ArrowDown => "down",
            Key::Tab => "tab",
            Key::Space => "space",
            Key::Enter => "enter",
            Key::Backspace => "backspace",
            Key::Delete => "delete",
            Key::Insert => "insert",
            Key::Home => "home",
            Key::End => "end",
            Key::PageUp => "pageup",
            Key::PageDown => "pagedown",
            Key::OpenBracket => "lbracket",
            Key::CloseBracket => "rbracket",
            Key::Minus => "minus",
            Key::Equals | Key::Plus => "equals",
            other => {
                let d = format!("{other:?}");
                return if let Some(n) = d.strip_prefix("Num") {
                    Some(n.to_string())
                } else if d.len() == 1 || (d.starts_with('F') && d[1..].parse::<u8>().is_ok()) {
                    Some(d.to_lowercase())
                } else {
                    None
                };
            }
        }
        .to_string(),
    )
}

/// Действие с аргументом: «pan-viewport right».
#[derive(Clone, Default, PartialEq)]
pub struct ActionFields {
    pub name: String,
    pub arg: String,
}

impl ActionFields {
    pub fn parse(s: &str) -> Self {
        let mut it = s.split_whitespace();
        Self {
            name: it.next().unwrap_or("").to_string(),
            arg: it.next().unwrap_or("").to_string(),
        }
    }

    pub fn compose(&self) -> String {
        if self.arg.is_empty() {
            self.name.clone()
        } else {
            format!("{} {}", self.name, self.arg)
        }
    }

    fn arg_kind(&self) -> Arg {
        ACTIONS
            .iter()
            .find(|a| a.name == self.name)
            .map_or(Arg::None, |a| a.arg)
    }

    fn label(&self) -> String {
        if self.name.is_empty() {
            return "— нет —".into();
        }
        if let Some(a) = ACTIONS.iter().find(|a| a.name == self.name) {
            return a.label.to_string();
        }
        MOUSE_DRAGS
            .iter()
            .find(|(n, _)| *n == self.name)
            .map_or_else(|| self.name.clone(), |(_, l)| (*l).to_string())
    }

    /// `allow_none` — пункт «— нет —»; `drags` — добавить непрерывные действия мыши.
    pub fn ui(&mut self, ui: &mut Ui, allow_none: bool, drags: bool) -> bool {
        let before = self.clone();
        ui.horizontal(|ui| {
            let mut picked: Option<String> = None;
            egui::ComboBox::from_id_salt(ui.next_auto_id())
                .width(260.0)
                .selected_text(self.label())
                .show_ui(ui, |ui| {
                    if allow_none
                        && ui
                            .selectable_label(self.name.is_empty(), "— нет —")
                            .clicked()
                    {
                        picked = Some(String::new());
                    }
                    if drags {
                        for (n, l) in MOUSE_DRAGS {
                            if ui.selectable_label(self.name == *n, *l).clicked() {
                                picked = Some((*n).to_string());
                            }
                        }
                    }
                    for a in ACTIONS {
                        if ui
                            .selectable_label(self.name == a.name, a.label)
                            .on_hover_text(a.name)
                            .clicked()
                        {
                            picked = Some(a.name.to_string());
                        }
                    }
                });
            if let Some(n) = picked {
                self.name = n;
                self.arg = match self.arg_kind() {
                    Arg::Dir => "up".into(),
                    Arg::Number => "1".into(),
                    Arg::Order => "forward".into(),
                    Arg::None => String::new(),
                };
            }
            match self.arg_kind() {
                Arg::Dir => choice(ui, &mut self.arg, &DIRS),
                Arg::Order => choice(ui, &mut self.arg, &["forward", "backward"]),
                Arg::Number => {
                    let mut n: u8 = self.arg.parse().unwrap_or(1);
                    ui.add(egui::DragValue::new(&mut n).range(1..=9).prefix("№ "));
                    self.arg = n.to_string();
                }
                Arg::None => {}
            }
        });
        *self != before
    }
}

fn choice(ui: &mut Ui, cur: &mut String, items: &[&str]) {
    egui::ComboBox::from_id_salt(ui.next_auto_id())
        .width(80.0)
        .selected_text(match cur.as_str() {
            "up" => "вверх",
            "down" => "вниз",
            "left" => "влево",
            "right" => "вправо",
            "forward" => "вперёд",
            "backward" => "назад",
            other => other,
        })
        .show_ui(ui, |ui| {
            for it in items {
                let l = match *it {
                    "up" => "вверх",
                    "down" => "вниз",
                    "left" => "влево",
                    "right" => "вправо",
                    "forward" => "вперёд",
                    "backward" => "назад",
                    o => o,
                };
                ui.selectable_value(cur, (*it).to_string(), l);
            }
        });
}
