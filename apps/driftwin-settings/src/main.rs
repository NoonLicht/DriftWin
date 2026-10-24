//! Окно настроек DriftWin: правит config.toml (комментарии сохраняются), демон подхватывает файл сам.

mod doc;
mod widgets;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dw_config::catalog::{
    self, ACTIONS, Def, Kind, SECTIONS, SETTINGS, Setting, check_combo, combo_id,
};
use eframe::egui::{self, Color32, RichText, Ui};
use toml_edit::{Array, DocumentMut};
use widgets::{ActionFields, ComboFields};

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    General,
    Keys,
    Mouse,
    Other,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum RowKind {
    Default,
    Overridden,
    Custom,
    Disabled,
}

#[derive(Clone)]
struct Row {
    id: String,
    combo: String,
    action: String,
    kind: RowKind,
}

enum RowOp {
    Edit(Row),
    Delete(Row),
    Reset(Row),
    Enable(Row),
}

struct Edit {
    mouse: bool,
    orig: Option<Row>,
    combo: ComboFields,
    action: ActionFields,
    error: Option<String>,
}

struct App {
    path: PathBuf,
    doc: DocumentMut,
    saved_text: String,
    tab: Tab,
    combos: HashMap<String, ComboFields>,
    actions: HashMap<String, ActionFields>,
    texts: HashMap<String, String>,
    errors: HashMap<String, String>,
    edit: Option<Edit>,
    status: Vec<(Color32, String)>,
}

fn config_path() -> PathBuf {
    let mut a = std::env::args().skip(1);
    while let Some(x) = a.next() {
        if x == "--config"
            && let Some(p) = a.next()
        {
            return PathBuf::from(p);
        }
    }
    PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into()))
        .join("DriftWin")
        .join("config.toml")
}

fn read_doc(path: &Path) -> Result<(DocumentMut, String), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("не удалось прочитать {}: {e}", path.display())),
    };
    let doc = text
        .parse::<DocumentMut>()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((doc, text))
}

const RED: Color32 = Color32::from_rgb(220, 70, 70);
const ORANGE: Color32 = Color32::from_rgb(220, 150, 40);
const GREEN: Color32 = Color32::from_rgb(70, 170, 90);

impl App {
    fn new(path: PathBuf) -> Self {
        let mut status = vec![];
        let (doc, text) = match read_doc(&path) {
            Ok(x) => x,
            Err(e) => {
                status.push((
                    RED,
                    format!("{e}. Править файл здесь нельзя — исправьте его вручную."),
                ));
                (DocumentMut::new(), String::new())
            }
        };
        Self {
            path,
            doc,
            saved_text: text,
            tab: Tab::General,
            combos: HashMap::new(),
            actions: HashMap::new(),
            texts: HashMap::new(),
            errors: HashMap::new(),
            edit: None,
            status,
        }
    }

    fn mod_key(&self) -> String {
        doc::get_str(&self.doc, "", "mod_key").unwrap_or_else(|| "super".into())
    }

    fn dirty(&self) -> bool {
        self.doc.to_string() != self.saved_text
    }

    fn clear_buffers(&mut self) {
        self.combos.clear();
        self.actions.clear();
        self.texts.clear();
        self.errors.clear();
        self.edit = None;
    }

    fn reload(&mut self) {
        match read_doc(&self.path) {
            Ok((d, t)) => {
                self.doc = d;
                self.saved_text = t;
                self.clear_buffers();
                self.status = vec![(GREEN, "Изменения отменены, файл прочитан заново.".into())];
            }
            Err(e) => self.status = vec![(RED, e)],
        }
    }

    fn save(&mut self) {
        let text = self.doc.to_string();
        match dw_config::parse(&text) {
            Err(es) => {
                self.status = es
                    .iter()
                    .map(|e| (RED, format!("ошибка: {e}")))
                    .chain([(RED, "Файл не сохранён.".into())])
                    .collect();
            }
            Ok(l) => {
                if let Some(dir) = self.path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let tmp = self.path.with_extension("toml.tmp");
                let r =
                    std::fs::write(&tmp, &text).and_then(|()| std::fs::rename(&tmp, &self.path));
                match r {
                    Ok(()) => {
                        self.saved_text = text;
                        self.status = l
                            .warnings
                            .iter()
                            .map(|w| (ORANGE, format!("предупреждение: {w}")))
                            .chain([(
                                GREEN,
                                "Сохранено. Запущенный DriftWin подхватит изменения сам.".into(),
                            )])
                            .collect();
                    }
                    Err(e) => {
                        self.status = vec![(RED, format!("не удалось записать файл: {e}"))];
                    }
                }
            }
        }
    }

