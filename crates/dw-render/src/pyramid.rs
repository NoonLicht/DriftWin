//! Огромные картинки фона без загрузки целиком: TIFF с плитками и уровнями (пирамида — несколько изображений в файле,
//! каждое меньше предыдущего). Читаются только плитки, попавшие на вид, нужного уровня; декодированные плитки лежат в
//! кэше ограниченного размера.
//!
//! Поддерживаются 8-битные Gray/RGB/RGBA и сжатие, которое умеет крейт `tiff` (без сжатия, LZW, Deflate, PackBits).
//! TIFF со сжатием JPEG не открывается — фон тогда загружается обычным путём (с уменьшением).

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use tiff::ColorType;
use tiff::decoder::{Decoder, DecodingResult};

/// Сколько байт декодированных плиток держим в памяти.
const CACHE_LIMIT: usize = 192 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Level {
    pub w: u32,
    pub h: u32,
    /// Номер изображения в файле (IFD).
    ifd: usize,
    /// Размер плитки (у полосы — ширина изображения и число строк в полосе).
    cw: u32,
    ch: u32,
    channels: u8,
}

struct Chunk {
    data: Vec<u8>,
    /// Размер данных плитки (у краевых меньше `cw` x `ch`).
    w: u32,
    stamp: u64,
}

pub struct Pyramid {
    dec: Decoder<BufReader<File>>,
    /// По убыванию размера; `levels[0]` — полное разрешение.
    pub levels: Vec<Level>,
    cache: HashMap<(usize, u32), Chunk>,
    cache_bytes: usize,
    cache_limit: usize,
    clock: u64,
}

impl Pyramid {
    pub fn open(path: &Path) -> Result<Self, String> {
        let f = File::open(path).map_err(|e| e.to_string())?;
        let mut dec = Decoder::new(BufReader::new(f)).map_err(|e| e.to_string())?;
        let mut levels = Vec::new();
        let mut ifd = 0usize;
        loop {
            let (w, h) = dec.dimensions().map_err(|e| e.to_string())?;
            let channels = match dec.colortype().map_err(|e| e.to_string())? {
                ColorType::Gray(8) => Some(1),
                ColorType::RGB(8) => Some(3),
                ColorType::RGBA(8) => Some(4),
                _ => None,
            };
            let (cw, ch) = dec.chunk_dimensions();
            if let Some(channels) = channels
                && w > 0
                && h > 0
                && cw > 0
                && ch > 0
            {
                levels.push(Level {
                    w,
                    h,
                    ifd,
                    cw,
                    ch,
                    channels,
                });
            }
            if !dec.more_images() {
                break;
            }
            dec.next_image().map_err(|e| e.to_string())?;
            ifd += 1;
            if ifd > 64 {
                break;
            }
        }
        levels.sort_by(|a, b| {
            (u64::from(b.w) * u64::from(b.h)).cmp(&(u64::from(a.w) * u64::from(a.h)))
        });
        let top = levels
            .first()
            .cloned()
            .ok_or("нет подходящих изображений (нужны 8-битные Gray/RGB/RGBA)")?;
        // уровни другой формы (миниатюра, этикетка) в пирамиду не входят
        levels.retain(|l| {
            let a = f64::from(l.w) / f64::from(l.h);
            let b = f64::from(top.w) / f64::from(top.h);
            (a / b - 1.0).abs() < 0.02
        });
        Ok(Self {
            dec,
            levels,
            cache: HashMap::new(),
            cache_bytes: 0,
            cache_limit: CACHE_LIMIT,
            clock: 0,
        })
    }

    /// Заменить предел кэша (в тестах).
    #[cfg(test)]
    fn set_cache_limit(&mut self, bytes: usize) {
        self.cache_limit = bytes;
    }

    pub fn size(&self) -> (u32, u32) {
        (self.levels[0].w, self.levels[0].h)
    }

    /// Самый мелкий уровень, у которого разрешение не ниже нужного: `scale` — во сколько раз картинка на экране
    /// меньше полного размера.
    pub fn level_for(&self, scale: f64) -> usize {
        let w0 = f64::from(self.levels[0].w);
        self.levels
            .iter()
            .rposition(|l| f64::from(l.w) / w0 >= scale * 0.999)
            .unwrap_or(0)
    }

    /// Доля размера уровня от полного.
    pub fn level_scale(&self, level: usize) -> f64 {
        f64::from(self.levels[level].w) / f64::from(self.levels[0].w)
    }

