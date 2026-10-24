//! Оверлей обзора (живые миниатюры DWM) и фон холста, рисуемый средствами GDI.
//!
//! Два вида окна: `Overview` — поверх всех, показывает миниатюры окон при масштабе меньше 1;
//! `Desktop` — внизу z-порядка, рисует фон под обычными окнами. Окна не берут фокус.
//! Все вызовы — из потока, который качает сообщения (в демоне это главный поток).

mod paint;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use dw_core::{Rect, Vec2};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION,
    DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE, DwmRegisterThumbnail, DwmUnregisterThumbnail,
    DwmUpdateThumbnailProperties,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, ScreenToClient, UpdateWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

/// Фон холста.
#[derive(Debug, Clone, PartialEq)]
pub enum BgKind {
    /// Ничего не рисовать (на рабочем столе — окно фона не создаётся).
    None,
    Solid,
    Grid,
    /// Картинка, повторяющаяся по всей плоскости.
    Tile,
    /// Одна картинка в начале координат мира.
    Wallpaper,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BgCfg {
    pub kind: BgKind,
    pub color: [u8; 3],
    pub grid_color: [u8; 3],
    /// Шаг сетки в мире, пикселей.
    pub grid_size: f64,
    pub image: Option<PathBuf>,
}

impl Default for BgCfg {
    fn default() -> Self {
        Self {
            kind: BgKind::Grid,
            color: [24, 26, 32],
            grid_color: [48, 52, 64],
            grid_size: 100.0,
            image: None,
        }
    }
}

/// Окно на сцене обзора. `rect` — видимый кадр в мире (Y вверх), `border` — невидимые рамки (l, t, r, b), px.
#[derive(Debug, Clone)]
pub struct Item {
    pub key: u64,
    pub hwnd: isize,
    pub rect: Rect,
    pub border: (i32, i32, i32, i32),
    pub focused: bool,
    pub title: String,
}

#[derive(Debug, Clone, Default)]
pub struct Scene {
    pub center: Vec2,
    pub zoom: f64,
    pub items: Vec<Item>,
}

/// События мыши на оверлее обзора.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Клик по миниатюре окна.
    Click(u64),
    /// Перетаскивание пустого места (смещение курсора, px).
    Pan { dx: i32, dy: i32 },
    /// Колесо: число «щелчков» (вверх положительно) и позиция курсора в окне.
    Wheel { steps: f64, x: i32, y: i32 },
    /// Правый клик: выйти из обзора.
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Overview,
    Desktop,
}

struct Shared {
    hwnd: isize,
    kind: Kind,
    size: (i32, i32),
    scene: Scene,
    bg: BgCfg,
    image: Option<paint::Image>,
    /// Последняя позиция курсора при перетаскивании и было ли заметное смещение.
    drag: Option<(i32, i32, bool)>,
}

static STATE: Mutex<Vec<Shared>> = Mutex::new(Vec::new());
static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());

/// События, накопленные с прошлого вызова.
pub fn drain_events() -> Vec<Event> {
    EVENTS
        .lock()
        .map(|mut e| std::mem::take(&mut *e))
        .unwrap_or_default()
}

fn push_event(e: Event) {
    if let Ok(mut q) = EVENTS.lock() {
        q.push(e);
    }
}

fn with_state<R>(hwnd: HWND, f: impl FnOnce(&mut Shared) -> R) -> Option<R> {
    let mut st = STATE.lock().ok()?;
    st.iter_mut().find(|s| s.hwnd == hwnd.0 as isize).map(f)
}

