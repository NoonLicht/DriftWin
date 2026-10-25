//! «Призраки»: окно, которое частично заходит на неуправляемый монитор, паркуется за экранами,
//! а на управляемом мониторе вместо него показывается живая миниатюра нужной части окна.
//! Так граница выглядит как обрезка, но без области окна (`SetWindowRgn` ломает рамку DWM у многих приложений).

use std::collections::HashMap;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE,
    DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE, DwmRegisterThumbnail, DwmUnregisterThumbnail,
    DwmUpdateThumbnailProperties,
};
use windows::Win32::Graphics::Gdi::{
    BLACK_BRUSH, BeginPaint, EndPaint, FillRect, GetStockObject, HBRUSH, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use crate::{Event, push_event};

/// Одна видимая часть окна.
#[derive(Debug, Clone, PartialEq)]
pub struct GhostItem {
    pub key: u64,
    pub hwnd: isize,
    /// Где показать на экране: (x, y, ширина, высота).
    pub dest: (i32, i32, i32, i32),
    /// Какую часть окна показать, в координатах окна (с невидимыми рамками): (left, top, right, bottom).
    pub src: (i32, i32, i32, i32),
}

struct Ghost {
    hwnd: HWND,
    thumb: isize,
    last: GhostItem,
}

#[derive(Default)]
pub struct Ghosts {
    wins: HashMap<u64, Ghost>,
}

unsafe extern "system" fn ghost_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                FillRect(hdc, &ps.rcPaint, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                let _ = EndPaint(hwnd, &ps);
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let key = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as u64;
            push_event(Event::GhostClick(key));
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn register() {
    unsafe {
        if let Ok(hi) = GetModuleHandleW(None) {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(ghost_proc),
                hInstance: hi.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: w!("DriftWinGhost"),
                ..Default::default()
            };
            RegisterClassExW(&wc);
        }
    }
}

fn props(it: &GhostItem) -> DWM_THUMBNAIL_PROPERTIES {
    DWM_THUMBNAIL_PROPERTIES {
        dwFlags: DWM_TNP_RECTDESTINATION
            | DWM_TNP_RECTSOURCE
            | DWM_TNP_VISIBLE
            | DWM_TNP_OPACITY
            | DWM_TNP_SOURCECLIENTAREAONLY,
        rcDestination: RECT {
            left: 0,
            top: 0,
            right: it.dest.2,
            bottom: it.dest.3,
        },
        rcSource: RECT {
            left: it.src.0,
            top: it.src.1,
            right: it.src.2,
            bottom: it.src.3,
        },
        opacity: 255,
        fVisible: true.into(),
        fSourceClientAreaOnly: false.into(),
    }
}

impl Ghosts {
    pub fn new() -> Self {
        register();
        Self::default()
    }

    fn create(it: &GhostItem) -> Option<Ghost> {
        let hwnd = unsafe {
            let hi = GetModuleHandleW(None).ok()?;
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("DriftWinGhost"),
                w!("DriftWin ghost"),
                WS_POPUP,
                it.dest.0,
                it.dest.1,
                it.dest.2,
                it.dest.3,
                None,
                None,
                Some(hi.into()),
                None,
            )
            .ok()?
        };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, it.key as isize);
            let thumb = match DwmRegisterThumbnail(hwnd, HWND(it.hwnd as *mut _)) {
                Ok(t) => t,
                Err(_) => {
                    let _ = DestroyWindow(hwnd);
                    return None;
                }
            };
            let _ = DwmUpdateThumbnailProperties(thumb, &props(it));
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Some(Ghost {
                hwnd,
                thumb,
                last: it.clone(),
            })
        }
    }

    /// Привести набор призраков к `items`: лишние убрать, недостающие создать, изменившиеся сдвинуть.
    pub fn update(&mut self, items: &[GhostItem]) {
        let gone: Vec<u64> = self
            .wins
            .keys()
            .copied()
            .filter(|k| !items.iter().any(|i| i.key == *k))
            .collect();
        for k in gone {
            if let Some(g) = self.wins.remove(&k) {
                Self::destroy(g);
            }
        }
        for it in items {
            match self.wins.get_mut(&it.key) {
                Some(g) if g.last.hwnd != it.hwnd => {
                    if let Some(old) = self.wins.remove(&it.key) {
                        Self::destroy(old);
                    }
                    if let Some(n) = Self::create(it) {
                        self.wins.insert(it.key, n);
                    }
                }
                Some(g) => {
                    if g.last != *it {
                        unsafe {
                            let _ = SetWindowPos(
                                g.hwnd,
                                None,
                                it.dest.0,
                                it.dest.1,
                                it.dest.2,
                                it.dest.3,
                                SWP_NOACTIVATE | SWP_NOZORDER,
                            );
                            let _ = DwmUpdateThumbnailProperties(g.thumb, &props(it));
                        }
                        g.last = it.clone();
                    }
                }
                None => {
                    if let Some(n) = Self::create(it) {
                        self.wins.insert(it.key, n);
                    }
                }
            }
        }
    }

    fn destroy(g: Ghost) {
        unsafe {
            let _ = DwmUnregisterThumbnail(g.thumb);
            let _ = DestroyWindow(g.hwnd);
        }
    }

    pub fn clear(&mut self) {
        for (_, g) in self.wins.drain() {
            Self::destroy(g);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.wins.is_empty()
    }
}

impl Drop for Ghosts {
    fn drop(&mut self) {
        self.clear();
    }
}