    fn chunk(&mut self, level: usize, index: u32) -> Option<(&[u8], u32, u32)> {
        self.clock += 1;
        let key = (level, index);
        if !self.cache.contains_key(&key) {
            let l = &self.levels[level];
            self.dec.seek_to_image(l.ifd).ok()?;
            let (dw, dh) = self.dec.chunk_data_dimensions(index);
            let data = match self.dec.read_chunk(index).ok()? {
                DecodingResult::U8(v) => v,
                _ => return None,
            };
            if data.len() < (dw * dh) as usize * usize::from(l.channels) {
                return None;
            }
            self.cache_bytes += data.len();
            self.cache.insert(
                key,
                Chunk {
                    data,
                    w: dw,
                    stamp: 0,
                },
            );
            // вытеснить самые давние
            while self.cache_bytes > self.cache_limit && self.cache.len() > 1 {
                let oldest = self
                    .cache
                    .iter()
                    .filter(|(k, _)| **k != key)
                    .min_by_key(|(_, c)| c.stamp)
                    .map(|(k, _)| *k)?;
                if let Some(c) = self.cache.remove(&oldest) {
                    self.cache_bytes -= c.data.len();
                }
            }
        }
        let c = self.cache.get_mut(&key)?;
        c.stamp = self.clock;
        let h = (c.data.len() as u32) / (c.w * u32::from(self.levels[level].channels)).max(1);
        Some((&c.data, c.w, h))
    }

