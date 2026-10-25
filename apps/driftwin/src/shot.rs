//! Снимок всего холста: каждое окно снимается само (`PrintWindow`) и ставится на картинку по своим мировым координатам.

/// Снимок окна, поставленный на холст. `x`, `y` — левый верхний угол в пикселях мира (Y вниз).
pub(crate) struct Piece {
    pub x: f64,
    pub y: f64,
    pub w: i32,
    pub h: i32,
    /// RGBA, `w*h*4`.
    pub px: Vec<u8>,
}

/// Сколько пикселей допускаем в итоговой картинке.
pub(crate) const MAX_PIXELS: f64 = 64_000_000.0;
/// Поля вокруг крайних окон, px мира.
const PAD: f64 = 24.0;

/// Собрать картинку: `bg` — цвет фона, `scale` — желаемый масштаб (сверху ограничен `MAX_PIXELS`).
/// Окна рисуются по порядку, последнее — сверху. Возвращает RGBA и размер.
pub(crate) fn compose(pieces: &[Piece], bg: [u8; 3], scale: f64) -> Option<(Vec<u8>, i32, i32)> {
    if pieces.is_empty() {
        return None;
    }
    let min_x = pieces.iter().map(|p| p.x).fold(f64::MAX, f64::min) - PAD;
    let min_y = pieces.iter().map(|p| p.y).fold(f64::MAX, f64::min) - PAD;
    let max_x = pieces
        .iter()
        .map(|p| p.x + f64::from(p.w))
        .fold(f64::MIN, f64::max)
        + PAD;
    let max_y = pieces
        .iter()
        .map(|p| p.y + f64::from(p.h))
        .fold(f64::MIN, f64::max)
        + PAD;
    let (ww, wh) = (max_x - min_x, max_y - min_y);
    let s = scale.clamp(0.01, 1.0).min((MAX_PIXELS / (ww * wh)).sqrt());
    let (ow, oh) = ((ww * s).ceil() as i32, (wh * s).ceil() as i32);
    if ow < 1 || oh < 1 {
        return None;
    }
    let mut out = vec![0u8; ow as usize * oh as usize * 4];
    for c in out.as_chunks_mut::<4>().0 {
        *c = [bg[0], bg[1], bg[2], 255];
    }
    for p in pieces {
        // прямоугольник окна в координатах итоговой картинки
        let dx0 = ((p.x - min_x) * s).round() as i32;
        let dy0 = ((p.y - min_y) * s).round() as i32;
        let dw = ((f64::from(p.w) * s).round() as i32).max(1);
        let dh = ((f64::from(p.h) * s).round() as i32).max(1);
        for dy in 0..dh {
            let oy = dy0 + dy;
            if !(0..oh).contains(&oy) {
                continue;
            }
            let sy = ((dy * p.h) / dh).clamp(0, p.h - 1) as usize;
            for dx in 0..dw {
                let ox = dx0 + dx;
                if !(0..ow).contains(&ox) {
                    continue;
                }
                let sx = ((dx * p.w) / dw).clamp(0, p.w - 1) as usize;
                let src = (sy * p.w as usize + sx) * 4;
                let dst = (oy as usize * ow as usize + ox as usize) * 4;
                out[dst..dst + 4].copy_from_slice(&p.px[src..src + 4]);
            }
        }
    }
    Some((out, ow, oh))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(x: f64, y: f64, w: i32, h: i32, c: [u8; 4]) -> Piece {
        Piece {
            x,
            y,
            w,
            h,
            px: c.repeat((w * h) as usize),
        }
    }

    fn at(img: &(Vec<u8>, i32, i32), x: i32, y: i32) -> [u8; 4] {
        let i = ((y * img.1 + x) * 4) as usize;
        img.0[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn places_windows_by_world_position_with_padding() {
        let ps = [
            piece(0.0, 0.0, 10, 10, [255, 0, 0, 255]),
            piece(100.0, 50.0, 20, 20, [0, 255, 0, 255]),
        ];
        let img = compose(&ps, [1, 2, 3], 1.0).unwrap();
        // поля 24 px с каждой стороны: 120 + 48 по ширине, 70 + 48 по высоте
        assert_eq!((img.1, img.2), (168, 118));
        assert_eq!(at(&img, 24 + 5, 24 + 5), [255, 0, 0, 255]);
        assert_eq!(at(&img, 24 + 110, 24 + 60), [0, 255, 0, 255]);
        assert_eq!(at(&img, 2, 2), [1, 2, 3, 255]);
    }

    #[test]
    fn later_windows_are_on_top() {
        let ps = [
            piece(0.0, 0.0, 20, 20, [255, 0, 0, 255]),
            piece(10.0, 10.0, 20, 20, [0, 0, 255, 255]),
        ];
        let img = compose(&ps, [0, 0, 0], 1.0).unwrap();
        assert_eq!(at(&img, 24 + 15, 24 + 15), [0, 0, 255, 255]);
        assert_eq!(at(&img, 24 + 5, 24 + 5), [255, 0, 0, 255]);
    }

    #[test]
    fn scale_shrinks_and_huge_canvas_is_capped() {
        let ps = [piece(0.0, 0.0, 100, 100, [9, 9, 9, 255])];
        let img = compose(&ps, [0, 0, 0], 0.5).unwrap();
        assert_eq!((img.1, img.2), (74, 74));
        let far = [
            piece(0.0, 0.0, 10, 10, [9, 9, 9, 255]),
            piece(60_000.0, 60_000.0, 10, 10, [9, 9, 9, 255]),
        ];
        let big = compose(&far, [0, 0, 0], 1.0).unwrap();
        assert!(f64::from(big.1) * f64::from(big.2) <= MAX_PIXELS * 1.01);
    }

    #[test]
    fn nothing_to_draw_is_none() {
        assert!(compose(&[], [0, 0, 0], 1.0).is_none());
    }
}
