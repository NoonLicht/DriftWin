//! Файл сессии `session.json`: окна и заглушки, камера, закладки.
//! Версия формата, миграция со старых версий, карантин повреждённого файла, атомарная запись.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Текущая версия формата.
pub const VERSION: u32 = 1;

/// Одно окно или заглушка на холсте. Координаты — мир, Y вверх, центр и размер видимого кадра.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Имя exe (как в `app_id`).
    pub app: String,
    pub title: String,
    /// Полный путь к exe для запуска заглушки; пусто — запускать нечем.
    #[serde(default)]
    pub path: String,
    /// Командная строка процесса целиком (если удалось прочитать).
    #[serde(default)]
    pub cmdline: String,
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    /// Окно было заглушкой.
    #[serde(default)]
    pub suspended: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    /// Секунды Unix на момент записи.
    #[serde(default)]
    pub saved_at: u64,
    #[serde(default)]
    pub camera: Option<(f64, f64)>,
    #[serde(default)]
    pub bookmarks: BTreeMap<String, (f64, f64)>,
    #[serde(default)]
    pub windows: Vec<Entry>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            version: VERSION,
            saved_at: 0,
            camera: None,
            bookmarks: BTreeMap::new(),
            windows: Vec::new(),
        }
    }
}

/// Приводит JSON старой версии к текущей. `Err` — версия неизвестна или структура непонятна.
pub fn migrate(mut v: serde_json::Value) -> Result<serde_json::Value, String> {
    let obj = v.as_object_mut().ok_or("корень не объект")?;
    let ver = match obj.get("version") {
        None => 0,
        Some(x) => x.as_u64().ok_or("version не число")? as u32,
    };
    if ver > VERSION {
        return Err(format!(
            "файл создан более новой версией (формат {ver}, поддерживается {VERSION})"
        ));
    }
    if ver == 0 {
        // v0: у окон поле `exe` вместо `app`, прямоугольник `rect` = [cx, cy, w, h]
        if let Some(ws) = obj.get_mut("windows").and_then(|w| w.as_array_mut()) {
            for w in ws {
                let Some(o) = w.as_object_mut() else {
                    return Err("запись окна не объект".into());
                };
                if let Some(e) = o.remove("exe") {
                    o.insert("app".into(), e);
                }
                if let Some(r) = o.remove("rect").and_then(|r| r.as_array().cloned())
                    && r.len() == 4
                {
                    for (k, val) in ["cx", "cy", "w", "h"].iter().zip(r) {
                        o.insert((*k).into(), val);
                    }
                }
                o.entry("title").or_insert_with(|| "".into());
            }
        }
        obj.insert("version".into(), VERSION.into());
    }
    Ok(v)
}

pub fn parse(text: &str) -> Result<Session, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let v = migrate(v)?;
    let mut s: Session = serde_json::from_value(v).map_err(|e| e.to_string())?;
    // отбрасываем записи с ненормальными числами: они дали бы окна в бесконечности
    s.windows.retain(|w| {
        [w.cx, w.cy, w.w, w.h].iter().all(|n| n.is_finite())
            && w.w >= 1.0
            && w.h >= 1.0
            && w.cx.abs() < 1e7
            && w.cy.abs() < 1e7
    });
    s.windows.truncate(2000);
    Ok(s)
}

pub fn to_json(s: &Session) -> String {
    serde_json::to_string_pretty(s).unwrap_or_default()
}

/// Результат чтения файла сессии.
#[derive(Debug)]
pub enum Loaded {
    /// Файла нет.
    Missing,
    Ok(Session),
    /// Файл повреждён и перенесён в `quarantine`; старт с пустой сессией.
    Corrupt {
        reason: String,
        quarantine: PathBuf,
    },
}

/// Прочитать сессию. Повреждённый файл переименовывается в `session.json.corrupt.<unix-секунды>`.
pub fn load(path: &Path, now_unix: u64) -> Loaded {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Missing,
        Err(e) => return quarantine(path, now_unix, e.to_string()),
    };
    // BOM и пробелы по краям — не повод считать файл битым
    let text = String::from_utf8_lossy(&bytes);
    match parse(text.trim_start_matches('\u{feff}').trim()) {
        Ok(s) => Loaded::Ok(s),
        Err(e) => quarantine(path, now_unix, e),
    }
}

fn quarantine(path: &Path, now_unix: u64, reason: String) -> Loaded {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".corrupt.{now_unix}"));
    let dest = path.with_file_name(name);
    let _ = std::fs::rename(path, &dest);
    Loaded::Corrupt {
        reason,
        quarantine: dest,
    }
}

/// Записать атомарно: во временный файл рядом и переименование поверх старого.
pub fn save(path: &Path, s: &Session) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, to_json(s))?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Session {
        Session {
            saved_at: 5,
            camera: Some((10.0, -4.0)),
            bookmarks: [("1".to_string(), (1.0, 2.0))].into(),
            windows: vec![Entry {
                app: "notepad.exe".into(),
                title: "a".into(),
                path: r"C:\w\notepad.exe".into(),
                cmdline: String::new(),
                cx: 1.0,
                cy: 2.0,
                w: 300.0,
                h: 200.0,
                suspended: true,
            }],
            ..Default::default()
        }
    }

    fn tmpdir(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dw-session-test-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn roundtrip() {
        let s = sample();
        assert_eq!(parse(&to_json(&s)).unwrap(), s);
    }

    #[test]
    fn migrates_v0() {
        let old = r#"{"windows":[{"exe":"a.exe","rect":[1,2,300,200],"suspended":true}]}"#;
        let s = parse(old).unwrap();
        assert_eq!(s.version, VERSION);
        assert_eq!(s.windows[0].app, "a.exe");
        assert_eq!((s.windows[0].cx, s.windows[0].w), (1.0, 300.0));
        assert!(s.windows[0].suspended);
    }

    #[test]
    fn newer_version_is_rejected() {
        assert!(parse(r#"{"version":99,"windows":[]}"#).is_err());
    }

    #[test]
    fn junk_entries_are_dropped() {
        let t = r#"{"version":1,"windows":[
          {"app":"a","title":"","cx":0,"cy":0,"w":0,"h":10},
          {"app":"b","title":"","cx":1e30,"cy":0,"w":10,"h":10},
          {"app":"c","title":"","cx":0,"cy":0,"w":10,"h":10}]}"#;
        let s = parse(t).unwrap();
        assert_eq!(s.windows.len(), 1);
        assert_eq!(s.windows[0].app, "c");
    }

    #[test]
    fn corrupt_file_is_quarantined() {
        let d = tmpdir("corrupt");
        let p = d.join("session.json");
        std::fs::write(&p, "{ не json").unwrap();
        match load(&p, 123) {
            Loaded::Corrupt { quarantine, .. } => {
                assert_eq!(quarantine, d.join("session.json.corrupt.123"));
                assert!(quarantine.exists());
                assert!(!p.exists());
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(load(&p, 124), Loaded::Missing));
    }

    #[test]
    fn save_then_load() {
        let d = tmpdir("save");
        let p = d.join("session.json");
        save(&p, &sample()).unwrap();
        save(&p, &sample()).unwrap(); // повторная запись поверх существующего
        assert!(matches!(load(&p, 1), Loaded::Ok(s) if s == sample()));
        assert!(!d.join("session.json.tmp").exists());
    }
}
