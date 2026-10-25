//! Рисование фона и подложек окон средствами GDI.

use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::*;

use crate::pyramid::Pyramid;
use crate::{BgCfg, BgKind, Kind, Shared, offset, screen_rect};

/// Картинка фона в памяти GDI.
pub struct Image {
    dc: HDC,
    bmp: HBITMAP,
    pub w: i32,
    pub h: i32,
    /// Средний цвет — для случаев, когда плитка слишком мелкая.
    pub avg: [u8; 3],
}

// HDC/HBITMAP живут только в потоке окна; структура лежит под мьютексом в STATE
unsafe impl Send for Image {}

impl Drop for Image {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.dc);
            let _ = DeleteObject(self.bmp.into());
        }
    }
}

/// Сколько пикселей картинки фона держим в памяти (RGBA: 40 Мпикс — 160 МБ). Большие (панорамы, гигапиксельные снимки)
/// уменьшаются при загрузке с сохранением пропорций.
const MAX_BG_PIXELS: u64 = 40_000_000;

/// Размер, вписанный в `max` пикселей с теми же пропорциями; не больше исходного.
fn fit_pixels(w: u32, h: u32, max: u64) -> (u32, u32) {
    let n = u64::from(w) * u64::from(h);
    if n <= max {
        return (w, h);
    }
    let k = (max as f64 / n as f64).sqrt();
    let (nw, nh) = (
        ((f64::from(w) * k).floor() as u32).max(1),
        ((f64::from(h) * k).floor() as u32).max(1),
    );
    // у очень вытянутых картинок сторона в 1 px раздувает вторую: дожимаем
    if u64::from(nw) * u64::from(nh) > max {
        return if nw == 1 {
            (1, (max as u32).max(1))
        } else {
            ((max as u32).max(1), 1)
        };
    }
    (nw, nh)
}

/// Что рисовать на фоне: картинка целиком или TIFF-пирамида (для огромных файлов).
pub fn load_bg(bg: &BgCfg) -> (Option<Image>, Option<Pyramid>) {
    if bg.kind == BgKind::Wallpaper
        && let Some(path) = &bg.image
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"))
    {
        match Pyramid::open(path) {
            Ok(p) => {
                let (w, h) = p.size();
                // обычный маленький TIFF проще загрузить целиком
                if p.levels.len() > 1 || u64::from(w) * u64::from(h) > MAX_BG_PIXELS {
                    tracing::info!(
                        "фон: TIFF {w}x{h}, уровней {} — плитки читаются по мере надобности",
                        p.levels.len()
                    );
                    return (None, Some(p));
                }
            }
            Err(e) => tracing::debug!("фон: {} не пирамида ({e})", path.display()),
        }
    }
    (load_image(bg), None)
}

pub fn load_image(bg: &BgCfg) -> Option<Image> {
    if !matches!(bg.kind, BgKind::Tile | BgKind::Wallpaper) {
        return None;
    }
    let path = bg.image.as_ref()?;
    let img = match image::open(path) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            tracing::warn!("фон: не удалось открыть {}: {e}", path.display());
            return None;
        }
    };
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    let (fw, fh) = fit_pixels(img.width(), img.height(), MAX_BG_PIXELS);
    let img = if (fw, fh) != (img.width(), img.height()) {
        tracing::info!(
            "фон: {}x{} велико, уменьшено до {fw}x{fh}",
            img.width(),
            img.height()
        );
        image::imageops::resize(&img, fw, fh, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let (w, h) = (img.width() as i32, img.height() as i32);
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
        ReleaseDC(None, screen);
        let bmp = bmp.ok()?;
        if bits.is_null() {
            return None;
        }
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
        let mut sum = [0u64; 3];
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(img.pixels()) {
            let [r, g, b, _] = s.0;
            *d = [b, g, r, 255];
            sum[0] += u64::from(r);
            sum[1] += u64::from(g);
            sum[2] += u64::from(b);
        }
        let n = (w * h) as u64;
        SelectObject(dc, bmp.into());
        Some(Image {
            dc,
            bmp,
            w,
            h,
            avg: [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8],
        })
    }
}

