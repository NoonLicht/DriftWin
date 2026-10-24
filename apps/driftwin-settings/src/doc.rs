//! Правка config.toml через toml_edit: комментарии и порядок ключей сохраняются.

use toml_edit::{DocumentMut, Item, Table, TableLike, Value};

fn walk<'a>(doc: &'a DocumentMut, section: &str) -> Option<&'a dyn TableLike> {
    let mut t: &dyn TableLike = doc.as_table();
    if section.is_empty() {
        return Some(t);
    }
    for part in section.split('.') {
        t = t.get(part)?.as_table_like()?;
    }
    Some(t)
}

/// Таблица секции; недостающие создаются. `None` — на пути лежит не таблица.
pub fn table_mut<'a>(doc: &'a mut DocumentMut, section: &str) -> Option<&'a mut dyn TableLike> {
    let mut t: &mut dyn TableLike = doc.as_table_mut();
    if section.is_empty() {
        return Some(t);
    }
    for part in section.split('.') {
        if !t.contains_key(part) {
            let mut n = Table::new();
            n.set_implicit(true);
            t.insert(part, Item::Table(n));
        }
        t = t.get_mut(part)?.as_table_like_mut()?;
    }
    Some(t)
}

pub fn get<'a>(doc: &'a DocumentMut, section: &str, key: &str) -> Option<&'a Item> {
    walk(doc, section)?.get(key)
}

pub fn get_bool(doc: &DocumentMut, section: &str, key: &str) -> Option<bool> {
    get(doc, section, key)?.as_value()?.as_bool()
}

pub fn get_f64(doc: &DocumentMut, section: &str, key: &str) -> Option<f64> {
    let v = get(doc, section, key)?.as_value()?;
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

pub fn get_str(doc: &DocumentMut, section: &str, key: &str) -> Option<String> {
    get(doc, section, key)?
        .as_value()?
        .as_str()
        .map(str::to_string)
}

pub fn get_list(doc: &DocumentMut, section: &str, key: &str) -> Vec<String> {
    get(doc, section, key)
        .and_then(Item::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Записать значение, сохранив комментарий в конце строки.
pub fn set(doc: &mut DocumentMut, section: &str, key: &str, v: impl Into<Value>) -> bool {
    let Some(t) = table_mut(doc, section) else {
        return false;
    };
    let mut nv: Value = v.into();
    if let Some(old) = t.get(key).and_then(Item::as_value) {
        *nv.decor_mut() = old.decor().clone();
    }
    t.insert(key, Item::Value(nv));
    true
}

pub fn remove(doc: &mut DocumentMut, section: &str, key: &str) {
    if let Some(t) = table_mut(doc, section) {
        t.remove(key);
    }
}

/// Пары «ключ → строка» таблицы (например, `[keybindings]`).
pub fn pairs(doc: &DocumentMut, section: &str) -> Vec<(String, String)> {
    walk(doc, section)
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| Some((k.to_string(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Человекочитаемое описание значения для обзора («Прочее»).
pub fn brief(item: &Item) -> String {
    item.to_string().trim().replace('\n', " ")
}

pub fn rules_text(doc: &DocumentMut) -> Vec<String> {
    doc.get("window_rules")
        .and_then(Item::as_array_of_tables)
        .map(|a| a.iter().map(|t| brief(&Item::Table(t.clone()))).collect())
        .unwrap_or_default()
}
