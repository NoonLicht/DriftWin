//! Холст: окна в мировых координатах и камера. Зум в Этапе 1 всегда 1.0.

use std::collections::{HashMap, HashSet};

use crate::{Dir, ScreenRect, Vec2, Viewport, WindowId};

pub struct Canvas {
    pub viewport: Viewport,
    /// Левый верхний угол монитора в экранных координатах (виртуального рабочего стола).
    pub origin: (i32, i32),
    home: Vec2,
    /// Центр и размер окна в мире.
    windows: HashMap<WindowId, (Vec2, (i32, i32))>,
    /// Закреплённые окна: остаются на экране на месте, камера их не двигает.
    pinned: HashMap<WindowId, ScreenRect>,
    /// Виджеты — закреплённые окна, которые не участвуют в навигации (focus-nearest, MRU).
    widgets: HashSet<WindowId>,
}

impl Canvas {
    pub fn new(origin: (i32, i32), size_px: (u32, u32)) -> Self {
        let center = Vec2::new(0.0, 0.0);
        Self {
            viewport: Viewport {
                center,
                zoom: 1.0,
                size_px,
            },
            origin,
            home: center,
            windows: HashMap::new(),
            pinned: HashMap::new(),
            widgets: HashSet::new(),
        }
    }

    pub fn contains(&self, id: WindowId) -> bool {
        self.windows.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    fn local(&self, r: ScreenRect) -> Vec2 {
        Vec2::new(
            (r.x - self.origin.0) as f64 + r.w as f64 / 2.0,
            (r.y - self.origin.1) as f64 + r.h as f64 / 2.0,
        )
    }

    /// Положить окно на холст по его текущему видимому прямоугольнику.
    pub fn adopt(&mut self, id: WindowId, screen: ScreenRect) {
        let c = self.viewport.screen_to_world(self.local(screen));
        self.windows.insert(id, (c, (screen.w, screen.h)));
        if let Some(p) = self.pinned.get_mut(&id) {
            *p = screen;
        }
    }

    pub fn forget(&mut self, id: WindowId) {
        self.windows.remove(&id);
        self.pinned.remove(&id);
        self.widgets.remove(&id);
    }

    /// Закрепить окно на экране (не двигается камерой, вне навигации).
    pub fn pin(&mut self, id: WindowId, screen: ScreenRect) {
        self.pinned.insert(id, screen);
    }

    /// Виджет: остаётся на холсте, но не участвует в навигации (focus-nearest, MRU).
    pub fn set_widget(&mut self, id: WindowId, on: bool) {
        if on {
            self.widgets.insert(id);
        } else {
            self.widgets.remove(&id);
        }
    }

    /// Поставить окно в точку мира (центр окна).
    pub fn set_world_center(&mut self, id: WindowId, c: Vec2) {
        if let Some(w) = self.windows.get_mut(&id) {
            w.0 = c;
        }
    }

    /// Открепить: окно остаётся там, где стоит на экране, и дальше живёт на холсте.
    pub fn unpin(&mut self, id: WindowId) {
        if let Some(r) = self.pinned.remove(&id) {
            self.adopt(id, r);
        }
    }

    pub fn is_pinned(&self, id: WindowId) -> bool {
        self.pinned.contains_key(&id)
    }

    pub fn is_widget(&self, id: WindowId) -> bool {
        self.widgets.contains(&id)
    }

    /// Начальная точка камеры.
    pub fn home(&self) -> Vec2 {
        self.home
    }

    /// Центр окна в мире.
    pub fn window_center(&self, id: WindowId) -> Option<Vec2> {
        self.windows.get(&id).map(|(c, _)| *c)
    }

    /// Окна, участвующие в навигации (не закреплённые и не виджеты).
    pub fn navigable(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.windows
            .keys()
            .copied()
            .filter(|id| !self.pinned.contains_key(id) && !self.widgets.contains(id))
    }

    /// Ближайшее окно в направлении `dir` от точки `from` (в пределах конуса примерно ±63°).
    /// Расстояние по оси считается прямо, отклонение вбок штрафуется вдвое.
    pub fn nearest(&self, from: Vec2, skip: Option<WindowId>, dir: Dir) -> Option<WindowId> {
        self.navigable()
            .filter(|id| Some(*id) != skip)
            .filter_map(|id| {
                let c = self.windows[&id].0;
                let (dx, dy) = (c.x - from.x, c.y - from.y);
                let (along, perp) = match dir {
                    Dir::Right => (dx, dy),
                    Dir::Left => (-dx, dy),
                    Dir::Up => (dy, dx),
                    Dir::Down => (-dy, dx),
                };
                (along > 0.0 && perp.abs() <= along * 2.0).then_some((id, along + 2.0 * perp.abs()))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }

    /// Сдвинуть камеру на дельту в пикселях экрана (вправо/вниз положительны).
    pub fn pan_px(&mut self, dx: f64, dy: f64) {
        self.viewport.center.x += dx / self.viewport.zoom;
        self.viewport.center.y -= dy / self.viewport.zoom;
    }

    fn rect_at(&self, vp: &Viewport, c: Vec2, size: (i32, i32)) -> ScreenRect {
        let s = vp.world_to_screen(c);
        ScreenRect {
            x: (s.x - size.0 as f64 / 2.0).round() as i32 + self.origin.0,
            y: (s.y - size.1 as f64 / 2.0).round() as i32 + self.origin.1,
            w: size.0,
            h: size.1,
        }
    }

    pub fn screen_rect(&self, id: WindowId) -> Option<ScreenRect> {
        if let Some(p) = self.pinned.get(&id) {
            return Some(*p);
        }
        let (c, sz) = self.windows.get(&id)?;
        Some(self.rect_at(&self.viewport, *c, *sz))
    }

    /// Положение окна при камере в исходной точке (для baseline поздно взятых окон).
    pub fn home_rect(&self, id: WindowId) -> Option<ScreenRect> {
        if let Some(p) = self.pinned.get(&id) {
            return Some(*p);
        }
        let (c, sz) = self.windows.get(&id)?;
        let vp = Viewport {
            center: self.home,
            ..self.viewport
        };
        Some(self.rect_at(&vp, *c, *sz))
    }

    pub fn layout(&self) -> Vec<(WindowId, ScreenRect)> {
        self.windows
            .keys()
            .filter_map(|id| self.screen_rect(*id).map(|r| (*id, r)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32) -> ScreenRect {
        ScreenRect {
            x,
            y,
            w: 400,
            h: 300,
        }
    }

    #[test]
    fn adopt_is_identity_at_home() {
        let mut c = Canvas::new((0, 0), (1920, 1080));
        c.adopt(WindowId(1), r(100, 200));
        assert_eq!(c.screen_rect(WindowId(1)), Some(r(100, 200)));
    }

    #[test]
    fn pan_right_moves_windows_left_and_back() {
        let mut c = Canvas::new((0, 0), (1920, 1080));
        c.adopt(WindowId(1), r(100, 200));
        c.pan_px(100.0, 0.0);
        assert_eq!(c.screen_rect(WindowId(1)).unwrap().x, 0);
        c.pan_px(-100.0, 50.0);
        let s = c.screen_rect(WindowId(1)).unwrap();
        assert_eq!((s.x, s.y), (100, 150));
        assert_eq!(c.home_rect(WindowId(1)), Some(r(100, 200)));
    }

    #[test]
    fn monitor_origin_is_respected() {
        let mut c = Canvas::new((-1920, 0), (1920, 1080));
        c.adopt(WindowId(1), r(-1800, 10));
        c.pan_px(10.0, 0.0);
        assert_eq!(c.screen_rect(WindowId(1)), Some(r(-1810, 10)));
    }

    #[test]
    fn pinned_window_ignores_camera() {
        let mut c = Canvas::new((0, 0), (1920, 1080));
        c.adopt(WindowId(1), r(100, 200));
        c.pin(WindowId(1), r(100, 200));
        c.pan_px(500.0, 500.0);
        assert_eq!(c.screen_rect(WindowId(1)), Some(r(100, 200)));
        assert_eq!(c.navigable().count(), 0);
        // открепление оставляет окно на месте экрана и возвращает его холсту
        c.unpin(WindowId(1));
        assert_eq!(c.screen_rect(WindowId(1)), Some(r(100, 200)));
        c.pan_px(100.0, 0.0);
        assert_eq!(c.screen_rect(WindowId(1)).unwrap().x, 0);
        assert_eq!(c.navigable().count(), 1);
    }

    #[test]
    fn widget_moves_with_camera_but_is_not_navigable() {
        let mut c = Canvas::new((0, 0), (1920, 1080));
        c.adopt(WindowId(1), r(100, 200));
        c.set_widget(WindowId(1), true);
        c.pan_px(100.0, 0.0);
        assert_eq!(c.screen_rect(WindowId(1)).unwrap().x, 0);
        assert!(c.is_widget(WindowId(1)));
        assert_eq!(c.navigable().count(), 0);
        c.set_world_center(WindowId(1), Vec2::new(0.0, 0.0));
        assert_eq!(c.window_center(WindowId(1)), Some(Vec2::new(0.0, 0.0)));
    }

    #[test]
    fn nearest_picks_by_direction_and_skips_pinned() {
        let mut c = Canvas::new((0, 0), (1920, 1080));
        c.adopt(WindowId(1), r(800, 400)); // центр
        c.adopt(WindowId(2), r(1400, 400)); // справа
        c.adopt(WindowId(3), r(2400, 400)); // справа дальше
        c.adopt(WindowId(4), r(800, 1000)); // ниже на экране
        c.adopt(WindowId(5), r(1100, 400)); // справа, ближе всех, но закреплено
        c.pin(WindowId(5), r(1100, 400));
        let from = c.window_center(WindowId(1)).unwrap();
        assert_eq!(
            c.nearest(from, Some(WindowId(1)), Dir::Right),
            Some(WindowId(2))
        );
        assert_eq!(
            c.nearest(from, Some(WindowId(1)), Dir::Down),
            Some(WindowId(4))
        );
        assert_eq!(c.nearest(from, Some(WindowId(1)), Dir::Left), None);
        assert_eq!(c.nearest(from, Some(WindowId(1)), Dir::Up), None);
    }
}
