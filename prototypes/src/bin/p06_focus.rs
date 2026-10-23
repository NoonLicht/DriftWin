//! Гипотеза: из фонового процесса SetForegroundWindow не срабатывает; обход через Alt-нажатие или
//! AttachThreadInput срабатывает. Окна — собственные. Внимание: на мгновение забирает фокус у пользователя.

use std::time::Duration;

use dw_prototypes::*;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

fn fg_is(h: HWND) -> bool {
    unsafe { GetForegroundWindow() == h }
}

fn alt_tap() {
    unsafe {
        keybd_event(VK_MENU.0 as u8, 0, KEYBD_EVENT_FLAGS(0), 0);
        keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
    }
}

fn attach_trick(h: HWND) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        let ft = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        let att = AttachThreadInput(me, ft, true).as_bool();
        let _ = BringWindowToTop(h);
        let r = SetForegroundWindow(h).as_bool();
        if att {
            let _ = AttachThreadInput(me, ft, false);
        }
        r
    }
}

/// Режим «чужого» foreground-окна: отдельный процесс, который забирает фокус и держит его.
fn holder() {
    let h = create_test_window_ex(
        WS_EX_TOOLWINDOW,
        "dw-focus HOLDER",
        900,
        200,
        300,
        200,
        0x00_40_C0_40,
    );
    alt_tap();
    unsafe {
        let _ = SetForegroundWindow(h);
    }
    println!("{}", h.0 as usize);
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    run_message_loop();
}

fn spawn_holder() -> (std::process::Child, HWND) {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let mut c = Command::new(std::env::current_exe().unwrap())
        .arg("--holder")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(c.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    (c, HWND(line.trim().parse::<usize>().unwrap() as _))
}

fn main() {
    init_dpi();
    if std::env::args().nth(1).as_deref() == Some("--holder") {
        holder();
        return;
    }
    let a = create_test_window_ex(
        WS_EX_TOOLWINDOW,
        "dw-focus A",
        200,
        200,
        300,
        200,
        0x00_40_40_C0,
    );
    let b = create_test_window_ex(
        WS_EX_TOOLWINDOW,
        "dw-focus B",
        520,
        200,
        300,
        200,
        0x00_C0_40_40,
    );
    pump_for(Duration::from_millis(300));

    // каждый тест начинается со свежего процесса-держателя фокуса (чужой foreground)
    let (mut h1, hw1) = spawn_holder();
    println!("-- чужой foreground установлен: {}", fg_is(hw1));
    let r1 = unsafe { SetForegroundWindow(a).as_bool() };
    pump_for(Duration::from_millis(300));
    println!(
        "  1) обычный SetForegroundWindow(A): вернул {r1}, A в фокусе: {}",
        fg_is(a)
    );
    let _ = h1.kill();

    let (mut h2, hw2) = spawn_holder();
    println!("-- чужой foreground установлен: {}", fg_is(hw2));
    alt_tap();
    let r2 = unsafe { SetForegroundWindow(b).as_bool() };
    pump_for(Duration::from_millis(300));
    println!(
        "  2) Alt-нажатие + SetForegroundWindow(B): вернул {r2}, B в фокусе: {}",
        fg_is(b)
    );
    let _ = h2.kill();

    let (mut h3, hw3) = spawn_holder();
    println!("-- чужой foreground установлен: {}", fg_is(hw3));
    let r3 = attach_trick(a);
    pump_for(Duration::from_millis(300));
    println!(
        "  3) AttachThreadInput + SetForegroundWindow(A): вернул {r3}, A в фокусе: {}",
        fg_is(a)
    );
    let _ = h3.kill();
    unsafe {
        let _ = DestroyWindow(a);
        let _ = DestroyWindow(b);
    }
}
