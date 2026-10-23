//! Привязка окон друг к другу, кластеры и изменение размера с сохранением кластера.
//! Всё в мировых координатах (Y вверх), без Win32.

use crate::{Dir, Rect, Vec2, WindowId};

/// Настройки привязки (`[snap]`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapCfg {
    pub enabled: bool,
    /// Зазор между привязанными окнами.
    pub gap: f64,
    /// Отступ от края вида при fit/fill.
    pub outer_gap: f64,
    /// На каком расстоянии срабатывает привязка.
    pub distance: f64,
    /// Насколько надо «дёрнуть» окно, чтобы оно отлипло.
    pub break_force: f64,
    /// Выравнивать углы (края) соседних окон.
    pub corners: bool,
    /// Выравнивать центры соседних окон.
    pub centers: bool,
}

impl Default for SnapCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            gap: 12.0,
            outer_gap: 12.0,
            distance: 24.0,
            break_force: 32.0,
            corners: true,
            centers: false,
        }
    }
}

/// Допуск при определении соседства: округление до пикселей и минимальные размеры приложений.
const ADJ_TOL: f64 = 4.0;
/// Перекрытие меньше этого значения окнами не считается.
const OVERLAP_EPS: f64 = 1.0;

/// Границы прямоугольника по оси: 0 — X, 1 — Y.
fn span(r: &Rect, ax: usize) -> (f64, f64) {
    if ax == 0 {
        (r.center.x - r.w / 2.0, r.center.x + r.w / 2.0)
    } else {
        (r.center.y - r.h / 2.0, r.center.y + r.h / 2.0)
    }
}

fn from_spans(x: (f64, f64), y: (f64, f64)) -> Rect {
    Rect {
        center: Vec2::new((x.0 + x.1) / 2.0, (y.0 + y.1) / 2.0),
        w: x.1 - x.0,
        h: y.1 - y.0,
    }
}

/// Расстояние между отрезками; отрицательное — глубина перекрытия.
fn gap_between(a: (f64, f64), b: (f64, f64)) -> f64 {
    (b.0 - a.1).max(a.0 - b.1)
}

fn translated(r: &Rect, d: Vec2) -> Rect {
    Rect {
        center: r.center + d,
        ..*r
    }
}

/// Окна накладываются друг на друга заметнее допуска.
pub fn overlaps(a: &Rect, b: &Rect) -> bool {
    gap_between(span(a, 0), span(b, 0)) < -OVERLAP_EPS
        && gap_between(span(a, 1), span(b, 1)) < -OVERLAP_EPS
}

/// Смещения `m` по оси `ax`, дающие привязку к `o` (без ограничения по расстоянию).
fn axis_candidates(m: &Rect, o: &Rect, ax: usize, cfg: &SnapCfg) -> Vec<f64> {
    let perp = 1 - ax;
    let pg = gap_between(span(m, perp), span(o, perp));
    if pg > cfg.gap + cfg.distance {
        return vec![];
    }
    let (a, b) = (span(m, ax), span(o, ax));
    // край к краю с зазором
    let mut v = vec![(b.0 - cfg.gap) - a.1, (b.1 + cfg.gap) - a.0];
    // выравнивание имеет смысл только для окон, стоящих друг над другом (не перекрывающихся по другой оси)
    if pg >= -OVERLAP_EPS {
        if cfg.corners {
            v.push(b.0 - a.0);
            v.push(b.1 - a.1);
        }
        if cfg.centers {
            v.push((b.0 + b.1) / 2.0 - (a.0 + a.1) / 2.0);
        }
    }
    v
}

fn axis_snap(m: &Rect, others: &[Rect], ax: usize, cfg: &SnapCfg) -> Option<f64> {
    others
        .iter()
        .flat_map(|o| axis_candidates(m, o, ax, cfg))
        .filter(|d| d.abs() <= cfg.distance)
        .min_by(|a, b| a.abs().total_cmp(&b.abs()))
}

