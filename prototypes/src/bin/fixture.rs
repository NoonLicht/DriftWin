//! Тестовые окна для безопасной разработки: N цветных окон, exe называется fixture.exe.
//! Использование: fixture [N=4]

use dw_prototypes::*;

fn main() {
    init_dpi();
    let n: i32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    for i in 0..n {
        let c = 0x00_30_30_30 + ((i as u32 * 0x2A_47_61) & 0x00_8F_8F_8F);
        create_test_window_ex(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            &format!("fixture {i}"),
            120 + (i % 3) * 360,
            120 + (i / 3) * 280,
            340,
            240,
            c,
        );
    }
    run_message_loop();
}
