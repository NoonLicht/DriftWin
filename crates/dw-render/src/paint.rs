//! Рисование фона и подложек окон средствами GDI.

use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::*;

use crate::{BgCfg, BgKind, Kind, Shared, screen_rect};

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
    let (w, h) = (img.width() as i32, img.height() as i32);
    if w == 0 || h == 0 {
        return None;
    }
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

pub fn paint(hdc: HDC, s: &mut Shared) {
    let (w, h) = s.size;
    unsafe {
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp.into());
        paint_bg(mem, s);
        if s.kind == Kind::Overview {
            paint_items(mem, s);
        }
        let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
    }
}

fn paint_bg(hdc: HDC, s: &Shared) {
    let (w, h) = s.size;
    let full = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    let (cx, cy, z) = (s.scene.center.x, s.scene.center.y, s.scene.zoom);
    // начало координат мира на экране
    let ox = -cx * z + f64::from(w) / 2.0;
    let oy = cy * z + f64::from(h) / 2.0;
    fill(hdc, &full, s.bg.color);
    match s.bg.kind {
        BgKind::None | BgKind::Solid => {}
        BgKind::Grid => paint_grid(hdc, s, ox, oy, z),
        BgKind::Tile => {
            if let Some(img) = &s.image {
                paint_tile(hdc, img, s, ox, oy, z);
            }
        }
        BgKind::Wallpaper => {
            if let Some(img) = &s.image {
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
            let r = screen_rect(&s.scene, s.size, &it.rect);
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