/// Расстояние до следующей точки привязки в направлении `dir` (для `move-to-snap`).
pub fn next_snap(win: &Rect, others: &[Rect], dir: Dir, cfg: &SnapCfg) -> Option<f64> {
    let (ax, sign) = match dir {
        Dir::Right => (0, 1.0),
        Dir::Left => (0, -1.0),
        Dir::Up => (1, 1.0),
        Dir::Down => (1, -1.0),
    };
    // для поиска точек привязки считаем «рядом» любые окна в полосе движения
    let wide = SnapCfg {
        distance: f64::MAX / 4.0,
        ..*cfg
    };
    others
        .iter()
        .flat_map(|o| {
            let pg = gap_between(span(win, 1 - ax), span(o, 1 - ax));
            if pg > cfg.gap + cfg.distance {
                vec![]
            } else {
                axis_candidates(win, o, ax, &wide)
            }
        })
        .map(|d| d * sign)
        .filter(|d| *d > 0.5)
        .min_by(|a, b| a.total_cmp(b))
}

/// «Липкое» значение: удерживается, пока исходное положение не уйдёт дальше `break_force`.
#[derive(Debug, Clone, Copy, Default)]
struct Latch(Option<f64>);

impl Latch {
    /// `raw` — положение без привязки, `best` — привязанное значение (если есть цель рядом).
    fn step(&mut self, raw: f64, best: impl FnOnce() -> Option<f64>, break_force: f64) -> f64 {
        if let Some(s) = self.0 {
            if (raw - s).abs() <= break_force {
                return s;
            }
            self.0 = None;
        }
        match best() {
            Some(s) => {
                self.0 = Some(s);
                s
            }
            None => raw,
        }
    }
}

/// Привязка при перетаскивании окна или группы окон.
#[derive(Debug, Clone, Default)]
pub struct SnapDrag {
    latch: [Latch; 2],
}

impl SnapDrag {
    /// `raw` — суммарное смещение курсора с начала перетаскивания (мир), `members` — прямоугольники
    /// перетаскиваемых окон в начале, `others` — все остальные. Возвращает итоговое смещение.
    pub fn update(&mut self, raw: Vec2, members: &[Rect], others: &[Rect], cfg: &SnapCfg) -> Vec2 {
        if !cfg.enabled || members.is_empty() {
            self.latch = Default::default();
            return raw;
        }
        let at_raw: Vec<Rect> = members.iter().map(|m| translated(m, raw)).collect();
        let r = [raw.x, raw.y];
        let mut out = r;
        for ax in 0..2 {
            out[ax] = self.latch[ax].step(
                r[ax],
                || {
                    at_raw
                        .iter()
                        .filter_map(|m| axis_snap(m, others, ax, cfg))
                        .min_by(|a, b| a.abs().total_cmp(&b.abs()))
                        .map(|off| r[ax] + off)
                },
                cfg.break_force,
            );
        }
        // привязка не должна загонять окно в другое окно
        let clash = |d: Vec2| {
            members
                .iter()
                .any(|m| others.iter().any(|o| overlaps(&translated(m, d), o)))
        };
        let raw_clash = clash(raw);
        let mut cand = vec![Vec2::new(out[0], out[1])];
        if out[0] != r[0] && out[1] != r[1] {
            cand.push(Vec2::new(out[0], r[1]));
            cand.push(Vec2::new(r[0], out[1]));
        }
        for c in cand {
            if raw_clash || !clash(c) {
                if c.x == r[0] {
                    self.latch[0] = Latch(None);
                }
                if c.y == r[1] {
                    self.latch[1] = Latch(None);
                }
                return c;
            }
        }
        self.latch = Default::default();
        raw
    }
}