fn rgb(c: [u8; 3]) -> COLORREF {
    COLORREF(u32::from(c[0]) | u32::from(c[1]) << 8 | u32::from(c[2]) << 16)
}

fn fill(hdc: HDC, r: &RECT, c: [u8; 3]) {
    unsafe {
        let b = CreateSolidBrush(rgb(c));
        FillRect(hdc, r, b);
        let _ = DeleteObject(b.into());
    }
}

fn mix(a: [u8; 3], b: [u8; 3], t: f64) -> [u8; 3] {
    let m = |x: u8, y: u8| (f64::from(x) + (f64::from(y) - f64::from(x)) * t).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

/// Буфер кадра, живущий между перерисовками.
pub struct Back {
    dc: HDC,
    bmp: HBITMAP,
    size: (i32, i32),
}

// HDC/HBITMAP принадлежат потоку окна; структура лежит под мьютексом
unsafe impl Send for Back {}

impl Drop for Back {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

pub fn paint(hdc: HDC, s: &mut Shared) {
    let (w, h) = s.size;
    if s.back.as_ref().is_none_or(|b| b.size != (w, h)) {
        unsafe {
            let dc = CreateCompatibleDC(Some(hdc));
            let bmp = CreateCompatibleBitmap(hdc, w, h);
            SelectObject(dc, bmp.into());
            s.back = Some(Back {
                dc,
                bmp,
                size: (w, h),
            });
        }
    }
    let mem = s.back.as_ref().map(|b| b.dc).expect("буфер кадра");
    paint_bg(mem, s);
    if s.kind == Kind::Overview {
        paint_items(mem, s);
    }
    unsafe {
        let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
    }
}

fn paint_bg(hdc: HDC, s: &mut Shared) {
    let (w, h) = s.size;
    let full = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    let (cx, cy, z) = (s.scene.center.x, s.scene.center.y, s.scene.zoom);
    // начало координат мира на экране
    let (offx, offy) = offset(s.pos, &s.scene);
    let ox = -cx * z + f64::from(s.scene.view.0) / 2.0 - f64::from(offx);
    let oy = cy * z + f64::from(s.scene.view.1) / 2.0 - f64::from(offy);
    fill(hdc, &full, s.bg.color);
    match s.bg.kind {
        BgKind::None | BgKind::Solid => {}
        BgKind::Grid => paint_grid(hdc, s, ox, oy, z),
        // фрактал считается только шейдером (режим capture); здесь запасной aurora
        BgKind::Aurora | BgKind::Fractal => {
            let (a, hh) = aurora_origin(&s.scene, (offx, offy), (ox, oy));
            paint_aurora(hdc, &mut s.aurora, s.bg.color, (w, h), a, hh);
        }
        BgKind::Tile => {
            if let Some(img) = &s.image {
                paint_tile(hdc, img, s, ox, oy, z);
            }
        }
        BgKind::Wallpaper => {
            if let Some(p) = s.pyramid.as_mut() {
                paint_pyramid(hdc, p, (w, h), (ox, oy), z, s.bg.color);
            } else if let Some(img) = &s.image {
                let (iw, ih) = (f64::from(img.w) * z, f64::from(img.h) * z);
                unsafe {
                    SetStretchBltMode(hdc, HALFTONE);
                    let _ = StretchBlt(
                        hdc,
                        (ox - iw / 2.0).round() as i32,
                        (oy - ih / 2.0).round() as i32,
                        iw.round() as i32,
                        ih.round() as i32,
                        Some(img.dc),
                        0,
                        0,
                        img.w,
                        img.h,
                        SRCCOPY,
                    );
                }
            }
        }
    }
}

/// Мягкие цветные пятна фона `aurora`: положение (в высотах вида, Y вниз), радиус (тоже в высотах), цвет, густота в центре.
/// Те же числа продублированы в шейдере `gpu.rs`.
pub(crate) const GLOWS: [(f64, f64, f64, [u8; 3], f64); 3] = [
    (-0.55, -0.25, 0.85, [77, 89, 242], 0.32),
    (0.55, 0.25, 0.75, [26, 166, 179], 0.24),
    (0.0, 0.6, 0.65, [191, 77, 179], 0.18),
];
/// Пятна повторяются с этим периодом (в высотах вида), чтобы фон не кончался при панорамировании.
pub(crate) const AURORA_PERIOD: (f64, f64) = (2.6, 1.7);
/// Во сколько раз фон смещается медленнее холста (глубина).
pub(crate) const AURORA_PARALLAX: f64 = 0.25;

/// Точка отсчёта пятен на оверлее (с параллаксом) и высота вида — единица длины для пятен.
pub(crate) fn aurora_origin(
    scene: &crate::Scene,
    (offx, offy): (i32, i32),
    (ox, oy): (f64, f64),
) -> ((f64, f64), f64) {
    let cx = f64::from(scene.view.0) / 2.0 - f64::from(offx);
    let cy = f64::from(scene.view.1) / 2.0 - f64::from(offy);
    (
        (
            cx + (ox - cx) * AURORA_PARALLAX,
            cy + (oy - cy) * AURORA_PARALLAX,
        ),
        f64::from(scene.view.1.max(1)),
    )
}

/// Спрайт одного пятна: предумноженный BGRA, плавный спад `(1 - t²)²` до нуля на краю.
fn glow_sprite(rgb: [u8; 3], a: f64) -> Option<Image> {
    const N: i32 = 128;
    let mut px = vec![0u8; (N * N * 4) as usize];
    for (i, d) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (u, v) = (
            ((i as i32 % N) as f64 + 0.5) / f64::from(N) * 2.0 - 1.0,
            ((i as i32 / N) as f64 + 0.5) / f64::from(N) * 2.0 - 1.0,
        );
        let t2 = (u * u + v * v).min(1.0);
        let al = a * (1.0 - t2) * (1.0 - t2);
        let m = |c: u8| (f64::from(c) * al).round() as u8;
        *d = [m(rgb[2]), m(rgb[1]), m(rgb[0]), (al * 255.0).round() as u8];
    }
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: N,
                biHeight: -N,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
        ReleaseDC(None, screen);
        let bmp = bmp.ok()?;
        if bits.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits.cast::<u8>(), px.len());
        SelectObject(dc, bmp.into());
        Some(Image {
            dc,
            bmp,
            w: N,
            h: N,
            avg: rgb,
        })
    }
}