/// Экранный прямоугольник видимого кадра окна относительно оверлея.
fn screen_rect(scene: &Scene, size: (i32, i32), r: &Rect) -> RECT {
    let z = scene.zoom;
    let (hw, hh) = (size.0 as f64 / 2.0, size.1 as f64 / 2.0);
    let left = (r.center.x - r.w / 2.0 - scene.center.x) * z + hw;
    let right = (r.center.x + r.w / 2.0 - scene.center.x) * z + hw;
    let top = -(r.center.y + r.h / 2.0 - scene.center.y) * z + hh;
    let bottom = -(r.center.y - r.h / 2.0 - scene.center.y) * z + hh;
    RECT {
        left: left.round() as i32,
        top: top.round() as i32,
        right: right.round() as i32,
        bottom: bottom.round() as i32,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                with_state(hwnd, |s| paint::paint(hdc, s));
                let _ = EndPaint(hwnd, &ps);
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_MOUSEMOVE | WM_LBUTTONUP | WM_RBUTTONUP | WM_MOUSEWHEEL => {
            let (x, y) = (
                (lp.0 & 0xFFFF) as i16 as i32,
                ((lp.0 >> 16) & 0xFFFF) as i16 as i32,
            );
            let is_overview = with_state(hwnd, |s| s.kind == Kind::Overview).unwrap_or(false);
            if is_overview {
                mouse(hwnd, msg, wp, x, y);
                return LRESULT(0);
            }
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn mouse(hwnd: HWND, msg: u32, wp: WPARAM, x: i32, y: i32) {
    match msg {
        WM_LBUTTONDOWN => {
            unsafe { SetCapture(hwnd) };
            with_state(hwnd, |s| s.drag = Some((x, y, false)));
        }
        WM_MOUSEMOVE => {
            let ev = with_state(hwnd, |s| {
                let (lx, ly, moved) = s.drag?;
                let (dx, dy) = (x - lx, y - ly);
                if !moved && dx.abs() + dy.abs() < 5 {
                    return None;
                }
                s.drag = Some((x, y, true));
                Some(Event::Pan { dx, dy })
            })
            .flatten();
            if let Some(e) = ev {
                push_event(e);
            }
        }
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            let ev = with_state(hwnd, |s| {
                let d = s.drag.take()?;
                if d.2 {
                    return None;
                }
                s.scene
                    .items
                    .iter()
                    .rev()
                    .find(|it| {
                        let r = screen_rect(&s.scene, s.size, &it.rect);
                        x >= r.left && x < r.right && y >= r.top && y < r.bottom
                    })
                    .map(|it| Event::Click(it.key))
            })
            .flatten();
            if let Some(e) = ev {
                push_event(e);
            }
        }
        WM_RBUTTONUP => push_event(Event::Back),
        WM_MOUSEWHEEL => {
            // координаты колеса — экранные
            let mut p = POINT { x, y };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut p);
            }
            let steps = ((wp.0 >> 16) & 0xFFFF) as i16 as f64 / 120.0;
            push_event(Event::Wheel {
                steps,
                x: p.x,
                y: p.y,
            });
        }
        _ => {}
    }
}

fn register_class() -> windows::core::Result<()> {
    unsafe {
        let hi = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hi.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: w!("DriftWinOverlay"),
            ..Default::default()
        };
        // повторная регистрация вернёт 0 — это нормально
        RegisterClassExW(&wc);
    }
    Ok(())
}

struct Thumb {
    handle: isize,
    last: Option<(RECT, bool)>,
}

pub struct Overlay {
    hwnd: HWND,
    kind: Kind,
    rect: (i32, i32, i32, i32),
    thumbs: HashMap<u64, Thumb>,
    visible: bool,
}

