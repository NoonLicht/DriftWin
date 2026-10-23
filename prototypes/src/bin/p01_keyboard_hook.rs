//! Гипотезы: WH_KEYBOARD_LL может «проглотить» Win+<клавиша>; без маскировки отпускание Win открывает «Пуск»;
//! вставка фиктивной клавиши (VK 0xE8) подавляет «Пуск»; задержка хука мала.
//! Тест синтетический (SendInput): хук видит injected-события. Реальную клавиатуру не проверяет.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::time::{Duration, Instant};

use dw_prototypes::*;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const MAGIC: usize = 0xD21F7;
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
static SWALLOWED: AtomicBool = AtomicBool::new(false);
static MASK_ON_WIN_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_COUNT: AtomicU64 = AtomicU64::new(0);
static MAX_HOOK_US: AtomicU64 = AtomicU64::new(0);
static SUM_HOOK_US: AtomicU64 = AtomicU64::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
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

fn send(i: &[INPUT]) {
    unsafe {
        SendInput(i, size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let t = Instant::now();
    let mut ret = None;
    if code >= 0 {
        let k = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        let up = matches!(wp.0 as u32, WM_KEYUP | WM_SYSKEYUP);
        let ours = k.dwExtraInfo == MAGIC;
        if ours && k.vkCode == 0xE8 {
            // собственная маскировка — пропускаем дальше
        } else if k.vkCode == VK_LWIN.0 as u32 {
            if !up {
                WIN_DOWN.store(true, SeqCst);
                SWALLOWED.store(false, SeqCst);
            } else {
                WIN_DOWN.store(false, SeqCst);
                if SWALLOWED.load(SeqCst) && MASK_ON_WIN_UP.load(SeqCst) {
                    send(&[key(VIRTUAL_KEY(0xE8), false), key(VIRTUAL_KEY(0xE8), true)]);
                }
            }
        } else if WIN_DOWN.load(SeqCst) && k.vkCode == 'Q' as u32 {
            SWALLOWED.store(true, SeqCst);
            SWALLOW_COUNT.fetch_add(1, SeqCst);
            ret = Some(LRESULT(1));
        }
    }
    let us = t.elapsed().as_micros() as u64;
    MAX_HOOK_US.fetch_max(us, SeqCst);
    SUM_HOOK_US.fetch_add(us, SeqCst);
    CALLS.fetch_add(1, SeqCst);
    ret.unwrap_or_else(|| unsafe { CallNextHookEx(None, code, wp, lp) })
}

fn fg_class() -> String {
    class_name(unsafe { GetForegroundWindow() })
}

fn trial(mask: bool) {
    MASK_ON_WIN_UP.store(mask, SeqCst);
    let before = fg_class();
    // SendInput из потока, владеющего хуком, заблокировался бы — шлём из отдельного потока
    let t = std::thread::spawn(|| {
        send(&[
            key(VK_LWIN, false),
            key(VK_Q, false),
            key(VK_Q, true),
            key(VK_LWIN, true),
        ])
    });
    pump_for(Duration::from_millis(700));
    let _ = t.join();
    let after = fg_class();
    println!("  маскировка={mask:<5} foreground: «{before}» -> «{after}»");
    // закрыть «Пуск», если открылся
    if after != before {
        send(&[key(VK_ESCAPE, false), key(VK_ESCAPE, true)]);
        pump_for(Duration::from_millis(400));
    }
}

fn main() -> anyhow::Result<()> {
    init_dpi();
    let h = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0)? };
    println!("-- Win+Q через SendInput; хук глотает Q:");
    trial(false);
    trial(true);
    println!(
        "  проглочено Q: {} (ожидается 2)",
        SWALLOW_COUNT.load(SeqCst)
    );
    let c = CALLS.load(SeqCst).max(1);
    println!(
        "  время внутри хука: среднее {} мкс, максимум {} мкс за {c} вызовов",
        SUM_HOOK_US.load(SeqCst) / c,
        MAX_HOOK_US.load(SeqCst)
    );
    unsafe {
        let _ = UnhookWindowsHookEx(h);
    }
    Ok(())
}