/// Какие стороны окна тянет пользователь.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl Edges {
    /// Стороны, ближайшие к точке `p` (в мире): тянем за ближайший угол.
    pub fn nearest_corner(win: &Rect, p: Vec2) -> Edges {
        let right = p.x >= win.center.x;
        let top = p.y >= win.center.y;
        Edges {
            left: !right,
            right,
            top,
            bottom: !top,
        }
    }

    pub fn from_dir(d: Dir) -> Edges {
        Edges {
            left: d == Dir::Left,
            right: d == Dir::Right,
            top: d == Dir::Up,
            bottom: d == Dir::Down,
        }
    }
}

/// Смещение сторон на `d` (мир, Y вверх) с соблюдением минимального размера.
pub fn resize_by(start: &Rect, e: Edges, d: Vec2, min: (f64, f64)) -> Rect {
    let (mut x, mut y) = (span(start, 0), span(start, 1));
    if e.left {
        x.0 = (x.0 + d.x).min(x.1 - min.0);
    }
    if e.right {
        x.1 = (x.1 + d.x).max(x.0 + min.0);
    }
    if e.bottom {
        y.0 = (y.0 + d.y).min(y.1 - min.1);
    }
    if e.top {
        y.1 = (y.1 + d.y).max(y.0 + min.1);
    }
    from_spans(x, y)
}

/// Привязка тянущихся сторон к краям соседей при изменении размера.
#[derive(Debug, Clone, Default)]
pub struct SnapResize {
    /// left, right, bottom, top
    latch: [Latch; 4],
}

impl SnapResize {
    pub fn update(
        &mut self,
        start: &Rect,
        e: Edges,
        d: Vec2,
        others: &[Rect],
        cfg: &SnapCfg,
        min: (f64, f64),
    ) -> Rect {
        let raw = resize_by(start, e, d, min);
        if !cfg.enabled {
            return raw;
        }
        let mut sp = [span(&raw, 0), span(&raw, 1)];
        // (флаг, ось, это верхняя граница отрезка)
        let sides = [
            (e.left, 0usize, false, 0usize),
            (e.right, 0, true, 1),
            (e.bottom, 1, false, 2),
            (e.top, 1, true, 3),
        ];
        for (on, ax, hi, li) in sides {
            if !on {
                continue;
            }
            let mine = sp[1 - ax];
            let raw_c = if hi { sp[ax].1 } else { sp[ax].0 };
            let c = self.latch[li].step(
                raw_c,
                || {
                    let perp = 1 - ax;
                    let mut best: Option<f64> = None;
                    for o in others {
                        if gap_between(mine, span(o, perp)) > cfg.gap + cfg.distance {
                            continue;
                        }
                        let s = span(o, ax);
                        let targets = if hi {
                            [Some(s.0 - cfg.gap), cfg.corners.then_some(s.1)]
                        } else {
                            [Some(s.1 + cfg.gap), cfg.corners.then_some(s.0)]
                        };
                        for t in targets.into_iter().flatten() {
                            if (t - raw_c).abs() <= cfg.distance
                                && best.is_none_or(|b| (t - raw_c).abs() < (b - raw_c).abs())
                            {
                                best = Some(t);
                            }
                        }
                    }
                    best
                },
                cfg.break_force,
            );
            if hi {
                sp[ax].1 = c.max(sp[ax].0 + 1.0);
            } else {
                sp[ax].0 = c.min(sp[ax].1 - 1.0);
            }
        }
        let snapped = from_spans(sp[0], sp[1]);
        // минимальный размер важнее привязки
        if snapped.w < min.0 || snapped.h < min.1 {
            raw
        } else {
            snapped
        }
    }
}

// ---------- кластеры ----------

/// Окна `a` и `b` стоят вплотную (через зазор `gap`) и перекрываются по другой оси.
pub fn adjacent(a: &Rect, b: &Rect, gap: f64) -> bool {
    (0..2).any(|ax| {
        let perp = 1 - ax;
        if gap_between(span(a, perp), span(b, perp)) >= -OVERLAP_EPS {
            return false;
        }
        let d = gap_between(span(a, ax), span(b, ax));
        (-ADJ_TOL..=gap + ADJ_TOL).contains(&d)
    })
}

