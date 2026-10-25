//! Бэкенд обзора `capture` (эксперимент): мышь над окном в обзоре пересылается в настоящее окно.
//!
//! Окна на оверлее — живые кадры WGC, а сами окна стоят на своих местах (в том числе за экраном). Точка на оверлее
//! переводится обратно в точку окна (делением на масштаб) и уходит окну сообщением мыши. Клавиатура пересылки не
//! требует: клик отдаёт окну фокус, а оверлей фокус не берёт, поэтому клавиши идут прямо в окно.

use dw_render::Event;
use windows::Win32::UI::WindowsAndMessaging::{
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_RBUTTONUP,
};

use super::*;

/// Что делает зажатая кнопка мыши.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Drag {
    /// Всё, что движется и отпускается, идёт этому окну (даже если курсор ушёл с него).
    Forward(WindowId),
    /// Перетаскивание пустого места: двигает холст.
    Pan { x: i32, y: i32 },
    /// Перетаскивание заголовка: двигает окно по холсту.
    Move { id: WindowId, x: i32, y: i32 },
}

#[derive(Default)]
pub(crate) struct CapState {
    pub hub: Option<dw_render::Hub>,
    pub drag: Option<Drag>,
}

/// Модификатор Ctrl в `wParam` мыши.
const MK_CONTROL: usize = 0x8;

impl App {
    /// Оверлей `capture`; при неудаче (нет D3D11/WGC) вызывающий вернётся к миниатюрам.
    pub(crate) fn capture_overlay(
        &mut self,
        kind: dw_render::Kind,
        rect: (i32, i32, i32, i32),
    ) -> Option<Overlay> {
        if self.ext.cap.hub.is_none() {
            match dw_render::Hub::new() {
                Ok(h) => self.ext.cap.hub = Some(h),
                Err(e) => {
                    tracing::warn!("capture недоступен ({e}), обзор рисуется миниатюрами");
                    return None;
                }
            }
        }
        let hub = self.ext.cap.hub.as_ref()?;
        match Overlay::new_capture(kind, rect, self.bg_cfg(), hub) {
            Ok(o) => Some(o),
            Err(e) => {
                tracing::warn!("оверлей capture не создан ({e}), обзор рисуется миниатюрами");
                None
            }
        }
    }

    fn forward_to(&self, id: WindowId, lx: f64, ly: f64, msg: u32, wp: usize) -> dw_win::Forwarded {
        let Some(v) = self.managed.get(&id).and_then(dw_win::current_visible) else {
            return dw_win::Forwarded::Other;
        };
        dw_win::forward_mouse(id, (v.x, v.y), (lx, ly), msg, wp)
    }

    /// Окно получает фокус по клику; камера при этом стоит на месте.
    fn focus_for_click(&mut self, id: WindowId) {
        if dw_win::foreground() != Some(id)
            && let Some(m) = self.managed.get(&id)
        {
            dw_win::focus(m);
        }
        self.fg_seen = Some(id);
        self.mru.touch(id);
    }

    /// Мышь на оверлее `capture`: `x`, `y` — в координатах общего вида.
    pub(crate) fn capture_mouse(&mut self, msg: u32, x: i32, y: i32, wp: usize) {
        if !self.in_overview() {
            return;
        }
        let scene = self.scene();
        let hit = dw_render::hit_test(&scene, x, y);
        let z = scene.zoom.max(0.02);
        match msg {
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => match hit {
                Some((key, lx, ly)) => {
                    let id = WindowId(key);
                    self.focus_for_click(id);
                    self.ext.cap.drag = match self.forward_to(id, lx, ly, msg, wp) {
                        dw_win::Forwarded::Caption if msg == WM_LBUTTONDOWN => {
                            Some(Drag::Move { id, x, y })
                        }
                        dw_win::Forwarded::Close => None,
                        _ => Some(Drag::Forward(id)),
                    };
                }
                None if msg == WM_LBUTTONDOWN => self.ext.cap.drag = Some(Drag::Pan { x, y }),
                None => {}
            },
            WM_MOUSEMOVE => match self.ext.cap.drag {
                Some(Drag::Forward(id)) => {
                    if let Some((lx, ly)) = dw_render::local_in(&scene, id.0, x, y) {
                        self.forward_to(id, lx, ly, msg, wp);
                    }
                }
                Some(Drag::Pan { x: px, y: py }) => {
                    self.ext.cap.drag = Some(Drag::Pan { x, y });
                    self.overlay_event(Event::Pan {
                        dx: x - px,
                        dy: y - py,
                    });
                }
                Some(Drag::Move { id, x: px, y: py }) => {
                    self.ext.cap.drag = Some(Drag::Move { id, x, y });
                    if let Some(r) = self.canvas.rect(id) {
                        let center = Vec2::new(
                            r.center.x + f64::from(x - px) / z,
                            r.center.y - f64::from(y - py) / z,
                        );
                        self.set_rects(vec![(id, Rect { center, ..r })]);
                    }
                }
                None => {
                    // наведение без нажатия: окно под курсором видит движение мыши
                    if let Some((key, lx, ly)) = hit {
                        self.forward_to(WindowId(key), lx, ly, msg, wp);
                    }
                }
            },
            WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
                let drag = self.ext.cap.drag.take();
                match (drag, hit) {
                    (Some(Drag::Forward(id)), _) => {
                        if let Some((lx, ly)) = dw_render::local_in(&scene, id.0, x, y) {
                            self.forward_to(id, lx, ly, msg, wp);
                        }
                    }
                    // правый клик по пустому месту — выйти из обзора
                    (None, None) if msg == WM_RBUTTONUP => self.leave_overview(),
                    // отпускание над окном, нажатие на котором мы не пересылали (кнопка закрытия)
                    (None, Some((key, lx, ly))) => {
                        self.forward_to(WindowId(key), lx, ly, msg, wp);
                    }
                    _ => {}
                }
            }
            WM_MOUSEWHEEL => {
                let steps = f64::from(((wp >> 16) & 0xFFFF) as u16 as i16) / 120.0;
                match hit {
                    Some((key, lx, ly)) if wp & MK_CONTROL == 0 => {
                        self.forward_to(WindowId(key), lx, ly, msg, wp);
                    }
                    // над пустым местом или с Ctrl — масштаб холста
                    _ => self.overlay_event(Event::Wheel { steps, x, y }),
                }
            }
            _ => {}
        }
    }
}