    // ---------- вкладка «Общие» ----------

    fn general_tab(&mut self, ui: &mut Ui) {
        for (sec, title) in SECTIONS {
            ui.add_space(8.0);
            ui.heading(*title);
            egui::Grid::new(format!("grid-{sec}"))
                .num_columns(3)
                .spacing([14.0, 8.0])
                .show(ui, |ui| {
                    for st in SETTINGS.iter().filter(|s| s.section == *sec) {
                        self.setting_row(ui, st);
                        ui.end_row();
                    }
                });
        }
    }

    fn setting_row(&mut self, ui: &mut Ui, st: &Setting) {
        let (sec, key) = (st.section, st.key);
        let id = format!("{sec}.{key}");
        let mk = self.mod_key();
        let label = ui.label(st.label);
        if !st.help.is_empty() {
            label.on_hover_text(st.help);
        }
        let num_def = match st.default {
            Def::Num(n) => n,
            _ => 0.0,
        };
        let str_def = match st.default {
            Def::Str(s) => s,
            _ => "",
        };
        match st.kind {
            Kind::Bool => {
                let mut v = doc::get_bool(&self.doc, sec, key)
                    .unwrap_or(matches!(st.default, Def::Bool(true)));
                if ui.checkbox(&mut v, "").changed() {
                    doc::set(&mut self.doc, sec, key, v);
                }
            }
            Kind::Float { min, max } => {
                let mut v = doc::get_f64(&self.doc, sec, key).unwrap_or(num_def);
                let speed = ((max - min) / 400.0).clamp(0.01, 5.0);
                let r = ui.add(
                    egui::DragValue::new(&mut v)
                        .range(min..=max)
                        .speed(speed)
                        .max_decimals(3),
                );
                if r.changed() {
                    doc::set(&mut self.doc, sec, key, (v * 1000.0).round() / 1000.0);
                }
            }
            Kind::Int { min, max } => {
                let mut v = doc::get_f64(&self.doc, sec, key).unwrap_or(num_def) as i64;
                if ui
                    .add(egui::DragValue::new(&mut v).range(min..=max))
                    .changed()
                {
                    doc::set(&mut self.doc, sec, key, v);
                }
            }
            Kind::Choice(items) => {
                let cur = doc::get_str(&self.doc, sec, key).unwrap_or_else(|| str_def.into());
                let mut pick = cur.clone();
                egui::ComboBox::from_id_salt(&id)
                    .selected_text(&cur)
                    .show_ui(ui, |ui| {
                        for it in items {
                            ui.selectable_value(&mut pick, (*it).to_string(), *it);
                        }
                    });
                if pick != cur {
                    doc::set(&mut self.doc, sec, key, pick);
                }
            }
            Kind::Combo => {
                let cur = doc::get_str(&self.doc, sec, key).unwrap_or_else(|| str_def.into());
                let mut f = self
                    .combos
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| ComboFields::parse(&cur));
                let changed = f.ui(ui, false);
                if changed {
                    let s = f.compose();
                    match check_combo(&s, &mk, false) {
                        Ok(()) => {
                            self.errors.remove(&id);
                            doc::set(&mut self.doc, sec, key, s);
                        }
                        Err(e) => {
                            self.errors.insert(id.clone(), e);
                        }
                    }
                }
                if let Some(e) = self.errors.get(&id) {
                    ui.colored_label(RED, e);
                }
                self.combos.insert(id.clone(), f);
            }
            Kind::Action => {
                let cur = doc::get_str(&self.doc, sec, key).unwrap_or_default();
                let mut f = self
                    .actions
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| ActionFields::parse(&cur));
                if f.ui(ui, true, false) {
                    if f.name.is_empty() {
                        doc::remove(&mut self.doc, sec, key);
                    } else {
                        doc::set(&mut self.doc, sec, key, f.compose());
                    }
                }
                self.actions.insert(id.clone(), f);
            }
            Kind::List => {
                let cur = doc::get_list(&self.doc, sec, key).join(", ");
                let buf = self.texts.entry(id.clone()).or_insert(cur);
                if ui
                    .add(egui::TextEdit::singleline(buf).desired_width(260.0))
                    .changed()
                {
                    let items: Vec<String> = buf
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if items.is_empty() {
                        doc::remove(&mut self.doc, sec, key);
                    } else {
                        doc::set(&mut self.doc, sec, key, Array::from_iter(items));
                    }
                }
            }
        }
        if doc::get(&self.doc, sec, key).is_some()
            && ui
                .small_button("Сбросить")
                .on_hover_text("Убрать значение из файла — будет стандартное")
                .clicked()
        {
            doc::remove(&mut self.doc, sec, key);
            self.combos.remove(&id);
            self.actions.remove(&id);
            self.texts.remove(&id);
            self.errors.remove(&id);
        }
    }

    // ---------- привязки ----------

    fn section(mouse: bool) -> &'static str {
        if mouse {
            "mouse.anywhere"
        } else {
            "keybindings"
        }
    }

    fn defaults_disabled(&self, mouse: bool) -> bool {
        let name = if mouse { "mouse" } else { "keys" };
        doc::get_list(&self.doc, "bindings", "disable_defaults")
            .iter()
            .any(|d| d == name)
    }

    fn defaults(&self, mouse: bool) -> Vec<(String, String)> {
        let mk = self.mod_key();
        if mouse {
            catalog::default_mouse_bindings(&mk)
        } else {
            catalog::default_keys(&mk)
        }
    }

    fn rows(&self, mouse: bool) -> Vec<Row> {
        let mk = self.mod_key();
        let mut rows: Vec<Row> = vec![];
        if !self.defaults_disabled(mouse) {
            for (c, a) in self.defaults(mouse) {
                if let Some(id) = combo_id(&c, &mk, mouse) {
                    rows.push(Row {
                        id,
                        combo: c,
                        action: a,
                        kind: RowKind::Default,
                    });
                }
            }
        }
        for (c, a) in doc::pairs(&self.doc, Self::section(mouse)) {
            let Some(id) = combo_id(&c, &mk, mouse) else {
                continue;
            };
            let a = a.trim().to_string();
            let found = rows.iter().position(|r| r.id == id);
            if a == "none" {
                if let Some(i) = found {
                    rows[i].kind = RowKind::Disabled;
                }
            } else if let Some(i) = found {
                if rows[i].action != a {
                    rows[i].action = a;
                    rows[i].kind = RowKind::Overridden;
                }
            } else {
                rows.push(Row {
                    id,
                    combo: c,
                    action: a,
                    kind: RowKind::Custom,
                });
            }
        }
        rows
    }

    fn remove_by_id(&mut self, mouse: bool, id: &str) {
        let mk = self.mod_key();
        let sec = Self::section(mouse);
        for (c, _) in doc::pairs(&self.doc, sec) {
            if combo_id(&c, &mk, mouse).as_deref() == Some(id) {
                doc::remove(&mut self.doc, sec, &c);
            }
        }
    }

    fn default_for(&self, mouse: bool, id: &str) -> Option<(String, String)> {
        let mk = self.mod_key();
        if self.defaults_disabled(mouse) {
            return None;
        }
        self.defaults(mouse)
            .into_iter()
            .find(|(c, _)| combo_id(c, &mk, mouse).as_deref() == Some(id))
    }

    fn apply_edit(&mut self, e: &Edit) -> Result<(), String> {
        let mouse = e.mouse;
        let mk = self.mod_key();
        if e.action.name.is_empty() {
            return Err("выберите действие".into());
        }
        let combo = e.combo.compose();
        check_combo(&combo, &mk, mouse)?;
        let id = combo_id(&combo, &mk, mouse).ok_or("некорректная комбинация")?;
        let orig_id = e.orig.as_ref().map(|o| o.id.clone());
        if orig_id.as_deref() != Some(&id)
            && let Some(r) = self
                .rows(mouse)
                .into_iter()
                .find(|r| r.id == id && r.kind != RowKind::Disabled)
        {
            return Err(format!(
                "комбинация уже занята: {} (сначала измените или удалите ту привязку)",
                pretty_action(&r.action)
            ));
        }
        let sec = Self::section(mouse);
        let action = e.action.compose();
        if let Some(oid) = &orig_id {
            self.remove_by_id(mouse, oid);
            if *oid != id
                && let Some((dc, _)) = self.default_for(mouse, oid)
            {
                doc::set(&mut self.doc, sec, &dc, "none");
            }
        }
        self.remove_by_id(mouse, &id);
        let same_as_default = self
            .default_for(mouse, &id)
            .is_some_and(|(_, a)| a == action);
        if !same_as_default && !doc::set(&mut self.doc, sec, &combo, action) {
            return Err(format!(
                "секция [{sec}] в файле записана в нестандартном виде — правьте вручную"
            ));
        }
        Ok(())
    }

    fn do_row_op(&mut self, mouse: bool, op: RowOp) {
        let sec = Self::section(mouse);
        match op {
            RowOp::Edit(r) => {
                self.edit = Some(Edit {
                    mouse,
                    combo: ComboFields::parse(&r.combo),
                    action: ActionFields::parse(&r.action),
                    orig: Some(r),
                    error: None,
                });
            }
            RowOp::Delete(r) => {
                self.remove_by_id(mouse, &r.id);
                if let Some((dc, _)) = self.default_for(mouse, &r.id) {
                    doc::set(&mut self.doc, sec, &dc, "none");
                }
            }
            RowOp::Reset(r) | RowOp::Enable(r) => self.remove_by_id(mouse, &r.id),
        }
    }

    fn bindings_tab(&mut self, ui: &mut Ui, mouse: bool) {
        let mk = self.mod_key();
        ui.add_space(6.0);
        ui.label(if mouse {
            "Мышь: нужен Win или Alt. «mod» — выбранная в общих настройках клавиша-модификатор."
        } else {
            "«mod» — выбранная в общих настройках клавиша-модификатор. Нужен Win или пара Ctrl+Alt."
        });
        ui.horizontal(|ui| {
            if ui.button("Добавить привязку").clicked() {
                self.edit = Some(Edit {
                    mouse,
                    orig: None,
                    combo: if mouse {
                        ComboFields::parse("alt+left")
                    } else {
                        ComboFields::with_mod()
                    },
                    action: ActionFields::default(),
                    error: None,
                });
            }
            let mut off = self.defaults_disabled(mouse);
            let name = if mouse { "mouse" } else { "keys" };
            if ui
                .checkbox(&mut off, "Отключить все стандартные привязки")
                .changed()
            {
                let mut list = doc::get_list(&self.doc, "bindings", "disable_defaults");
                list.retain(|d| d != name);
                if off {
                    list.push(name.to_string());
                }
                if list.is_empty() {
                    doc::remove(&mut self.doc, "bindings", "disable_defaults");
                } else {
                    doc::set(
                        &mut self.doc,
                        "bindings",
                        "disable_defaults",
                        Array::from_iter(list),
                    );
                }
            }
        });
        ui.separator();
        let rows = self.rows(mouse);
        let mut op: Option<RowOp> = None;
        egui::Grid::new(format!("bind-{mouse}"))
            .num_columns(4)
            .striped(true)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                ui.strong("Комбинация");
                ui.strong("Действие");
                ui.strong("Источник");
                ui.label("");
                ui.end_row();
                for r in &rows {
                    let dim = r.kind == RowKind::Disabled;
                    let rich = |s: String| {
                        if dim {
                            RichText::new(s).weak().strikethrough()
                        } else {
                            RichText::new(s)
                        }
                    };
                    ui.label(rich(pretty_combo(&r.combo, &mk)));
                    ui.label(rich(pretty_action(&r.action)));
                    ui.label(match r.kind {
                        RowKind::Default => "стандартная",
                        RowKind::Overridden => "изменена",
                        RowKind::Custom => "своя",
                        RowKind::Disabled => "отключена",
                    });
                    ui.horizontal(|ui| {
                        if r.kind == RowKind::Disabled {
                            if ui.small_button("Включить").clicked() {
                                op = Some(RowOp::Enable(r.clone()));
                            }
                        } else {
                            if ui.small_button("Изменить").clicked() {
                                op = Some(RowOp::Edit(r.clone()));
                            }
                            if r.kind == RowKind::Overridden
                                && ui.small_button("Сбросить").clicked()
                            {
                                op = Some(RowOp::Reset(r.clone()));
                            }
                            let del = if r.kind == RowKind::Custom {
                                "Удалить"
                            } else {
                                "Отключить"
                            };
                            if ui.small_button(del).clicked() {
                                op = Some(RowOp::Delete(r.clone()));
                            }
                        }
                    });
                    ui.end_row();
                }
            });
        if let Some(op) = op {
            self.do_row_op(mouse, op);
        }
    }

    fn edit_window(&mut self, ctx: &egui::Context) {
        let Some(mut e) = self.edit.take() else {
            return;
        };
        let mut keep = true;
        let mut apply = false;
        egui::Window::new(if e.orig.is_some() {
            "Изменить привязку"
        } else {
            "Новая привязка"
        })
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label("Комбинация");
            e.combo.ui(ui, e.mouse);
            ui.add_space(6.0);
            ui.label("Действие");
            e.action.ui(ui, false, e.mouse);
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "Запись в файле: «{}» = «{}»",
                    e.combo.compose(),
                    e.action.compose()
                ))
                .weak(),
            );
            if let Some(err) = &e.error {
                ui.colored_label(RED, err);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Применить").clicked() {
                    apply = true;
                }
                if ui.button("Отмена").clicked() {
                    keep = false;
                }
            });
        });
        if apply {
            match self.apply_edit(&e) {
                Ok(()) => keep = false,
                Err(err) => e.error = Some(err),
            }
        }
        if keep {
            self.edit = Some(e);
        }
    }

    // ---------- прочее ----------

    fn other_tab(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        ui.label(
            "Правила окон, исключения и закладки пока правятся в самом файле. \
             Здесь они показаны для справки; комментарии и порядок ключей при сохранении не теряются.",
        );
        ui.horizontal(|ui| {
            if ui.button("Открыть config.toml в Блокноте").clicked() {
                let _ = std::process::Command::new("notepad.exe")
                    .arg(&self.path)
                    .spawn();
            }
            if ui.button("Показать в папке").clicked() {
                let _ = std::process::Command::new("explorer.exe")
                    .arg(format!("/select,{}", self.path.display()))
                    .spawn();
            }
        });
        ui.label(RichText::new(self.path.display().to_string()).weak());
        ui.add_space(10.0);
        ui.heading("Исключения [windows] exclude");
        let ex = doc::get(&self.doc, "windows", "exclude")
            .map(doc::brief)
            .unwrap_or_else(|| "по умолчанию".into());
        ui.label(ex);
        ui.add_space(10.0);
        ui.heading("Закладки [navigation.bookmarks]");
        let bm = doc::get(&self.doc, "navigation", "bookmarks")
            .map(doc::brief)
            .unwrap_or_else(|| "нет".into());
        ui.label(bm);
        ui.add_space(10.0);
        ui.heading("Правила окон [[window_rules]]");
        let rules = doc::rules_text(&self.doc);
        if rules.is_empty() {
            ui.label("нет");
        }
        for r in rules {
            ui.label(r);
        }
    }
}

