//! Глобальный хук клавиатуры (WH_KEYBOARD_LL) в отдельном потоке.
//! Хук только ставит событие в очередь; аварийная клавиша обрабатывается прямо на потоке хука.

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, OnceLock};

use crossbeam_channel::Sender;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Pan(Dir),
    /// Mod+Ctrl+Shift+R. К моменту отправки колбэк паники уже вызван.
    Panic,
}

const MAGIC: usize = 0xD21F7;
/// Неиспользуемый VK: вставляется перед отпусканием Win, чтобы не открылось «Пуск».
const MASK_VK: u16 = 0xE8;

static TX: OnceLock<Sender<Input>> = OnceLock::new();
static ON_PANIC: OnceLock<Arc<dyn Fn() + Send + Sync>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(true);

static WIN: AtomicBool = AtomicBool::new(false);
static CTRL: AtomicBool = AtomicBool::new(false);
static SHIFT: AtomicBool = AtomicBool::new(false);
static SWALLOWED: AtomicBool = AtomicBool::new(false);

/// Включить/выключить обработку привязок (пауза). Аварийная клавиша работает всегда.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, SeqCst);
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

unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let k = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        if k.dwExtraInfo != MAGIC {
            let up = matches!(wp.0 as u32, WM_KEYUP | WM_SYSKEYUP);
            let vk = k.vkCode as u16;
            match VIRTUAL_KEY(vk) {
                VK_LWIN | VK_RWIN => {
                    WIN.store(!up, SeqCst);
                    if !up {
                        SWALLOWED.store(false, SeqCst);
                    } else if SWALLOWED.swap(false, SeqCst) {
                        let i = [kb(MASK_VK, false), kb(MASK_VK, true)];
                        unsafe { SendInput(&i, size_of::<INPUT>() as i32) };
                    }
                }
                VK_LCONTROL | VK_RCONTROL | VK_CONTROL => CTRL.store(!up, SeqCst),
                VK_LSHIFT | VK_RSHIFT | VK_SHIFT => SHIFT.store(!up, SeqCst),
                key if WIN.load(SeqCst) && CTRL.load(SeqCst) => {
                    let panic = SHIFT.load(SeqCst) && key == VK_R;
                    let dir = match key {
                        VK_LEFT => Some(Dir::Left),
                        VK_RIGHT => Some(Dir::Right),
                        VK_UP => Some(Dir::Up),
                        VK_DOWN => Some(Dir::Down),
                        _ => None,
                    };
                    if panic {
                        SWALLOWED.store(true, SeqCst);
                        if !up {
                            if let Some(f) = ON_PANIC.get() {
                                let f = f.clone();
                                std::thread::spawn(move || f());
                            }
                            if let Some(tx) = TX.get() {
                                let _ = tx.send(Input::Panic);
                            }
                        }
                        return LRESULT(1);
                    }
                    if let Some(d) = dir
                        && ENABLED.load(SeqCst)
                    {
                        SWALLOWED.store(true, SeqCst);
                        if !up && let Some(tx) = TX.get() {
                            let _ = tx.send(Input::Pan(d));
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

/// Запустить поток хука. `on_panic` вызывается в отдельном потоке и не зависит от главного цикла.
pub fn spawn(
    tx: Sender<Input>,
    on_panic: Arc<dyn Fn() + Send + Sync>,
) -> std::thread::JoinHandle<()> {
    let _ = TX.set(tx);
    let _ = ON_PANIC.set(on_panic);
    std::thread::spawn(|| unsafe {
        let h = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("не удалось установить хук клавиатуры: {e}");
                return;
            }
        };
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWindowsHookEx(h);
    })
}
