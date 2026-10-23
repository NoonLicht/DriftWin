//! Глобальные хуки клавиатуры и мыши (WH_KEYBOARD_LL, WH_MOUSE_LL) в отдельном потоке.
//! Хуки только ставят событие в очередь; аварийная клавиша обрабатывается прямо на потоке хука.
//!
//! Привязки приходят из конфига (`dw-config`) в виде `Bindings` и подменяются на лету
//! через `set_bindings`; хук только сопоставляет клавишу с таблицей и ставит событие в очередь.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering::SeqCst};
use std::sync::{Arc, OnceLock, RwLock};

use crossbeam_channel::Sender;
pub use dw_core::Dir;
use dw_core::action::{Action, Bindings, Mods};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Action(Action),
    /// Аварийная клавиша. К моменту отправки колбэк паники уже вызван.
    Panic,
    /// Смещение курсора при перетаскивании холста, пиксели экрана.
    Drag {
        dx: i32,
        dy: i32,
    },
    DragEnd,
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

/// Клавиша последней перехваченной комбинации: её отпускание тоже глотаем, даже если модификаторы уже сняты.
static LAST_SWALLOWED_VK: AtomicU32 = AtomicU32::new(0);

static BINDINGS: OnceLock<RwLock<Bindings>> = OnceLock::new();

fn bindings() -> &'static RwLock<Bindings> {
    BINDINGS.get_or_init(|| {
        RwLock::new(Bindings {
            keys: vec![],
            panic: (
                Mods {
                    win: true,
                    ctrl: true,
                    shift: true,
                    alt: false,
                },
                0x52,
            ),
            drag_pan: None,
        })
    })
}

/// Подменить привязки (при старте и горячей перезагрузке конфига).
pub fn set_bindings(b: Bindings) {
    if let Ok(mut g) = bindings().write() {
        *g = b;
    }
}

fn cur_mods() -> Mods {
    Mods {
        win: WIN.load(SeqCst),
        ctrl: CTRL.load(SeqCst),
        shift: SHIFT.load(SeqCst),
        alt: ALT.load(SeqCst),
    }
}

/// Действие для комбинации (точное совпадение модификаторов).
pub fn lookup(b: &Bindings, mods: Mods, vk: u16) -> Option<Action> {
    b.keys
        .iter()
        .find(|k| k.mods == mods && k.vk == vk)
        .map(|k| k.action)
}

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
                _ => {
                    let mods = cur_mods();
                    let Ok(b) = bindings().read() else {
                        return unsafe { CallNextHookEx(None, code, wp, lp) };
                    };
                    let hit = if (mods, vk) == b.panic {
                        Some(None)
                    } else if ENABLED.load(SeqCst) {
                        lookup(&b, mods, vk).map(Some)
                    } else {
                        None
                    };
                    drop(b);
                    if let Some(act) = hit {
                        if mods.win {
                            SWALLOWED.store(true, SeqCst);
                        }
                        if mods.alt {
                            ALT_SWALLOWED.store(true, SeqCst);
                        }
                        LAST_SWALLOWED_VK.store(vk as u32, SeqCst);
                        if !up {
                            match act {
                                Some(a) => send(Input::Action(a)),
                                None => {
                                    if let Some(f) = ON_PANIC.get() {
                                        let f = f.clone();
                                        std::thread::spawn(move || f());
                                    }
                                    send(Input::Panic);
                                }
                            }
                        }
                        return LRESULT(1);
                    }
                    // отпускание клавиши перехваченной комбинации, когда модификаторы уже сняты
                    if up
                        && LAST_SWALLOWED_VK
                            .compare_exchange(vk as u32, 0, SeqCst, SeqCst)
                            .is_ok()
                    {
                        return LRESULT(1);
                    }
                }
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
            let mods = bindings()
                .read()
                .is_ok_and(|b| b.drag_pan == Some(cur_mods()));
            match wp.0 as u32 {
                WM_LBUTTONDOWN if mods && ENABLED.load(SeqCst) => {
                    DRAG.store(true, SeqCst);
                    LAST_X.store(m.pt.x, SeqCst);
                    LAST_Y.store(m.pt.y, SeqCst);
                    if WIN.load(SeqCst) {
                        SWALLOWED.store(true, SeqCst);
                    }
                    if ALT.load(SeqCst) {
                        ALT_SWALLOWED.store(true, SeqCst);
                    }
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
    use dw_core::action::KeyBinding;

    const WC: Mods = Mods {
        win: true,
        ctrl: true,
        shift: false,
        alt: false,
    };

    fn table() -> Bindings {
        Bindings {
            keys: vec![KeyBinding {
                mods: WC,
                vk: 0x27,
                action: Action::PanViewport(Dir::Right),
            }],
            panic: (WC, 0x52),
            drag_pan: Some(WC),
        }
    }

    #[test]
    fn lookup_requires_exact_modifiers() {
        let b = table();
        assert_eq!(lookup(&b, WC, 0x27), Some(Action::PanViewport(Dir::Right)));
        assert_eq!(lookup(&b, WC, 0x25), None);
        assert_eq!(lookup(&b, Mods { shift: true, ..WC }, 0x27), None);
        assert_eq!(lookup(&b, Mods::default(), 0x27), None);
    }
}
