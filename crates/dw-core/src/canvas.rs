//! Холст: окна в мировых координатах и камера. Зум в Этапе 1 всегда 1.0.

use std::collections::HashMap;

use crate::{ScreenRect, Vec2, Viewport, WindowId};

pub struct Canvas {
    pub viewport: Viewport,
    /// Левый верхний угол монитора в экранных координатах (виртуального рабочего стола).
    pub origin: (i32, i32),
    home: Vec2,
    /// Центр и размер окна в мире.
    windows: HashMap<WindowId, (Vec2, (i32, i32))>,
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
    }

    pub fn forget(&mut self, id: WindowId) {
        self.windows.remove(&id);
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
        let (c, sz) = self.windows.get(&id)?;
        Some(self.rect_at(&self.viewport, *c, *sz))
    }

    /// Положение окна при камере в исходной точке (для baseline поздно взятых окон).
    pub fn home_rect(&self, id: WindowId) -> Option<ScreenRect> {
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
}
