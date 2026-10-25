//! Плавное движение камеры и масштаба: критически демпфированная пружина вместо «доли пути за кадр».
//!
//! У экспоненты скорость максимальна в самом первом кадре, поэтому переход к соседнему окну выглядел как телепорт.
//! Пружина разгоняется с нуля и мягко останавливается без перелёта; время перехода растёт с расстоянием.

use dw_core::Vec2;

/// Один шаг критически демпфированной пружины (форма SmoothDamp): без перелёта и с нулевой начальной скоростью.
fn damp(cur: f64, target: f64, vel: &mut f64, smooth: f64, dt: f64) -> f64 {
    let omega = 2.0 / smooth.max(1e-3);
    let x = omega * dt;
    let k = 1.0 / (1.0 + x + 0.48 * x * x + 0.235 * x * x * x);
    let change = cur - target;
    let temp = (*vel + omega * change) * dt;
    *vel = (*vel - omega * temp) * k;
    target + (change + temp) * k
}

/// Время сглаживания камеры, с: из `camera_speed` (доля пути за кадр на 60 Гц) и длины перехода.
pub fn camera_smooth_time(speed: f64, dist: f64) -> f64 {
    let rate = -(1.0 - speed.clamp(0.01, 0.99)).ln() * 60.0;
    let base = 3.5 / rate;
    (base * (1.0 + 0.3 * (1.0 + dist / 800.0).ln())).min(0.45)
}

/// Камера: следует за целью пружиной.
#[derive(Debug, Default)]
pub struct Glide {
    vel: Vec2,
    goal: Vec2,
    smooth: f64,
    /// Куда мы сами поставили камеру в прошлый раз: если она оказалась в другом месте, её двигал кто-то ещё.
    last: Vec2,
}

impl Glide {
    /// Камеру поставили в `at` помимо пружины (инерция, команда): скорость сбрасывается.
    pub fn rest(&mut self, at: Vec2) {
        self.vel = Vec2::default();
        self.last = at;
        self.goal = at;
    }

    pub fn step(&mut self, cur: Vec2, target: Vec2, dt: f64, speed: f64) -> Vec2 {
        if cur != self.last {
            self.vel = Vec2::default();
        }
        if target != self.goal {
            self.goal = target;
            let d = target - cur;
            self.smooth = camera_smooth_time(speed, d.x.hypot(d.y));
        }
        if speed >= 0.99 {
            self.rest(target);
            return target;
        }
        let d = target - cur;
        let next = Vec2::new(
            damp(cur.x, target.x, &mut self.vel.x, self.smooth, dt),
            damp(cur.y, target.y, &mut self.vel.y, self.smooth, dt),
        );
        let out = if d.x.abs() < 1.0
            && d.y.abs() < 1.0
            && self.vel.x.abs() < 30.0
            && self.vel.y.abs() < 30.0
        {
            self.vel = Vec2::default();
            target
        } else {
            next
        };
        self.last = out;
        out
    }
}

/// Масштаб: пружина в логарифмической шкале (одинаково плавно и от 1.0 к 0.5, и от 0.2 к 0.1).
#[derive(Debug, Default)]
pub struct ZoomGlide {
    vel: f64,
    goal: f64,
    smooth: f64,
}

impl ZoomGlide {
    pub fn rest(&mut self) {
        self.vel = 0.0;
    }

    /// Следующий масштаб на шаге `dt`; у цели возвращает её точно.
    pub fn step(&mut self, zoom: f64, target: f64, dt: f64) -> f64 {
        let (a, b) = (zoom.ln(), target.ln());
        if target != self.goal {
            self.goal = target;
            self.smooth = 0.22 + 0.12 * (b - a).abs().min(2.5);
        }
        if (b - a).abs() < 0.002 && self.vel.abs() < 0.02 {
            self.vel = 0.0;
            return target;
        }
        damp(a, b, &mut self.vel, self.smooth, dt).exp()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f64 = 1.0 / 60.0;

    fn run(from: Vec2, to: Vec2) -> Vec<Vec2> {
        let mut g = Glide::default();
        g.rest(from);
        let mut at = from;
        let mut path = vec![at];
        for _ in 0..600 {
            at = g.step(at, to, DT, 0.3);
            path.push(at);
            if at == to {
                break;
            }
        }
        path
    }

    #[test]
    fn camera_starts_from_rest_and_arrives_without_overshoot() {
        let path = run(Vec2::new(0.0, 0.0), Vec2::new(3400.0, 0.0));
        assert_eq!(*path.last().unwrap(), Vec2::new(3400.0, 0.0));
        // первый шаг маленький: никакого скачка на старте (экспонента проходила 30 % сразу)
        assert!(path[1].x < 3400.0 * 0.02, "первый шаг {}", path[1].x);
        assert!(path.iter().all(|p| p.x <= 3400.0 + 1e-6), "перелёт");
        assert!(
            path.windows(2).all(|w| w[1].x >= w[0].x - 1e-9),
            "возврат назад"
        );
    }

    #[test]
    fn longer_trip_takes_longer_but_is_bounded() {
        let near = run(Vec2::new(0.0, 0.0), Vec2::new(200.0, 0.0)).len();
        let far = run(Vec2::new(0.0, 0.0), Vec2::new(8000.0, 0.0)).len();
        assert!(far > near, "{far} <= {near}");
        // не дольше ~1.8 с на 60 Гц даже на очень длинный переход
        assert!(far < 110, "{far} кадров");
    }

    #[test]
    fn external_move_resets_velocity() {
        let mut g = Glide::default();
        g.rest(Vec2::new(0.0, 0.0));
        let mut at = Vec2::new(0.0, 0.0);
        for _ in 0..10 {
            at = g.step(at, Vec2::new(5000.0, 0.0), DT, 0.3);
        }
        // камеру переставили извне в другое место: скорость не должна тащить её дальше по инерции
        let jumped = Vec2::new(-1000.0, 0.0);
        let next = g.step(jumped, Vec2::new(5000.0, 0.0), DT, 0.3);
        assert!(
            next.x - jumped.x < 100.0,
            "скорость не сброшена: {}",
            next.x - jumped.x
        );
    }

    #[test]
    fn zoom_converges_both_ways() {
        for (from, to) in [(1.0, 0.3), (0.3, 1.0), (0.2, 0.1)] {
            let mut z = ZoomGlide::default();
            let mut cur = from;
            let mut prev_gap = f64::MAX;
            for i in 0..400 {
                cur = z.step(cur, to, DT);
                let gap = (cur.ln() - f64::ln(to)).abs();
                assert!(gap <= prev_gap + 1e-9, "кадр {i}: отдаляется от цели");
                prev_gap = gap;
                if cur == to {
                    break;
                }
            }
            assert_eq!(cur, to, "{from} -> {to} не дошёл");
        }
    }
}
