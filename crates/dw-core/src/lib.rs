//! Чистая логика DriftWin без Win32: холст, камера, геометрия, трейт `Platform`.

pub mod canvas;
pub mod nav;

use std::ops::{Add, Sub};

/// Стабильный идентификатор окна DriftWin (не HWND).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonitorId(pub u64);

/// Направление на экране.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Точка/вектор. В мире Y направлена вверх.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    fn add(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x - o.x, self.y - o.y)
    }
}

/// Прямоугольник: центр и размер.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub center: Vec2,
    pub w: f64,
    pub h: f64,
}

/// Камера монитора на общем холсте.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub center: Vec2,
    /// Верхняя граница всегда 1.0 (см. README, «Ограничения»).
    pub zoom: f64,
    pub size_px: (u32, u32),
}

impl Viewport {
    /// Мир (Y вверх) -> пиксели экрана монитора (Y вниз), начало — левый верхний угол.
    pub fn world_to_screen(&self, p: Vec2) -> Vec2 {
        let (w, h) = (self.size_px.0 as f64, self.size_px.1 as f64);
        Vec2::new(
            (p.x - self.center.x) * self.zoom + w / 2.0,
            -(p.y - self.center.y) * self.zoom + h / 2.0,
        )
    }

    pub fn screen_to_world(&self, s: Vec2) -> Vec2 {
        let (w, h) = (self.size_px.0 as f64, self.size_px.1 as f64);
        Vec2::new(
            (s.x - w / 2.0) / self.zoom + self.center.x,
            -(s.y - h / 2.0) / self.zoom + self.center.y,
        )
    }
}

/// Реальный прямоугольник на экране в пикселях (видимый кадр).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlatformEvent {
    Created(WindowId),
    Destroyed(WindowId),
    Moved(WindowId, ScreenRect),
    Focused(WindowId),
    Minimized(WindowId, bool),
    TitleChanged(WindowId),
    MonitorsChanged,
}

/// Граница между логикой и ОС. Реализация для Windows — в `dw-win`, для тестов — `FakePlatform`.
pub trait Platform {
    fn enumerate_windows(&self) -> Vec<(WindowId, ScreenRect)>;
    /// Пакетное перемещение (один `DeferWindowPos` на кадр).
    fn apply_layout(&mut self, layout: &[(WindowId, ScreenRect)]);
    fn focus(&mut self, id: WindowId);
    fn close(&mut self, id: WindowId);
    fn poll_events(&mut self) -> Vec<PlatformEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_screen_roundtrip() {
        let vp = Viewport {
            center: Vec2::new(100.0, -50.0),
            zoom: 0.5,
            size_px: (1920, 1080),
        };
        let p = Vec2::new(123.0, 456.0);
        let back = vp.screen_to_world(vp.world_to_screen(p));
        assert!((back.x - p.x).abs() < 1e-9 && (back.y - p.y).abs() < 1e-9);
    }

    #[test]
    fn y_axis_points_up() {
        let vp = Viewport {
            center: Vec2::default(),
            zoom: 1.0,
            size_px: (100, 100),
        };
        // точка выше центра в мире оказывается выше (меньше y) на экране
        assert!(vp.world_to_screen(Vec2::new(0.0, 10.0)).y < 50.0);
    }
}