fn pretty_combo(combo: &str, mk: &str) -> String {
    combo
        .split('+')
        .map(|p| match p.trim().to_lowercase().as_str() {
            "mod" => if mk == "alt" {
                "Alt (mod)"
            } else {
                "Win (mod)"
            }
            .to_string(),
            "win" | "super" | "meta" => "Win".into(),
            "ctrl" | "control" => "Ctrl".into(),
            "alt" => "Alt".into(),
            "shift" => "Shift".into(),
            "left" => "←/ЛКМ".into(),
            "right" => "→/ПКМ".into(),
            "middle" => "СКМ".into(),
            "up" => "↑".into(),
            "down" => "↓".into(),
            "lbracket" => "[".into(),
            "rbracket" => "]".into(),
            "minus" => "-".into(),
            "equals" => "=".into(),
            o => o.to_uppercase(),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

fn pretty_action(action: &str) -> String {
    let mut it = action.split_whitespace();
    let name = it.next().unwrap_or("");
    let arg = it.next();
    let label = ACTIONS
        .iter()
        .find(|a| a.name == name)
        .map(|a| a.label)
        .or_else(|| {
            catalog::MOUSE_DRAGS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, l)| *l)
        })
        .unwrap_or(name);
    match arg {
        Some(a) => {
            let a = match a {
                "up" => "вверх",
                "down" => "вниз",
                "left" => "влево",
                "right" => "вправо",
                "forward" => "вперёд",
                "backward" => "назад",
                o => o,
            };
            format!("{label}: {a}")
        }
        None => label.to_string(),
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::General, "Общие");
                ui.selectable_value(&mut self.tab, Tab::Keys, "Клавиши");
                ui.selectable_value(&mut self.tab, Tab::Mouse, "Мышь");
                ui.selectable_value(&mut self.tab, Tab::Other, "Прочее");
            });
        });
        egui::TopBottomPanel::bottom("bar").show(ctx, |ui| {
            ui.add_space(4.0);
            for (c, m) in &self.status {
                ui.colored_label(*c, m);
            }
            ui.horizontal(|ui| {
                let dirty = self.dirty();
                if ui
                    .add_enabled(dirty, egui::Button::new("Сохранить"))
                    .clicked()
                {
                    self.save();
                }
                if ui
                    .add_enabled(dirty, egui::Button::new("Отменить изменения"))
                    .clicked()
                {
                    self.reload();
                }
                if dirty {
                    ui.colored_label(ORANGE, "есть несохранённые изменения");
                }
            });
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match self.tab {
                    Tab::General => self.general_tab(ui),
                    Tab::Keys => self.bindings_tab(ui, false),
                    Tab::Mouse => self.bindings_tab(ui, true),
                    Tab::Other => self.other_tab(ui),
                });
        });
        self.edit_window(ctx);
    }
}

