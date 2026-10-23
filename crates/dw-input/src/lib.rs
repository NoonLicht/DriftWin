//! Глобальные хуки клавиатуры и мыши (WH_KEYBOARD_LL, WH_MOUSE_LL) в отдельном потоке.
//! Хуки только ставят событие в очередь; аварийная клавиша обрабатывается прямо на потоке хука.
//!
//! Привязки (Mod = Win):
//! - Mod+Ctrl+стрелки — сдвиг камеры; Mod+Ctrl+ЛКМ-перетаскивание — панорама с инерцией
//! - Mod+Alt+стрелки — фокус на ближайшее окно в направлении
//! - Mod+Ctrl+Tab / +Shift — переключатель по MRU вперёд/назад
//! - Mod+Ctrl+C — центрировать на окне в фокусе; Mod+Ctrl+H — домой
//! - Mod+Ctrl+1..9 — перейти к закладке; Mod+Ctrl+Shift+1..9 — запомнить закладку
//! - Mod+Ctrl+Shift+R — аварийный возврат окон

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::SeqCst};
use std::sync::{Arc, OnceLock};

use crossbeam_channel::Sender;
pub use dw_core::Dir;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Pan(Dir),
    /// Mod+Ctrl+Shift+R. К моменту отправки колбэк паники уже вызван.
    Panic,
    /// Смещение курсора при перетаскивании холста, пиксели экрана.
    Drag {
        dx: i32,
        dy: i32,
    },
    DragEnd,
    Focus(Dir),
    /// `true` — вперёд по списку MRU.
    Cycle(bool),
    Center,
    Home,
    BookmarkGo(u8),
    BookmarkSet(u8),
}

const MAGIC: usize = 0xD21F7;
/// Неиспользуемый VK: вставляется перед отпусканием Win, чтобы не открылось «Пуск».
const MASK_VK: u16 = 0xE8;

static TX: OnceLock<Sender<Input>> = OnceLock::new();
static ON_PANIC: OnceLock<Arc<dyn Fn() + Send + Sync>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(true);
static KEYLOG: AtomicBool = AtomicBool::new(false);

static WIN: AtomicBool = AtomicBool::new(false);
static CTRL: AtomicBool = AtomicBool::new(false);
static SHIFT: AtomicBool = AtomicBool::new(false);
static ALT: AtomicBool = AtomicBool::new(false);
static SWALLOWED: AtomicBool = AtomicBool::new(false);
/// Alt участвовал в перехваченной комбинации: при отпускании маскируем его, иначе приложение
/// воспримет это как одиночное нажатие Alt и покажет подсказки меню.
static ALT_SWALLOWED: AtomicBool = AtomicBool::new(false);

static DRAG: AtomicBool = AtomicBool::new(false);
static LAST_X: AtomicI32 = AtomicI32::new(0);
static LAST_Y: AtomicI32 = AtomicI32::new(0);

/// Включить/выключить обработку привязок (пауза). Аварийная клавиша работает всегда.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, SeqCst);
}

fn send(i: Input) {
    if let Some(tx) = TX.get() {
        let _ = tx.send(i);
    }
}

/// Привязка клавиши при зажатом Win (без аварийной). Чистая функция — проверяется тестами.
pub fn map_key(vk: u16, ctrl: bool, shift: bool, alt: bool) -> Option<Input> {
    let dir = match VIRTUAL_KEY(vk) {
        VK_LEFT => Some(Dir::Left),
        VK_RIGHT => Some(Dir::Right),
        VK_UP => Some(Dir::Up),
        VK_DOWN => Some(Dir::Down),
        _ => None,
    };
    if ctrl && !alt {
        if let Some(d) = dir {
            return (!shift).then_some(Input::Pan(d));
        }
        if (0x31..=0x39).contains(&vk) {
            let n = (vk - 0x30) as u8;
            return Some(if shift {
                Input::BookmarkSet(n)
            } else {
                Input::BookmarkGo(n)
            });
        }
        return match VIRTUAL_KEY(vk) {
            VK_TAB => Some(Input::Cycle(!shift)),
            VK_C if !shift => Some(Input::Center),
            VK_H if !shift => Some(Input::Home),
            _ => None,
        };
    }
    if alt && !ctrl && !shift {
        return dir.map(Input::Focus);
    }
    None
}

fn kb(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                dwExtraInfo: MAGIC,
                ..Default::default()
            },
        },
    }
}

fn down(vk: VIRTUAL_KEY) -> bool {
    (unsafe { GetAsyncKeyState(vk.0 as i32) } as u16 & 0x8000) != 0
}

unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let k = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        if k.dwExtraInfo != MAGIC {
            let up = matches!(wp.0 as u32, WM_KEYUP | WM_SYSKEYUP);
            let vk = k.vkCode as u16;
            if KEYLOG.load(SeqCst) && (WIN.load(SeqCst) || vk == VK_LWIN.0 || vk == VK_RWIN.0) {
                tracing::info!(
                    "key vk={vk:#x} up={up} win={} ctrl={} shift={} alt={} enabled={}",
                    WIN.load(SeqCst),
                    CTRL.load(SeqCst),
                    SHIFT.load(SeqCst),
                    ALT.load(SeqCst),
                    ENABLED.load(SeqCst)
                );
            }
            match VIRTUAL_KEY(vk) {
                VK_LWIN | VK_RWIN => {
                    WIN.store(!up, SeqCst);
                    if !up {
                        SWALLOWED.store(false, SeqCst);
                        // состояние остальных модификаторов могло «залипнуть» (нажаты до запуска хука)
                        CTRL.store(down(VK_CONTROL), SeqCst);
                        SHIFT.store(down(VK_SHIFT), SeqCst);
                        ALT.store(down(VK_MENU), SeqCst);
                    } else if SWALLOWED.swap(false, SeqCst) {
                        let i = [kb(MASK_VK, false), kb(MASK_VK, true)];
                        unsafe { SendInput(&i, size_of::<INPUT>() as i32) };
                    }
                }
                VK_LCONTROL | VK_RCONTROL | VK_CONTROL => CTRL.store(!up, SeqCst),
                VK_LSHIFT | VK_RSHIFT | VK_SHIFT => SHIFT.store(!up, SeqCst),
                VK_LMENU | VK_RMENU | VK_MENU => {
                    ALT.store(!up, SeqCst);
                    if up && ALT_SWALLOWED.swap(false, SeqCst) {
                        let i = [kb(MASK_VK, false), kb(MASK_VK, true)];
                        unsafe { SendInput(&i, size_of::<INPUT>() as i32) };
                    }
                }
                key if WIN.load(SeqCst) => {
                    let (ctrl, shift, alt) =
                        (CTRL.load(SeqCst), SHIFT.load(SeqCst), ALT.load(SeqCst));
                    if ctrl && shift && !alt && key == VK_R {
                        SWALLOWED.store(true, SeqCst);
                        if !up {
                            if let Some(f) = ON_PANIC.get() {
                                let f = f.clone();
                                std::thread::spawn(move || f());
                            }
                            send(Input::Panic);
                        }
                        return LRESULT(1);
                    }
                    if ENABLED.load(SeqCst)
                        && let Some(i) = map_key(vk, ctrl, shift, alt)
                    {
                        SWALLOWED.store(true, SeqCst);
                        if alt {
                            ALT_SWALLOWED.store(true, SeqCst);
                        }
                        if !up {
                            send(i);
                        }
                        return LRESULT(1);
                    }
                }
                _ => {}
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

fn end_drag() {
    if DRAG.swap(false, SeqCst) {
        send(Input::DragEnd);
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let m = unsafe { &*(lp.0 as *const MSLLHOOKSTRUCT) };
        if m.dwExtraInfo != MAGIC {
            let mods = WIN.load(SeqCst) && CTRL.load(SeqCst);
            match wp.0 as u32 {
                WM_LBUTTONDOWN if mods && ENABLED.load(SeqCst) => {
                    DRAG.store(true, SeqCst);
                    LAST_X.store(m.pt.x, SeqCst);
                    LAST_Y.store(m.pt.y, SeqCst);
                    SWALLOWED.store(true, SeqCst);
                    return LRESULT(1);
                }
                WM_MOUSEMOVE if DRAG.load(SeqCst) => {
                    if mods && ENABLED.load(SeqCst) {
                        let dx = m.pt.x - LAST_X.swap(m.pt.x, SeqCst);
                        let dy = m.pt.y - LAST_Y.swap(m.pt.y, SeqCst);
                        if dx != 0 || dy != 0 {
                            send(Input::Drag { dx, dy });
                        }
                    } else {
                        // модификаторы отпущены раньше кнопки
                        end_drag();
                    }
                }
                WM_LBUTTONUP if DRAG.load(SeqCst) => {
                    end_drag();
                    return LRESULT(1);
                }
                _ => {}
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

/// Запустить поток хуков. `on_panic` вызывается в отдельном потоке и не зависит от главного цикла.
pub fn spawn(
    tx: Sender<Input>,
    on_panic: Arc<dyn Fn() + Send + Sync>,
) -> std::thread::JoinHandle<()> {
    KEYLOG.store(std::env::var_os("DRIFTWIN_KEYLOG").is_some(), SeqCst);
    let _ = TX.set(tx);
    let _ = ON_PANIC.set(on_panic);
    std::thread::spawn(|| unsafe {
        let hk = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("не удалось установить хук клавиатуры: {e}");
                return;
            }
        };
        let hm = match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::error!("не удалось установить хук мыши: {e}");
                None
            }
        };
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if let Some(h) = hm {
            let _ = UnhookWindowsHookEx(h);
        }
        let _ = UnhookWindowsHookEx(hk);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings() {
        assert_eq!(
            map_key(VK_RIGHT.0, true, false, false),
            Some(Input::Pan(Dir::Right))
        );
        assert_eq!(
            map_key(VK_LEFT.0, false, false, true),
            Some(Input::Focus(Dir::Left))
        );
        assert_eq!(
            map_key(VK_TAB.0, true, false, false),
            Some(Input::Cycle(true))
        );
        assert_eq!(
            map_key(VK_TAB.0, true, true, false),
            Some(Input::Cycle(false))
        );
        assert_eq!(map_key(VK_C.0, true, false, false), Some(Input::Center));
        assert_eq!(map_key(VK_H.0, true, false, false), Some(Input::Home));
        assert_eq!(
            map_key(0x33, true, false, false),
            Some(Input::BookmarkGo(3))
        );
        assert_eq!(
            map_key(0x33, true, true, false),
            Some(Input::BookmarkSet(3))
        );
        // без нужных модификаторов — ничего не перехватываем
        assert_eq!(map_key(VK_RIGHT.0, false, false, false), None);
        assert_eq!(map_key(VK_C.0, false, false, false), None);
        assert_eq!(map_key(0x30, true, false, false), None);
    }
}