/// Кластер окна `id`: все окна, связанные с ним цепочкой соседства (включая само `id`).
pub fn cluster_of(items: &[(WindowId, Rect)], id: WindowId, gap: f64) -> Vec<WindowId> {
    let Some(start) = items.iter().position(|(i, _)| *i == id) else {
        return vec![];
    };
    let mut seen = vec![false; items.len()];
    let mut stack = vec![start];
    seen[start] = true;
    let mut out = vec![];
    while let Some(i) = stack.pop() {
        out.push(items[i].0);
        for (j, (_, rb)) in items.iter().enumerate() {
            if !seen[j] && adjacent(&items[i].1, rb, gap) {
                seen[j] = true;
                stack.push(j);
            }
        }
    }
    out
}

/// Окна, присоединённые к стороне `side` окна `id` (транзитивно, не заходя в `id`).
fn downstream(items: &[(WindowId, Rect)], id: WindowId, side: Dir, gap: f64) -> Vec<usize> {
    let Some(me) = items.iter().position(|(i, _)| *i == id) else {
        return vec![];
    };
    let r = &items[me].1;
    let (ax, hi) = match side {
        Dir::Right => (0, true),
        Dir::Left => (0, false),
        Dir::Up => (1, true),
        Dir::Down => (1, false),
    };
    let mine = span(r, ax);
    let mut seen = vec![false; items.len()];
    seen[me] = true;
    let mut stack = vec![];
    for (j, (_, o)) in items.iter().enumerate() {
        let s = span(o, ax);
        let on_side = if hi {
            s.0 >= mine.1 - ADJ_TOL
        } else {
            s.1 <= mine.0 + ADJ_TOL
        };
        if j != me && on_side && adjacent(r, o, gap) {
            seen[j] = true;
            stack.push(j);
        }
    }
    let mut out = vec![];
    while let Some(i) = stack.pop() {
        out.push(i);
        for (j, (_, o)) in items.iter().enumerate() {
            if !seen[j] && adjacent(&items[i].1, o, gap) {
                seen[j] = true;
                stack.push(j);
            }
        }
    }
    out
}

/// Новый прямоугольник окна `id` и сдвиги соседей, чтобы кластер остался сомкнутым.
/// Возвращает только изменившиеся окна (само `id` — первым).
pub fn resize_keep_cluster(
    items: &[(WindowId, Rect)],
    id: WindowId,
    new: Rect,
    gap: f64,
) -> Vec<(WindowId, Rect)> {
    let Some(old) = items.iter().find(|(i, _)| *i == id).map(|(_, r)| *r) else {
        return vec![];
    };
    let mut shift = vec![Vec2::default(); items.len()];
    let (ox, oy, nx, ny) = (span(&old, 0), span(&old, 1), span(&new, 0), span(&new, 1));
    for (side, dx, dy) in [
        (Dir::Right, nx.1 - ox.1, 0.0),
        (Dir::Left, nx.0 - ox.0, 0.0),
        (Dir::Up, 0.0, ny.1 - oy.1),
        (Dir::Down, 0.0, ny.0 - oy.0),
    ] {
        if dx == 0.0 && dy == 0.0 {
            continue;
        }
        for j in downstream(items, id, side, gap) {
            shift[j] = shift[j] + Vec2::new(dx, dy);
        }
    }
    let mut out = vec![(id, new)];
    for (j, (wid, r)) in items.iter().enumerate() {
        if *wid != id && (shift[j].x != 0.0 || shift[j].y != 0.0) {
            out.push((*wid, translated(r, shift[j])));
        }
    }
    out
}

// ---------- fit / fill ----------

/// Прямоугольник вида, сжатый на `padding` с каждой стороны (для fit-window).
pub fn fit_rect(view: &Rect, padding: f64) -> Rect {
    let w = (view.w - 2.0 * padding).max(50.0);
    let h = (view.h - 2.0 * padding).max(50.0);
    Rect {
        center: view.center,
        w,
        h,
    }
}

