//! Оформление окон средствами DWM (Windows 11): скругление углов, цвет рамки, подложка, прозрачность.
//! Задаётся глобально в `[decorations]` и переопределяется в `[[window_rules]]`.

use serde::Deserialize;

/// Скругление углов (`DWMWA_WINDOW_CORNER_PREFERENCE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    /// Как решит система.
    Default = 0,
    Square = 1,
    Round = 2,
    Small = 3,
}

/// Цвет рамки окна (`DWMWA_BORDER_COLOR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderColor {
    /// Без рамки.
    None,
    Rgb([u8; 3]),
}

/// Подложка окна (`DWMWA_SYSTEMBACKDROP_TYPE`); видна только там, где приложение растягивает рамку на клиентскую область.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    None = 1,
    Mica = 2,
    Acrylic = 3,
    Tabbed = 4,
}

/// Желаемое оформление; `None` в поле — «не менять».
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Deco {
    pub corner: Option<Corner>,
    pub border: Option<BorderColor>,
    pub backdrop: Option<Backdrop>,
    /// 0.1..1.0; 1.0 — как есть.
    pub opacity: Option<f64>,
}

impl Deco {
    /// Поля `self` важнее полей `base`.
    pub fn over(self, base: Deco) -> Deco {
        Deco {
            corner: self.corner.or(base.corner),
            border: self.border.or(base.border),
            backdrop: self.backdrop.or(base.backdrop),
            opacity: self.opacity.or(base.opacity),
        }
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub(crate) struct RawDeco {
    pub corner: Option<String>,
    pub border_color: Option<String>,
    pub backdrop: Option<String>,
    pub opacity: Option<f64>,
}

fn color(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().strip_prefix('#')?;
    if !h.is_ascii() {
        return None;
    }
    let v = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok();
    match h.len() {
        6 => Some([v(0, 2)?, v(2, 2)?, v(4, 2)?]),
        3 => Some([v(0, 1)? * 17, v(1, 1)? * 17, v(2, 1)? * 17]),
        _ => None,
    }
}

/// Разбор; `Err` — текст ошибки для первого неверного поля.
pub(crate) fn compile(r: &RawDeco) -> Result<Deco, String> {
    let corner = match r.corner.as_deref().map(str::trim) {
        None | Some("" | "keep") => None,
        Some("default") => Some(Corner::Default),
        Some("square") => Some(Corner::Square),
        Some("round") => Some(Corner::Round),
        Some("small") => Some(Corner::Small),
        Some(o) => {
            return Err(format!(
                "corner: «{o}» — ожидалось keep, default, square, round или small"
            ));
        }
    };
    let border = match r.border_color.as_deref().map(str::trim) {
        None | Some("" | "keep") => None,
        Some("none") => Some(BorderColor::None),
        Some(c) => Some(BorderColor::Rgb(color(c).ok_or_else(|| {
            format!("border_color: «{c}» — ожидалось #rrggbb, none или keep")
        })?)),
    };
    let backdrop = match r.backdrop.as_deref().map(str::trim) {
        None | Some("" | "keep") => None,
        Some("none") => Some(Backdrop::None),
        Some("mica") => Some(Backdrop::Mica),
        Some("acrylic") => Some(Backdrop::Acrylic),
        Some("tabbed") => Some(Backdrop::Tabbed),
        Some(o) => {
            return Err(format!(
                "backdrop: «{o}» — ожидалось keep, none, mica, acrylic или tabbed"
            ));
        }
    };
    let opacity = match r.opacity {
        None => None,
        Some(o) if (0.1..=1.0).contains(&o) => (o < 1.0).then_some(o),
        Some(o) => return Err(format!("opacity: {o} вне диапазона 0.1..1.0")),
    };
    Ok(Deco {
        corner,
        border,
        backdrop,
        opacity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(c: Option<&str>, b: Option<&str>, bd: Option<&str>, o: Option<f64>) -> RawDeco {
        RawDeco {
            corner: c.map(Into::into),
            border_color: b.map(Into::into),
            backdrop: bd.map(Into::into),
            opacity: o,
        }
    }

    #[test]
    fn parses_all_fields() {
        let d = compile(&raw(Some("round"), Some("#f80"), Some("mica"), Some(0.8))).unwrap();
        assert_eq!(d.corner, Some(Corner::Round));
        assert_eq!(d.border, Some(BorderColor::Rgb([255, 136, 0])));
        assert_eq!(d.backdrop, Some(Backdrop::Mica));
        assert_eq!(d.opacity, Some(0.8));
        assert_eq!(
            compile(&raw(Some("keep"), Some("none"), Some(""), Some(1.0))).unwrap(),
            Deco {
                border: Some(BorderColor::None),
                ..Deco::default()
            }
        );
    }

    #[test]
    fn rejects_bad_values() {
        assert!(compile(&raw(Some("pointy"), None, None, None)).is_err());
        assert!(compile(&raw(None, Some("red"), None, None)).is_err());
        assert!(compile(&raw(None, None, Some("glass"), None)).is_err());
        assert!(compile(&raw(None, None, None, Some(0.0))).is_err());
        assert!(compile(&raw(None, None, None, Some(1.5))).is_err());
    }

    #[test]
    fn rule_overrides_global() {
        let g = Deco {
            corner: Some(Corner::Square),
            opacity: Some(0.9),
            ..Deco::default()
        };
        let r = Deco {
            corner: Some(Corner::Round),
            ..Deco::default()
        };
        let m = r.over(g);
        assert_eq!(m.corner, Some(Corner::Round));
        assert_eq!(m.opacity, Some(0.9));
    }
}