fn main() -> eframe::Result {
    let path = config_path();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DriftWin — настройки")
            .with_inner_size([860.0, 660.0])
            .with_min_inner_size([620.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DriftWin — настройки",
        opts,
        Box::new(move |_| Ok(Box::new(App::new(path)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(src: &str) -> App {
        let mut a = App::new(PathBuf::from("nonexistent-test.toml"));
        a.doc = src.parse().unwrap();
        a.saved_text = src.into();
        a
    }

    fn edit(mouse: bool, orig: Option<Row>, combo: &str, action: &str) -> Edit {
        Edit {
            mouse,
            orig,
            combo: ComboFields::parse(combo),
            action: ActionFields::parse(action),
            error: None,
        }
    }

    fn row(a: &App, combo: &str) -> Row {
        let id = combo_id(combo, "super", false).unwrap();
        a.rows(false).into_iter().find(|r| r.id == id).unwrap()
    }

    #[test]
    fn rebind_default_disables_old_and_adds_new() {
        let mut a = app("# заметка\nmod_key = \"super\" # клавиша\n");
        let old = row(&a, "mod+f");
        a.apply_edit(&edit(false, Some(old), "mod+ctrl+f", "toggle-fullscreen"))
            .unwrap();
        let text = a.doc.to_string();
        assert!(text.contains("# заметка") && text.contains("# клавиша"));
        assert_eq!(row(&a, "mod+f").kind, RowKind::Disabled);
        assert_eq!(row(&a, "mod+ctrl+f").kind, RowKind::Custom);
        dw_config::parse(&text).unwrap();
    }

    #[test]
    fn override_and_reset_restore_default() {
        let mut a = app("");
        let r = row(&a, "mod+f");
        a.apply_edit(&edit(false, Some(r), "mod+f", "fit-window"))
            .unwrap();
        let r = row(&a, "mod+f");
        assert_eq!(
            (r.kind, r.action.as_str()),
            (RowKind::Overridden, "fit-window")
        );
        a.do_row_op(false, RowOp::Reset(r));
        assert_eq!(row(&a, "mod+f").kind, RowKind::Default);
        assert!(!a.dirty());
    }

    #[test]
    fn busy_combo_is_rejected_and_delete_disables() {
        let mut a = app("");
        let e = edit(false, None, "mod+f", "fit-window");
        assert!(a.apply_edit(&e).unwrap_err().contains("занята"));
        let r = row(&a, "mod+f");
        a.do_row_op(false, RowOp::Delete(r));
        assert_eq!(row(&a, "mod+f").kind, RowKind::Disabled);
        a.apply_edit(&edit(false, None, "mod+ctrl+q", "quit"))
            .unwrap();
        let custom = row(&a, "mod+ctrl+q");
        a.do_row_op(false, RowOp::Delete(custom.clone()));
        assert!(a.rows(false).iter().all(|r| r.id != custom.id));
    }

    #[test]
    fn invalid_combo_and_mouse_binding() {
        let mut a = app("");
        assert!(a.apply_edit(&edit(false, None, "ctrl+x", "quit")).is_err());
        a.apply_edit(&edit(true, None, "win+right", "move-window"))
            .unwrap();
        let t = a.doc.to_string();
        assert!(t.contains("[mouse.anywhere]"), "{t}");
        dw_config::parse(&t).unwrap();
    }

    #[test]
    fn setting_values_keep_comments_and_validate() {
        let mut a = app("[snap]\ngap = 12 # зазор\n");
        doc::set(&mut a.doc, "snap", "gap", 20.0);
        assert!(a.doc.to_string().contains("gap = 20.0 # зазор"));
        doc::set(&mut a.doc, "navigation.edge_pan", "zone", 150.0);
        dw_config::parse(&a.doc.to_string()).unwrap();
        doc::remove(&mut a.doc, "snap", "gap");
        assert!(doc::get(&a.doc, "snap", "gap").is_none());
    }
}