/// Процедурный фон: вертикальный градиент основного цвета и три мягких пятна. Ни одной картинки, спрайты по 64 КБ.
fn paint_aurora(
    hdc: HDC,
    sprites: &mut Vec<Image>,
    base: [u8; 3],
    (w, h): (i32, i32),
    (ax, ay): (f64, f64),
    hh: f64,
) {
    let tone = |c: u8, k: f64| ((f64::from(c) * k).min(255.0).round() as u16) << 8;
    let vertex = |y: i32, k: f64| TRIVERTEX {
        x: if y == 0 { 0 } else { w },
        y: if y == 0 { 0 } else { h },
        Red: tone(base[0], k),
        Green: tone(base[1], k),
        Blue: tone(base[2], k),
        Alpha: 0,
    };
    let verts = [vertex(0, 1.18), vertex(1, 0.82)];
    let mesh = GRADIENT_RECT {
        UpperLeft: 0,
        LowerRight: 1,
    };
    unsafe {
        let _ = GradientFill(
            hdc,
            &verts,
            (&raw const mesh).cast(),
            1,
            GRADIENT_FILL_RECT_V,
        );
    }
    if sprites.is_empty() {
        *sprites = GLOWS.iter().filter_map(|g| glow_sprite(g.3, g.4)).collect();
    }
    if sprites.len() != GLOWS.len() {
        return;
    }
    let (px, py) = (AURORA_PERIOD.0 * hh, AURORA_PERIOD.1 * hh);
    let bf = BLENDFUNCTION {
        BlendOp: 0,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: 1,
    };
    for (g, spr) in GLOWS.iter().zip(sprites.iter()) {
        let (gx, gy, r) = (ax + g.0 * hh, ay + g.1 * hh, g.2 * hh);
        let kx = (((-r - gx) / px).ceil() as i32)..=(((f64::from(w) + r - gx) / px).floor() as i32);
        let ky = (((-r - gy) / py).ceil() as i32)..=(((f64::from(h) + r - gy) / py).floor() as i32);
        for j in ky {
            for i in kx.clone() {
                let (x, y) = (gx + f64::from(i) * px - r, gy + f64::from(j) * py - r);
                unsafe {
                    let _ = AlphaBlend(
                        hdc,
                        x.round() as i32,
                        y.round() as i32,
                        (2.0 * r).round() as i32,
                        (2.0 * r).round() as i32,
                        spr.dc,
                        0,
                        0,
                        spr.w,
                        spr.h,
                        bf,
                    );
                }
            }
        }
    }
}