/// Растянуть окно на свободное место: до соседей (с зазором) и границ вида (с отступом).
pub fn fill_rect(win: &Rect, others: &[Rect], view: &Rect, cfg: &SnapCfg) -> Rect {
    let mut s = [span(win, 0), span(win, 1)];
    let bounds = [
        (
            span(view, 0).0 + cfg.outer_gap,
            span(view, 0).1 - cfg.outer_gap,
        ),
        (
            span(view, 1).0 + cfg.outer_gap,
            span(view, 1).1 - cfg.outer_gap,
        ),
    ];
    // сначала по X, затем по Y — уже с новой шириной
    for ax in 0..2 {
        let perp = 1 - ax;
        let mut hi = bounds[ax].1;
        let mut lo = bounds[ax].0;
        for o in others {
            if gap_between(s[perp], span(o, perp)) >= -OVERLAP_EPS {
                continue;
            }
            let t = span(o, ax);
            if t.0 >= s[ax].1 - ADJ_TOL {
                hi = hi.min(t.0 - cfg.gap);
            } else if t.1 <= s[ax].0 + ADJ_TOL {
                lo = lo.max(t.1 + cfg.gap);
            }
        }
        s[ax].1 = s[ax].1.max(hi);
        s[ax].0 = s[ax].0.min(lo);
    }
    from_spans(s[0], s[1])
}