    /// Прямоугольник `[x0, x1) x [y0, y1)` уровня `level` как BGRA (`GetDIBits`-совместимо). За краем изображения —
    /// цвет `fill`.
    pub fn read_bgra(
        &mut self,
        level: usize,
        (x0, y0, x1, y1): (i64, i64, i64, i64),
        fill: [u8; 3],
    ) -> Option<Vec<u8>> {
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        if w == 0 || h == 0 || w * h > 100_000_000 {
            return None;
        }
        let mut out = vec![0u8; w * h * 4];
        for px in out.as_chunks_mut::<4>().0 {
            *px = [fill[2], fill[1], fill[0], 255];
        }
        let l = self.levels[level].clone();
        let tiles_x = l.w.div_ceil(l.cw);
        let tiles_y = l.h.div_ceil(l.ch);
        let (cx0, cx1) = (
            x0.max(0) / i64::from(l.cw),
            (x1 - 1).min(i64::from(l.w) - 1) / i64::from(l.cw),
        );
        let (cy0, cy1) = (
            y0.max(0) / i64::from(l.ch),
            (y1 - 1).min(i64::from(l.h) - 1) / i64::from(l.ch),
        );
        if x1 <= 0 || y1 <= 0 || x0 >= i64::from(l.w) || y0 >= i64::from(l.h) {
            return Some(out);
        }
        let ch = usize::from(l.channels);
        for ty in cy0..=cy1.min(i64::from(tiles_y) - 1) {
            for tx in cx0..=cx1.min(i64::from(tiles_x) - 1) {
                let index = (ty as u32) * tiles_x + tx as u32;
                let Some((data, dw, dh)) = self.chunk(level, index) else {
                    continue;
                };
                let (ox, oy) = (tx * i64::from(l.cw), ty * i64::from(l.ch));
                for ry in 0..i64::from(dh) {
                    let y = oy + ry;
                    if y < y0 || y >= y1 {
                        continue;
                    }
                    for rx in 0..i64::from(dw) {
                        let x = ox + rx;
                        if x < x0 || x >= x1 {
                            continue;
                        }
                        let s = (ry as usize * dw as usize + rx as usize) * ch;
                        let d = ((y - y0) as usize * w + (x - x0) as usize) * 4;
                        let (r, g, b) = match ch {
                            1 => (data[s], data[s], data[s]),
                            _ => (data[s], data[s + 1], data[s + 2]),
                        };
                        out[d] = b;
                        out[d + 1] = g;
                        out[d + 2] = r;
                    }
                }
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Цвет точки тестовой картинки: зависит от координат и уровня.
    fn color(level: usize, x: u32, y: u32) -> [u8; 3] {
        [(x % 251) as u8, (y % 241) as u8, (level as u8) * 60 + 10]
    }

    /// Пишет TIFF без сжатия с плитками и несколькими уровнями (каждый вдвое меньше).
    fn write_tiff(path: &Path, w0: u32, h0: u32, tile: u32, levels: usize) {
        fn put16(v: &mut Vec<u8>, x: u16) {
            v.extend_from_slice(&x.to_le_bytes());
        }
        fn put32(v: &mut Vec<u8>, x: u32) {
            v.extend_from_slice(&x.to_le_bytes());
        }
        let mut f: Vec<u8> = vec![b'I', b'I', 42, 0, 0, 0, 0, 0];
        let mut prev_next_ptr = 4usize;
        for lv in 0..levels {
            let (w, h) = (w0 >> lv, h0 >> lv);
            let (tx, ty) = (w.div_ceil(tile), h.div_ceil(tile));
            let mut offsets = Vec::new();
            let mut counts = Vec::new();
            for j in 0..ty {
                for i in 0..tx {
                    offsets.push(f.len() as u32);
                    counts.push(tile * tile * 3);
                    for y in 0..tile {
                        for x in 0..tile {
                            let (px, py) = (i * tile + x, j * tile + y);
                            let c = if px < w && py < h {
                                color(lv, px, py)
                            } else {
                                [0; 3]
                            };
                            f.extend_from_slice(&c);
                        }
                    }
                }
            }
            let bps = f.len() as u32;
            for _ in 0..3 {
                put16(&mut f, 8);
            }
            let off_arr = f.len() as u32;
            offsets.iter().for_each(|o| put32(&mut f, *o));
            let cnt_arr = f.len() as u32;
            counts.iter().for_each(|c| put32(&mut f, *c));
            let ifd = f.len() as u32;
            f[prev_next_ptr..prev_next_ptr + 4].copy_from_slice(&ifd.to_le_bytes());
            let n = offsets.len() as u32;
            let entries: [(u16, u16, u32, u32); 11] = [
                (256, 4, 1, w),
                (257, 4, 1, h),
                (258, 3, 3, bps),
                (259, 3, 1, 1),
                (262, 3, 1, 2),
                (277, 3, 1, 3),
                (284, 3, 1, 1),
                (322, 4, 1, tile),
                (323, 4, 1, tile),
                // одно значение лежит прямо в записи, а не по указателю
                (324, 4, n, if n == 1 { offsets[0] } else { off_arr }),
                (325, 4, n, if n == 1 { counts[0] } else { cnt_arr }),
            ];
            put16(&mut f, entries.len() as u16);
            for (tag, ty, cnt, val) in entries {
                put16(&mut f, tag);
                put16(&mut f, ty);
                put32(&mut f, cnt);
                put32(&mut f, val);
            }
            prev_next_ptr = f.len();
            put32(&mut f, 0);
        }
        std::fs::write(path, f).unwrap();
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dw-pyr-{}-{name}.tif", std::process::id()))
    }

    #[test]
    fn reads_levels_and_pixels_across_tiles() {
        let p = tmp("a");
        write_tiff(&p, 1000, 700, 256, 3);
        let mut py = Pyramid::open(&p).unwrap();
        assert_eq!(py.levels.len(), 3);
        assert_eq!(py.size(), (1000, 700));
        assert_eq!((py.levels[1].w, py.levels[2].w), (500, 250));
        // участок на стыке четырёх плиток на уровне 0, плюс выход за правый край
        let b = py.read_bgra(0, (250, 250, 262, 258), [9, 8, 7]).unwrap();
        assert_eq!(b.len(), 12 * 8 * 4);
        for (i, px) in b.chunks(4).enumerate() {
            let (x, y) = (250 + (i % 12) as u32, 250 + (i / 12) as u32);
            let c = color(0, x, y);
            assert_eq!([px[2], px[1], px[0]], c, "({x},{y})");
        }
        let edge = py.read_bgra(0, (998, 0, 1004, 1), [9, 8, 7]).unwrap();
        assert_eq!([edge[2], edge[1], edge[0]], color(0, 998, 0));
        assert_eq!([edge[4 * 5 + 2], edge[4 * 5 + 1], edge[4 * 5]], [9, 8, 7]);
        // другой уровень читает свои данные
        let l2 = py.read_bgra(2, (10, 20, 11, 21), [0; 3]).unwrap();
        assert_eq!([l2[2], l2[1], l2[0]], color(2, 10, 20));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn level_choice_follows_zoom() {
        let p = tmp("b");
        write_tiff(&p, 1024, 512, 128, 4);
        let py = Pyramid::open(&p).unwrap();
        assert_eq!(py.level_for(1.0), 0);
        assert_eq!(py.level_for(0.5), 1);
        assert_eq!(py.level_for(0.3), 1);
        assert_eq!(py.level_for(0.25), 2);
        assert_eq!(py.level_for(0.01), 3);
        assert!((py.level_scale(2) - 0.25).abs() < 1e-9);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn cache_is_bounded_and_still_serves_pixels() {
        let p = tmp("e");
        write_tiff(&p, 1024, 1024, 256, 1);
        let mut py = Pyramid::open(&p).unwrap();
        // помещается не больше двух плиток по 192 КБ
        py.set_cache_limit(400_000);
        for ty in 0..4 {
            for tx in 0..4 {
                let (x, y) = (tx * 256 + 1, ty * 256 + 2);
                let b = py
                    .read_bgra(
                        0,
                        (
                            i64::from(x),
                            i64::from(y),
                            i64::from(x) + 1,
                            i64::from(y) + 1,
                        ),
                        [0; 3],
                    )
                    .unwrap();
                assert_eq!([b[2], b[1], b[0]], color(0, x, y));
            }
        }
        assert!(py.cache.len() <= 2, "в кэше {} плиток", py.cache.len());
        assert!(py.cache_bytes <= 400_000);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn image_smaller_than_one_tile_reads() {
        let p = tmp("d");
        write_tiff(&p, 250, 175, 256, 1);
        let mut py = Pyramid::open(&p).unwrap();
        let b = py.read_bgra(0, (3, 4, 5, 6), [0; 3]).unwrap();
        assert_eq!([b[2], b[1], b[0]], color(0, 3, 4));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn non_tiff_is_rejected() {
        let p = tmp("c");
        std::fs::write(&p, b"not a tiff").unwrap();
        assert!(Pyramid::open(&p).is_err());
        let _ = std::fs::remove_file(p);
    }
}
