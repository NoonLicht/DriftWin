//! Разбор описаний комбинаций клавиш («mod+ctrl+shift+right») в виртуальные коды.

use dw_core::action::Mods;

use crate::ModKey;

fn key_vk(name: &str) -> Option<u16> {
    let n = name.to_lowercase();
    let b = n.as_bytes();
    if b.len() == 1 {
        return match b[0] {
            c @ b'a'..=b'z' => Some(c.to_ascii_uppercase() as u16),
            c @ b'0'..=b'9' => Some(c as u16),
            _ => None,
        };
    }
    if let Some(num) = n.strip_prefix('f')
        && let Ok(k) = num.parse::<u16>()
        && (1..=24).contains(&k)
    {
        return Some(0x6F + k);
    }
    Some(match n.as_str() {
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "tab" => 0x09,
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "escape" | "esc" => 0x1B,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        _ => return None,
    })
}

fn split(s: &str, m: ModKey) -> Result<(Mods, Option<&str>), String> {
    let mut mods = Mods::default();
    let mut key = None;
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err("пустая часть комбинации".into());
    }
    for p in parts {
        match p.to_lowercase().as_str() {
            "mod" => match m {
                ModKey::Super => mods.win = true,
                ModKey::Alt => mods.alt = true,
            },
            "super" | "win" | "meta" => mods.win = true,
            "ctrl" | "control" => mods.ctrl = true,
            "shift" => mods.shift = true,
            "alt" => mods.alt = true,
            _ => {
                if key.replace(p).is_some() {
                    return Err("в комбинации больше одной обычной клавиши".into());
                }
            }
        }
    }
    Ok((mods, key))
}

/// Без Win и без пары Ctrl+Alt комбинация перехватила бы привычные сочетания приложений.
fn check_safe(mods: Mods) -> Result<(), String> {
    if mods.win || (mods.ctrl && mods.alt) {
        Ok(())
    } else {
        Err("нужен Win или одновременно Ctrl и Alt (иначе перехватываются стандартные сочетания приложений)".into())
    }
}

pub fn parse_combo(s: &str, m: ModKey) -> Result<(Mods, u16), String> {
    let (mods, key) = split(s, m)?;
    let key = key.ok_or("не указана клавиша")?;
    let vk = key_vk(key).ok_or_else(|| format!("неизвестная клавиша «{key}»"))?;
    check_safe(mods)?;
    if mods.ctrl && mods.alt && vk == 0x2E {
        return Err("Ctrl+Alt+Delete зарезервировано системой".into());
    }
    if mods.win && !mods.ctrl && !mods.alt && !mods.shift && vk == 0x4C {
        return Err("Win+L зарезервировано системой".into());
    }
    Ok((mods, vk))
}

/// Мышиная привязка: модификаторы и кнопка (`left`).
pub fn parse_mouse_combo(s: &str, m: ModKey) -> Result<Mods, String> {
    let (mods, key) = split(s, m)?;
    match key {
        Some("left") => {}
        Some(other) => {
            return Err(format!(
                "кнопка «{other}» пока не поддерживается (только left)"
            ));
        }
        None => return Err("не указана кнопка мыши".into()),
    }
    check_safe(mods)?;
    Ok(mods)
}

/// Строка и столбец (с 1) первого вхождения `needle`.
pub fn locate(src: &str, needle: &str) -> Option<(usize, usize)> {
    let pos = src.find(needle)?;
    let before = &src[..pos];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    Some((line, col))
}