/// Растянуть только сторону `dir` до ближайшего соседа (с зазором). `None`, если упереться не во что.
pub fn extend_side(win: &Rect, others: &[Rect], dir: Dir, cfg: &SnapCfg) -> Option<Rect> {
    let huge = Rect {
        center: win.center,
        w: 2.0e6,
        h: 2.0e6,
    };
    let filled = fill_rect(
        win,
        others,
        &huge,
        &SnapCfg {
            outer_gap: 0.0,
            ..*cfg
        },
    );
    let (x, y) = (span(win, 0), span(win, 1));
    let (fx, fy) = (span(&filled, 0), span(&filled, 1));
    let (nx, ny) = match dir {
        Dir::Right => ((x.0, fx.1), y),
        Dir::Left => ((fx.0, x.1), y),
        Dir::Up => (x, (y.0, fy.1)),
        Dir::Down => (x, (fy.0, y.1)),
    };
    let moved =
        (nx.1 - x.1).abs().max((nx.0 - x.0).abs()) + (ny.1 - y.1).abs().max((ny.0 - y.0).abs());
    (moved > 0.5 && moved < 1.0e5).then(|| from_spans(nx, ny))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rc(x0: f64, y0: f64, w: f64, h: f64) -> Rect {
        Rect {
            center: Vec2::new(x0 + w / 2.0, y0 + h / 2.0),
            w,
            h,
        }
    }

    fn cfg() -> SnapCfg {
        SnapCfg::default()
    }

    #[test]
    fn snaps_to_neighbor_with_gap_and_aligns_edges() {
        let other = rc(0.0, 0.0, 400.0, 300.0);
        // справа от соседа на расстоянии 20 вместо зазора 12, по вертикали сдвиг 10
        let m = rc(412.0 + 8.0, 10.0, 300.0, 300.0);
        let mut s = SnapDrag::default();
        let d = s.update(Vec2::default(), &[m], &[other], &cfg());
        assert_eq!(d.x, -8.0);
        assert_eq!(d.y, -10.0); // выровнялись нижние края
    }

    #[test]
    fn no_snap_when_far_or_disabled() {
        let other = rc(0.0, 0.0, 400.0, 300.0);
        let m = rc(500.0, 0.0, 300.0, 300.0);
        let mut s = SnapDrag::default();
        assert_eq!(
            s.update(Vec2::new(5.0, 0.0), &[m], &[other], &cfg()),
            Vec2::new(5.0, 0.0)
        );
        let off = SnapCfg {
            enabled: false,
            ..cfg()
        };
        let near = rc(415.0, 0.0, 300.0, 300.0);
        assert_eq!(
            s.update(Vec2::new(1.0, 2.0), &[near], &[other], &off),
            Vec2::new(1.0, 2.0)
        );
    }

    #[test]
    fn break_force_holds_then_releases() {
        let other = rc(0.0, 0.0, 400.0, 300.0);
        let m = rc(420.0, 0.0, 300.0, 300.0);
        let mut s = SnapDrag::default();
        // притянулись к x = 412
        assert_eq!(s.update(Vec2::default(), &[m], &[other], &cfg()).x, -8.0);
        // отодвинули на 20 — всё ещё держим (break_force 32)
        assert_eq!(
            s.update(Vec2::new(20.0, 0.0), &[m], &[other], &cfg()).x,
            -8.0
        );
        // на 60 — отпустило, окно там, где курсор
        assert_eq!(
            s.update(Vec2::new(60.0, 0.0), &[m], &[other], &cfg()).x,
            60.0
        );
    }

    #[test]
    fn snap_never_creates_overlap() {
        // два окна с зазором 30: притяжка к одному не должна загнать окно в другое
        let a = rc(0.0, 0.0, 200.0, 200.0);
        let b = rc(330.0, 0.0, 200.0, 200.0);
        let m = rc(218.0, 0.0, 100.0, 200.0); // между ними, слева зазор 18, справа 12
        let mut s = SnapDrag::default();
        let d = s.update(Vec2::default(), &[m], &[a, b], &cfg());
        let r = translated(&m, d);
        assert!(!overlaps(&r, &a) && !overlaps(&r, &b));
    }

    #[test]
    fn pseudo_random_snaps_keep_windows_apart() {
        let mut seed = 12345u64;
        let mut rnd = |n: f64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as f64 / (1u64 << 31) as f64) * n
        };
        for _ in 0..500 {
            let mut others: Vec<Rect> = vec![];
            while others.len() < 4 {
                let r = rc(
                    rnd(1500.0),
                    rnd(900.0),
                    100.0 + rnd(300.0),
                    100.0 + rnd(300.0),
                );
                if others.iter().all(|o| !overlaps(&r, o)) {
                    others.push(r);
                }
            }
            let m = rc(
                rnd(1500.0),
                rnd(900.0),
                100.0 + rnd(300.0),
                100.0 + rnd(300.0),
            );
            if others.iter().any(|o| overlaps(&m, o)) {
                continue;
            }
            let mut s = SnapDrag::default();
            let d = s.update(Vec2::default(), &[m], &others, &cfg());
            let r = translated(&m, d);
            assert!(others.iter().all(|o| !overlaps(&r, o)));
        }
    }

    #[test]
    fn clusters_follow_adjacency() {
        let items = [
            (WindowId(1), rc(0.0, 0.0, 400.0, 300.0)),
            (WindowId(2), rc(412.0, 0.0, 400.0, 300.0)),
            (WindowId(3), rc(412.0, 312.0, 400.0, 300.0)), // над вторым
            (WindowId(4), rc(2000.0, 0.0, 400.0, 300.0)),  // далеко
            (WindowId(5), rc(0.0, 700.0, 400.0, 300.0)),   // далеко по вертикали от всех
        ];
        let mut c = cluster_of(&items, WindowId(1), 12.0);
        c.sort_by_key(|w| w.0);
        assert_eq!(c, vec![WindowId(1), WindowId(2), WindowId(3)]);
        assert_eq!(cluster_of(&items, WindowId(4), 12.0), vec![WindowId(4)]);
        assert!(cluster_of(&items, WindowId(9), 12.0).is_empty());
    }

    #[test]
    fn resize_moves_downstream_neighbors() {
        let items = [
            (WindowId(1), rc(0.0, 0.0, 400.0, 300.0)),
            (WindowId(2), rc(412.0, 0.0, 400.0, 300.0)),
            (WindowId(3), rc(824.0, 0.0, 400.0, 300.0)),
            (WindowId(4), rc(-412.0, 0.0, 400.0, 300.0)),
        ];
        // правая сторона первого окна +100 px
        let new = rc(0.0, 0.0, 500.0, 300.0);
        let out = resize_keep_cluster(&items, WindowId(1), new, 12.0);
        let get = |id: u64| out.iter().find(|(w, _)| w.0 == id).map(|(_, r)| *r);
        assert_eq!(get(1), Some(new));
        assert_eq!(get(2).unwrap().center.x, items[1].1.center.x + 100.0);
        assert_eq!(get(3).unwrap().center.x, items[2].1.center.x + 100.0);
        assert!(get(4).is_none(), "левый сосед не должен двигаться");
    }

    #[test]
    fn resize_snaps_edge_to_neighbor() {
        let win = rc(0.0, 0.0, 300.0, 300.0);
        let other = rc(412.0, 0.0, 300.0, 300.0);
        let mut s = SnapResize::default();
        let e = Edges {
            right: true,
            ..Default::default()
        };
        // правую сторону тянем до 305 + ... цель — левый край соседа минус зазор = 400
        let r = s.update(
            &win,
            e,
            Vec2::new(95.0, 0.0),
            &[other],
            &cfg(),
            (50.0, 50.0),
        );
        assert_eq!(span(&r, 0).1, 400.0);
        assert_eq!(span(&r, 0).0, 0.0);
    }

    #[test]
    fn resize_respects_min_size() {
        let win = rc(0.0, 0.0, 300.0, 300.0);
        let r = resize_by(
            &win,
            Edges {
                right: true,
                ..Default::default()
            },
            Vec2::new(-1000.0, 0.0),
            (120.0, 120.0),
        );
        assert_eq!(r.w, 120.0);
        assert_eq!(span(&r, 0).0, 0.0);
    }

    #[test]
    fn fill_expands_to_neighbors_and_view() {
        let view = rc(-500.0, -400.0, 1000.0, 800.0);
        let win = rc(-100.0, -100.0, 200.0, 200.0);
        let right = rc(300.0, -400.0, 100.0, 800.0);
        let r = fill_rect(&win, &[right], &view, &cfg());
        let (x, y) = (span(&r, 0), span(&r, 1));
        assert_eq!(x.1, 288.0); // до соседа с зазором
        assert_eq!(x.0, -488.0); // до края вида с отступом
        assert_eq!(y, (-388.0, 388.0));
    }

    #[test]
    fn next_snap_finds_closest_point() {
        let win = rc(0.0, 0.0, 200.0, 200.0);
        let far = rc(1000.0, 0.0, 200.0, 200.0);
        let near = rc(500.0, 0.0, 200.0, 200.0);
        let d = next_snap(&win, &[far, near], Dir::Right, &cfg()).unwrap();
        // до выравнивания по левому краю ближнего окна? нет — ближайшая цель: вплотную с зазором = 288
        assert!((d - 288.0).abs() < 1e-9, "{d}");
        assert!(next_snap(&win, &[far, near], Dir::Left, &cfg()).is_none());
    }

    #[test]
    fn extend_side_stops_at_neighbor_or_nothing() {
        let win = rc(0.0, 0.0, 200.0, 200.0);
        let right = rc(500.0, 0.0, 200.0, 200.0);
        let r = extend_side(&win, &[right], Dir::Right, &cfg()).unwrap();
        assert_eq!(span(&r, 0), (0.0, 488.0));
        assert_eq!(span(&r, 1), (0.0, 200.0));
        assert!(extend_side(&win, &[right], Dir::Left, &cfg()).is_none());
    }

    #[test]
    fn fit_rect_shrinks_by_padding() {
        let v = rc(0.0, 0.0, 1920.0, 1080.0);
        let r = fit_rect(&v, 80.0);
        assert_eq!((r.w, r.h), (1760.0, 920.0));
        assert_eq!(r.center, v.center);
    }
}
