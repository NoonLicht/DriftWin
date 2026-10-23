//! Гипотезы: (1) DeferWindowPos на 20/50/100 окнах другого процесса укладывается в бюджет кадра;
//! (2) поведение окон на больших координатах (±30000 и далее); (3) невидимые рамки: GetWindowRect vs EXTENDED_FRAME_BOUNDS.
//! Двигает только окна, созданные этим же бинарём (дочерний процесс `--host N`).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use dw_prototypes::*;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::*;

fn host(n: usize) {
    let mut hs = Vec::new();
    for i in 0..n {
        let c = 0x00_20_20_20 + ((i as u32 * 0x03_05_07) & 0x00_7f_7f_7f);
        hs.push(create_test_window(
            &format!("dw-proto {i}"),
            50 + (i as i32 % 10) * 40,
            50 + (i as i32 / 10) * 30,
            300,
            200,
            c,
        ));
    }
    let line: Vec<String> = hs.iter().map(|h| (h.0 as usize).to_string()).collect();
    println!("{}", line.join(","));
    std::io::stdout().flush().unwrap();
    run_message_loop(); // родитель убьёт процесс
}

fn bench(n: usize, async_flag: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .args(["--host", &n.to_string()])
        .stdout(Stdio::piped())
        .spawn()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line)?;
    let hs: Vec<HWND> = line
        .trim()
        .split(',')
        .map(|s| HWND(s.parse::<usize>().unwrap() as _))
        .collect();
    std::thread::sleep(Duration::from_millis(300));

    if n == 20 && !async_flag {
        println!("-- Проба флагов DeferWindowPos (одно окно другого процесса):");
        let base = SWP_NOACTIVATE | SWP_NOZORDER;
        for (name, f) in [
            ("NOACTIVATE|NOZORDER", base),
            ("+NOOWNERZORDER", base | SWP_NOOWNERZORDER),
            ("+NOSENDCHANGING", base | SWP_NOSENDCHANGING),
            ("+NOCOPYBITS", base | SWP_NOCOPYBITS),
            ("+ASYNCWINDOWPOS", base | SWP_ASYNCWINDOWPOS),
        ] {
            let r = unsafe {
                let hdwp = BeginDeferWindowPos(1)?;
                match DeferWindowPos(hdwp, hs[0], None, 60, 60, 300, 200, f) {
                    Ok(h2) => EndDeferWindowPos(h2).map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                }
            };
            println!("  {name:<22} -> {r:?}");
        }
    }

    // DeferWindowPos отвергает SWP_NOSENDCHANGING и SWP_ASYNCWINDOWPOS (0x80070057) — см. пробу выше.
    // Режим async_flag = цикл SetWindowPos с SWP_ASYNCWINDOWPOS|NOSENDCHANGING (не пакетный).
    let flags = SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOCOPYBITS;
    let aflags = flags | SWP_ASYNCWINDOWPOS | SWP_NOSENDCHANGING;
    let mut times = Vec::new();
    for frame in 0..200 {
        let dx = (frame % 100) * 6;
        let t = Instant::now();
        unsafe {
            if async_flag {
                for (i, h) in hs.iter().enumerate() {
                    let _ = SetWindowPos(
                        *h,
                        None,
                        50 + (i as i32 % 10) * 40 + dx,
                        50 + (i as i32 / 10) * 30,
                        300,
                        200,
                        aflags,
                    );
                }
            } else {
                let mut hdwp = BeginDeferWindowPos(hs.len() as i32)?;
                for (i, h) in hs.iter().enumerate() {
                    hdwp = DeferWindowPos(
                        hdwp,
                        *h,
                        None,
                        50 + (i as i32 % 10) * 40 + dx,
                        50 + (i as i32 / 10) * 30,
                        300,
                        200,
                        flags,
                    )
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "DeferWindowPos i={i} hwnd={:?} valid={} flags={:#x}: {e}",
                            h,
                            IsWindow(Some(*h)).as_bool(),
                            flags.0
                        )
                    })?;
                }
                EndDeferWindowPos(hdwp)?;
            }
        }
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        std::thread::sleep(Duration::from_millis(8));
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    println!(
        "  N={n:<3} {:<22} avg={avg:6.2}мс p50={:6.2} p95={:6.2} max={:6.2}",
        if async_flag {
            "SetWindowPos+ASYNC"
        } else {
            "DeferWindowPos (пакет)"
        },
        times[times.len() / 2],
        times[times.len() * 95 / 100],
        times[times.len() - 1]
    );
    let _ = child.kill();
    Ok(())
}

fn offscreen() {
    println!("-- Большие координаты (собственное окно, X меняется, Y=100):");
    let h = create_test_window("dw-proto far", 100, 100, 300, 200, 0x00_80_40_20);
    for x in [
        20_000, 30_000, 32_000, 32_767, 40_000, 100_000, -20_000, -30_000, -32_000, -32_768,
        -40_000,
    ] {
        unsafe {
            let ok = SetWindowPos(h, None, x, 100, 300, 200, SWP_NOACTIVATE | SWP_NOZORDER).is_ok();
            let r = window_rect(h);
            let visible = IsWindowVisible(h).as_bool();
            println!(
                "  запрошено x={x:>7} ok={ok} -> left={:>7} top={} видимо={visible}",
                r.left, r.top
            );
        }
    }
    unsafe {
        let _ = DestroyWindow(h);
    }
}

fn frame_bounds() {
    println!("-- Невидимые рамки (собственное окно 300x200 при x=100,y=100):");
    let h = create_test_window("dw-proto frame", 100, 100, 300, 200, 0x00_20_80_40);
    pump_for(Duration::from_millis(200));
    let wr = window_rect(h);
    let mut fb = RECT::default();
    unsafe {
        let _ = DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut fb as *mut _ as *mut _,
            size_of::<RECT>() as u32,
        );
        let _ = DestroyWindow(h);
    }
    println!(
        "  GetWindowRect         = L{} T{} R{} B{}",
        wr.left, wr.top, wr.right, wr.bottom
    );
    println!(
        "  EXTENDED_FRAME_BOUNDS = L{} T{} R{} B{}",
        fb.left, fb.top, fb.right, fb.bottom
    );
    println!(
        "  невидимая рамка: слева {} справа {} сверху {} снизу {}",
        fb.left - wr.left,
        wr.right - fb.right,
        fb.top - wr.top,
        wr.bottom - fb.bottom
    );
}

fn main() -> anyhow::Result<()> {
    init_dpi();
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--host") {
        host(args[2].parse()?);
        return Ok(());
    }
    println!(
        "-- DeferWindowPos, окна в другом процессе (бюджет: 60 fps = 16.7мс; цель <4мс/50 окон):"
    );
    for n in [20, 50, 100] {
        bench(n, false)?;
        bench(n, true)?;
    }
    offscreen();
    frame_bounds();
    Ok(())
}