impl Overlay {
    /// Окно на мониторе `rect` = (x, y, ширина, высота) в экранных координатах.
    pub fn new(kind: Kind, rect: (i32, i32, i32, i32), bg: BgCfg) -> Result<Self, String> {
        register_class().map_err(|e| e.to_string())?;
        let ex = match kind {
            Kind::Overview => WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            Kind::Desktop => WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
        };
        let hwnd = unsafe {
            let hi = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            CreateWindowExW(
                ex,
                w!("DriftWinOverlay"),
                w!("DriftWin"),
                WS_POPUP,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                None,
                None,
                Some(hi.into()),
                None,
            )
            .map_err(|e| e.to_string())?
        };
        let image = paint::load_image(&bg);
        if let Ok(mut st) = STATE.lock() {
            st.push(Shared {
                hwnd: hwnd.0 as isize,
                kind,
                size: (rect.2, rect.3),
                scene: Scene {
                    zoom: 1.0,
                    ..Default::default()
                },
                bg,
                image,
                drag: None,
            });
        }
        Ok(Self {
            hwnd,
            kind,
            rect,
            thumbs: HashMap::new(),
            visible: false,
        })
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn set_bg(&mut self, bg: BgCfg) {
        let hwnd = self.hwnd;
        with_state(hwnd, |s| {
            if s.bg != bg {
                s.image = paint::load_image(&bg);
                s.bg = bg;
            }
        });
        if self.visible {
            self.repaint();
        }
    }

    pub fn show(&mut self) {
        let after = match self.kind {
            Kind::Overview => HWND_TOPMOST,
            Kind::Desktop => HWND_BOTTOM,
        };
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(after),
                self.rect.0,
                self.rect.1,
                self.rect.2,
                self.rect.3,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        self.visible = true;
        self.repaint();
    }

    pub fn hide(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.visible = false;
        for (_, t) in self.thumbs.drain() {
            unsafe {
                let _ = DwmUnregisterThumbnail(t.handle);
            }
        }
    }

    fn repaint(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            let _ = UpdateWindow(self.hwnd);
        }
    }

    /// Новая сцена: обновить миниатюры и перерисовать фон.
    pub fn update(&mut self, scene: Scene) {
        if self.kind == Kind::Overview {
            self.sync_thumbs(&scene);
        }
        let hwnd = self.hwnd;
        with_state(hwnd, |s| s.scene = scene);
        if self.visible {
            self.repaint();
        }
    }

    fn sync_thumbs(&mut self, scene: &Scene) {
        let size = (self.rect.2, self.rect.3);
        let gone: Vec<u64> = self
            .thumbs
            .keys()
            .copied()
            .filter(|k| !scene.items.iter().any(|i| i.key == *k))
            .collect();
        for k in gone {
            if let Some(t) = self.thumbs.remove(&k) {
                unsafe {
                    let _ = DwmUnregisterThumbnail(t.handle);
                }
            }
        }
        let z = scene.zoom;
        for it in &scene.items {
            if !self.thumbs.contains_key(&it.key) {
                let reg = unsafe { DwmRegisterThumbnail(self.hwnd, HWND(it.hwnd as *mut _)).ok() };
                match reg {
                    Some(h) => {
                        self.thumbs.insert(
                            it.key,
                            Thumb {
                                handle: h,
                                last: None,
                            },
                        );
                    }
                    None => continue,
                }
            }
            let vis = screen_rect(scene, size, &it.rect);
            let (bl, bt, br, bb) = it.border;
            let dest = RECT {
                left: vis.left - (bl as f64 * z).round() as i32,
                top: vis.top - (bt as f64 * z).round() as i32,
                right: vis.right + (br as f64 * z).round() as i32,
                bottom: vis.bottom + (bb as f64 * z).round() as i32,
            };
            let shown = dest.right > 0
                && dest.bottom > 0
                && dest.left < size.0
                && dest.top < size.1
                && dest.right > dest.left
                && dest.bottom > dest.top;
            let Some(t) = self.thumbs.get_mut(&it.key) else {
                continue;
            };
            let unchanged = t
                .last
                .is_some_and(|(r, v)| v == shown && (!shown || r == dest));
            if unchanged {
                continue;
            }
            t.last = Some((dest, shown));
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION
                    | DWM_TNP_VISIBLE
                    | DWM_TNP_OPACITY
                    | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: dest,
                opacity: 255,
                fVisible: shown.into(),
                fSourceClientAreaOnly: false.into(),
                ..Default::default()
            };
            unsafe {
                let _ = DwmUpdateThumbnailProperties(t.handle, &props);
            }
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        for (_, t) in self.thumbs.drain() {
            unsafe {
                let _ = DwmUnregisterThumbnail(t.handle);
            }
        }
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
        if let Ok(mut st) = STATE.lock() {
            st.retain(|s| s.hwnd != self.hwnd.0 as isize);
        }
    }
}
