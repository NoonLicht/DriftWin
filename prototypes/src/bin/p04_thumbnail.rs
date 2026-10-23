//! Гипотезы: DwmRegisterThumbnail работает в прозрачном оверлее поверх рабочего стола; обновление dest-rect дёшево;
//! по пикселю экрана можно подтвердить, что миниатюра реально нарисована. Источник — собственное окно.

use std::time::{Duration, Instant};

use dw_prototypes::*;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};
use windows::Win32::UI::WindowsAndMessaging::*;

fn main() -> anyhow::Result<()> {
    init_dpi();
    const KEY: u32 = 0x00_FF_00_FF; // пурпурный = прозрачный
    const SRC_COLOR: u32 = 0x00_30_C0_30; // BGR: ярко-зелёный
    let src = create_test_window("dw-proto src", 100, 100, 400, 300, SRC_COLOR);
    pump_for(Duration::from_millis(300));

    // оверлей: слоистое, click-through, поверх всех, без активации
    let ov = create_test_window_ex(
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
        "dw-proto overlay",
        700,
        100,
        500,
        400,
        KEY,
    );
    unsafe {
        SetWindowLongPtrW(ov, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as isize);
        let _ = SetWindowPos(
            ov,
            Some(HWND_TOPMOST),
            700,
            100,
            500,
            400,
            SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        // чёрный = прозрачный (colorkey), поэтому на оверлее видно только миниатюры
        SetLayeredWindowAttributes(ov, COLORREF(KEY), 0, LWA_COLORKEY)?;
    }
    pump_for(Duration::from_millis(200));

    let thumb = unsafe { DwmRegisterThumbnail(ov, src)? };
    let sz = unsafe { DwmQueryThumbnailSourceSize(thumb)? };
    println!(
        "-- DwmRegisterThumbnail ok; размер источника: {}x{}",
        sz.cx, sz.cy
    );

    let set = |r: RECT, op: u8| unsafe {
        let p = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION
                | DWM_TNP_OPACITY
                | DWM_TNP_VISIBLE
                | DWM_TNP_SOURCECLIENTAREAONLY,
            rcDestination: r,
            opacity: op,
            fVisible: true.into(),
            fSourceClientAreaOnly: false.into(),
            ..Default::default()
        };
        DwmUpdateThumbnailProperties(thumb, &p)
    };
    set(
        RECT {
            left: 50,
            top: 50,
            right: 250,
            bottom: 200,
        },
        255,
    )?;
    pump_for(Duration::from_millis(400));

    // пиксель на экране в центре миниатюры: экранные координаты = origin оверлея (700,100) + (150,125)
    let px = |x: i32, y: i32| unsafe {
        let dc = GetDC(None);
        let c = GetPixel(dc, x, y);
        ReleaseDC(None, dc);
        c.0
    };
    let c = px(700 + 150, 100 + 125);
    println!(
        "  пиксель в центре миниатюры = {c:#08x} (источник {SRC_COLOR:#08x}) -> {}",
        if c == SRC_COLOR {
            "СОВПАЛ: миниатюра нарисована"
        } else {
            "не совпал"
        }
    );
    let outside = px(700 + 400, 100 + 350);
    println!(
        "  пиксель вне миниатюры     = {outside:#08x} (цвет рабочего стола/окна под оверлеем)"
    );

    // анимация зума: 240 обновлений dest-rect
    let mut ts = Vec::new();
    for i in 0..240 {
        let k = 0.4 + 0.6 * ((i as f64 / 240.0) * std::f64::consts::PI).sin().abs();
        let (w, h) = ((400.0 * k) as i32 / 2, (300.0 * k) as i32 / 2);
        let t = Instant::now();
        set(
            RECT {
                left: 50,
                top: 50,
                right: 50 + w,
                bottom: 50 + h,
            },
            255,
        )?;
        ts.push(t.elapsed().as_secs_f64() * 1000.0);
        std::thread::sleep(Duration::from_millis(4));
    }
    ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "  DwmUpdateThumbnailProperties: p50={:.3}мс p95={:.3}мс max={:.3}мс",
        ts[120], ts[228], ts[239]
    );

    // click-through: окно под точкой оверлея должно быть не оверлеем
    let hit = unsafe { WindowFromPoint(windows::Win32::Foundation::POINT { x: 900, y: 300 }) };
    println!(
        "  WindowFromPoint в области оверлея -> «{}» (оверлей прозрачен для кликов, если не наш класс)",
        class_name(hit)
    );

    unsafe {
        let _ = DwmUnregisterThumbnail(thumb);
        let _ = DestroyWindow(ov);
        let _ = DestroyWindow(src);
    }
    Ok(())
}