/// Огромная картинка: уровень выбирается по масштабу, читаются только плитки, попавшие на вид.
fn paint_pyramid(
    hdc: HDC,
    p: &mut Pyramid,
    (w, h): (i32, i32),
    (ox, oy): (f64, f64),
    z: f64,
    bg: [u8; 3],
) {
    let (iw, ih) = p.size();
    let (dw, dh) = (f64::from(iw) * z, f64::from(ih) * z);
    // картинка стоит центром в начале координат мира; видимая часть в пикселях экрана
    let (dl, dt) = (ox - dw / 2.0, oy - dh / 2.0);
    let vis = (
        dl.max(0.0),
        dt.max(0.0),
        (dl + dw).min(f64::from(w)),
        (dt + dh).min(f64::from(h)),
    );
    if vis.2 - vis.0 < 1.0 || vis.3 - vis.1 < 1.0 {
        return;
    }
    let level = p.level_for(z.min(1.0));
    // пикселей уровня на пиксель экрана
    let k = p.level_scale(level) / z;
    let src = (
        ((vis.0 - dl) * k).floor() as i64,
        ((vis.1 - dt) * k).floor() as i64,
        ((vis.2 - dl) * k).ceil() as i64,
        ((vis.3 - dt) * k).ceil() as i64,
    );
    let Some(buf) = p.read_bgra(level, src, bg) else {
        return;
    };
    let (sw, sh) = ((src.2 - src.0) as i32, (src.3 - src.1) as i32);
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: sw,
            biHeight: -sh,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        SetStretchBltMode(hdc, HALFTONE);
        StretchDIBits(
            hdc,
            vis.0.round() as i32,
            vis.1.round() as i32,
            (vis.2 - vis.0).round() as i32,
            (vis.3 - vis.1).round() as i32,
            0,
            0,
            sw,
            sh,
            Some(buf.as_ptr().cast()),
            &bmi,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}

fn paint_grid(hdc: HDC, s: &Shared, ox: f64, oy: f64, z: f64) {
    let (w, h) = s.size;
    let mut step = (s.bg.grid_size.max(4.0)) * z;
    while step < 10.0 {
        step *= 2.0;
    }
    let minor = s.bg.grid_color;
    let major = mix(minor, [255, 255, 255], 0.18);
    let lines = |origin: f64, len: i32, vertical: bool| {
        let n0 = ((0.0 - origin) / step).ceil() as i64;
        let n1 = ((f64::from(len) - origin) / step).floor() as i64;
        for n in n0..=n1 {
            let p = (origin + n as f64 * step).round() as i32;
            let c = if n.rem_euclid(5) == 0 { major } else { minor };
            let r = if vertical {
                RECT {
                    left: p,
                    top: 0,
                    right: p + 1,
                    bottom: h,
                }
            } else {
                RECT {
                    left: 0,
                    top: p,
                    right: w,
                    bottom: p + 1,
                }
            };
            fill(hdc, &r, c);
        }
    };
    lines(ox, w, true);
    lines(oy, h, false);
}

fn paint_tile(hdc: HDC, img: &Image, s: &Shared, ox: f64, oy: f64, z: f64) {
    let (w, h) = s.size;
    let (tw, th) = (f64::from(img.w) * z, f64::from(img.h) * z);
    let full = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    let (nx, ny) = (
        (f64::from(w) / tw).ceil() + 1.0,
        (f64::from(h) / th).ceil() + 1.0,
    );
    if tw < 6.0 || th < 6.0 || nx * ny > 3000.0 {
        fill(hdc, &full, img.avg);
        return;
    }
    let sx = ox.rem_euclid(tw) - tw;
    let sy = oy.rem_euclid(th) - th;
    unsafe {
        SetStretchBltMode(hdc, HALFTONE);
        let mut y = sy;
        while y < f64::from(h) {
            let mut x = sx;
            while x < f64::from(w) {
                let _ = StretchBlt(
                    hdc,
                    x.round() as i32,
                    y.round() as i32,
                    tw.round() as i32 + 1,
                    th.round() as i32 + 1,
                    Some(img.dc),
                    0,
                    0,
                    img.w,
                    img.h,
                    SRCCOPY,
                );
                x += tw;
            }
            y += th;
        }
    }
}

fn paint_items(hdc: HDC, s: &Shared) {
    unsafe {
        let font = CreateFontW(
            -15,
            0,
            0,
            0,
            600,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            windows::core::w!("Segoe UI"),
        );
        let old = SelectObject(hdc, font.into());
        SetBkMode(hdc, TRANSPARENT);
        for it in &s.scene.items {
            let r = screen_rect(&s.scene, offset(s.pos, &s.scene), &it.rect);
            if r.right < 0 || r.bottom < 0 || r.left > s.size.0 || r.top > s.size.1 {
                continue;
            }
            // подложка: видна, пока миниатюра не нарисована (и если окно не отдаёт картинку)
            fill(hdc, &r, [38, 42, 54]);
            let (col, th) = if it.focused {
                ([90, 160, 255], 3)
            } else {
                ([110, 118, 140], 1)
            };
            let b = CreateSolidBrush(rgb(col));
            for i in 1..=th {
                let fr = RECT {
                    left: r.left - i,
                    top: r.top - i,
                    right: r.right + i,
                    bottom: r.bottom + i,
                };
                FrameRect(hdc, &fr, b);
            }
            let _ = DeleteObject(b.into());
            if r.right - r.left > 40 {
                let mut text: Vec<u16> = it.title.encode_utf16().collect();
                SetTextColor(
                    hdc,
                    rgb(if it.focused {
                        [200, 225, 255]
                    } else {
                        [190, 195, 210]
                    }),
                );
                let mut tr = RECT {
                    left: r.left,
                    top: r.top - th - 22,
                    right: r.right,
                    bottom: r.top - th - 2,
                };
                DrawTextW(
                    hdc,
                    &mut text,
                    &mut tr,
                    DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX | DT_BOTTOM,
                );
            }
        }
        SelectObject(hdc, old);
        let _ = DeleteObject(font.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_pixels_keeps_aspect_and_cap() {
        assert_eq!(fit_pixels(1000, 500, 40_000_000), (1000, 500));
        let (w, h) = fit_pixels(40_000, 20_000, 40_000_000);
        assert!(u64::from(w) * u64::from(h) <= 40_000_000);
        assert_eq!(w / h, 2);
        assert_eq!(fit_pixels(1, 1_000_000_000, 10), (1, 10));
    }
}
