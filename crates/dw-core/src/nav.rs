//! Навигация: инерция камеры, оценка скорости перетаскивания, MRU-список окон.

use crate::{Vec2, WindowId};

/// Скорость камеры в пикселях мира в секунду, затухает экспоненциально.
#[derive(Debug, Clone, Copy, Default)]
pub struct Inertia {
    vel: Vec2,
}

impl Inertia {
    /// Скорость, ниже которой движение останавливается (пикс/с).
    pub const MIN_SPEED: f64 = 8.0;
    /// Постоянная затухания (1/с): за секунду скорость падает в e^4 ≈ 55 раз.
    pub const FRICTION: f64 = 4.0;
    /// Верхний предел скорости, чтобы резкий рывок не унёс камеру за пределы разумного.
    pub const MAX_SPEED: f64 = 8000.0;

    pub fn fling(&mut self, v: Vec2) {
        let len = (v.x * v.x + v.y * v.y).sqrt();
        let k = if len > Self::MAX_SPEED {
            Self::MAX_SPEED / len
        } else {
            1.0
        };
        self.vel = Vec2::new(v.x * k, v.y * k);
        if self.speed() < Self::MIN_SPEED {
            self.stop();
        }
    }

    pub fn stop(&mut self) {
        self.vel = Vec2::default();
    }

    pub fn active(&self) -> bool {
        self.vel.x != 0.0 || self.vel.y != 0.0
    }

    pub fn speed(&self) -> f64 {
        (self.vel.x * self.vel.x + self.vel.y * self.vel.y).sqrt()
    }

    /// Сдвиг камеры за `dt` секунд; скорость затухает.
    pub fn step(&mut self, dt: f64) -> Vec2 {
        if !self.active() {
            return Vec2::default();
        }
        let d = Vec2::new(self.vel.x * dt, self.vel.y * dt);
        let k = (-Self::FRICTION * dt).exp();
        self.vel = Vec2::new(self.vel.x * k, self.vel.y * k);
        if self.speed() < Self::MIN_SPEED {
            self.stop();
        }
        d
    }
}

/// Скорость по последним отсчётам перетаскивания (окно по времени ~80 мс).
#[derive(Debug, Default)]
pub struct VelocityTracker {
    samples: Vec<(f64, Vec2)>,
}

impl VelocityTracker {
    const WINDOW: f64 = 0.08;

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// `t` — время в секундах, `delta` — смещение камеры в мире с прошлого отсчёта.
    pub fn push(&mut self, t: f64, delta: Vec2) {
        self.samples.push((t, delta));
        self.samples.retain(|(ts, _)| t - ts <= Self::WINDOW);
    }

    /// Скорость на момент `now`. Если рука остановилась (давно не было отсчётов) — ноль.
    pub fn velocity(&self, now: f64) -> Vec2 {
        let (Some(first), Some(last)) = (self.samples.first(), self.samples.last()) else {
            return Vec2::default();
        };
        if now - last.0 > Self::WINDOW {
            return Vec2::default();
        }
        let span = (now - first.0).max(0.016);
        let sum = self
            .samples
            .iter()
            .fold(Vec2::default(), |a, (_, d)| a + *d);
        Vec2::new(sum.x / span, sum.y / span)
    }
}

/// Список окон от последнего использованного к давнему, с курсором для циклического переключения.
#[derive(Debug, Default)]
pub struct Mru {
    order: Vec<WindowId>,
    cursor: usize,
}

impl Mru {
    /// Окно получило фокус не через переключатель: выносим в начало.
    pub fn touch(&mut self, id: WindowId) {
        if self.order.get(self.cursor) == Some(&id) && self.cursor != 0 {
            return; // это окно, на которое мы сами только что переключились
        }
        self.order.retain(|x| *x != id);
        self.order.insert(0, id);
        self.cursor = 0;
    }

    pub fn remove(&mut self, id: WindowId) {
        if let Some(p) = self.order.iter().position(|x| *x == id) {
            self.order.remove(p);
            if self.cursor >= self.order.len() {
                self.cursor = 0;
            } else if p < self.cursor {
                self.cursor -= 1;
            }
        }
    }

    /// Следующее (`forward`) или предыдущее окно в цикле. Порядок не меняется до `commit`.
    pub fn cycle(&mut self, forward: bool) -> Option<WindowId> {
        let n = self.order.len();
        if n < 2 {
            return None;
        }
        self.cursor = if forward {
            (self.cursor + 1) % n
        } else {
            (self.cursor + n - 1) % n
        };
        Some(self.order[self.cursor])
    }

    /// Завершить переключение: выбранное окно становится самым свежим.
    pub fn commit(&mut self) {
        if self.cursor != 0 {
            let id = self.order.remove(self.cursor);
            self.order.insert(0, id);
            self.cursor = 0;
        }
    }

    pub fn first(&self) -> Option<WindowId> {
        self.order.first().copied()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inertia_decays_and_stops() {
        let mut i = Inertia::default();
        i.fling(Vec2::new(1000.0, 0.0));
        let mut total = 0.0;
        for _ in 0..1000 {
            total += i.step(0.016).x;
        }
        assert!(!i.active());
        // интеграл v0*e^{-kt}: v0/k = 250 px
        assert!((total - 250.0).abs() < 15.0, "total={total}");
    }

    #[test]
    fn slow_fling_is_ignored_and_fast_is_clamped() {
        let mut i = Inertia::default();
        i.fling(Vec2::new(1.0, 1.0));
        assert!(!i.active());
        i.fling(Vec2::new(1e6, 0.0));
        assert!((i.speed() - Inertia::MAX_SPEED).abs() < 1e-6);
    }

    #[test]
    fn tracker_velocity_and_stale_hand() {
        let mut t = VelocityTracker::default();
        for k in 0..5 {
            t.push(k as f64 * 0.016, Vec2::new(10.0, 0.0));
        }
        let v = t.velocity(0.064);
        assert!(v.x > 500.0 && v.x < 800.0, "v={v:?}");
        assert_eq!(t.velocity(1.0), Vec2::default());
    }

    #[test]
    fn mru_touch_and_cycle() {
        let (a, b, c) = (WindowId(1), WindowId(2), WindowId(3));
        let mut m = Mru::default();
        m.touch(a);
        m.touch(b);
        m.touch(c); // c, b, a
        assert_eq!(m.cycle(true), Some(b));
        assert_eq!(m.cycle(true), Some(a));
        assert_eq!(m.cycle(true), Some(c));
        assert_eq!(m.cycle(false), Some(a));
        // фокус пришёл на a вследствие переключения — порядок не трогаем
        m.touch(a);
        assert_eq!(m.cycle(true), Some(c));
        m.cycle(false); // a
        m.commit();
        assert_eq!(m.first(), Some(a));
    }

    #[test]
    fn mru_single_window_does_not_cycle_and_remove_keeps_cursor() {
        let mut m = Mru::default();
        m.touch(WindowId(1));
        assert_eq!(m.cycle(true), None);
        m.touch(WindowId(2));
        m.touch(WindowId(3));
        m.cycle(true);
        m.cycle(true); // cursor на 1
        m.remove(WindowId(3));
        assert_eq!(m.len(), 2);
        assert!(m.cycle(true).is_some());
    }
}
