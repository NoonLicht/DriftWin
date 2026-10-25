//! Прототип Этапа 9: Windows Graphics Capture окна — для бэкенда `capture`.
//! Вопросы: работает ли захват окна без прав администратора; сколько кадров в секунду; снимается ли окно,
//! запаркованное далеко за экраном (как делает холст), и приходит ли в кадре настоящее содержимое.
//!
//! Двигает и снимает только собственное тестовое окно: `fixture.exe 1` должен быть запущен.
//! Использование: `p10_wgc [заголовок окна, по умолчанию "fixture 0"] [onscreen|parked] [секунды]`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowRect, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos,
};
use windows::core::{HSTRING, Interface};

struct Gpu {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
}
// контекст используется только под мьютексом, из потока обработчика кадров
unsafe impl Send for Gpu {}

#[derive(Default)]
struct Stats {
    frames: AtomicU32,
    /// Самая яркая точка и доля непустых (не чёрных) точек последнего прочитанного кадра.
    last: Mutex<Option<(u8, f64, i32, i32)>>,
}

fn read_back(gpu: &Gpu, tex: &ID3D11Texture2D) -> Option<(u8, f64, i32, i32)> {
    unsafe {
        let mut d = D3D11_TEXTURE2D_DESC::default();
        tex.GetDesc(&mut d);
        d.Usage = D3D11_USAGE_STAGING;
        d.BindFlags = 0;
        d.MiscFlags = 0;
        d.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        let mut st = None;
        gpu.dev.CreateTexture2D(&d, None, Some(&mut st)).ok()?;
        let st = st?;
        gpu.ctx.CopyResource(&st, tex);
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m)).ok()?;
        let (w, h) = (d.Width as usize, d.Height as usize);
        let mut max = 0u8;
        let mut lit = 0usize;
        for y in 0..h {
            let row = std::slice::from_raw_parts(
                (m.pData as *const u8).add(y * m.RowPitch as usize),
                w * 4,
            );
            for px in row.as_chunks::<4>().0 {
                let v = px[0].max(px[1]).max(px[2]);
                max = max.max(v);
                if v > 8 {
                    lit += 1;
                }
            }
        }
        gpu.ctx.Unmap(&st, 0);
        Some((max, lit as f64 / (w * h) as f64, w as i32, h as i32))
    }
}

fn main() -> Result<()> {
    let title = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fixture 0".into());
    let parked = std::env::args().nth(2).as_deref() == Some("parked");
    let secs: u64 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    unsafe {
        RoInitialize(RO_INIT_MULTITHREADED)?;
    }
    let hwnd: HWND = unsafe { FindWindowW(None, &HSTRING::from(title.as_str()))? };
    if hwnd.0.is_null() {
        bail!("окно «{title}» не найдено: запустите fixture.exe 1");
    }
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r)? };
    let (x0, y0) = if parked {
        (-20_000, 200)
    } else {
        (r.left, r.top)
    };

    // устройство D3D11 и его обёртка для WinRT
    let (mut dev, mut ctx) = (None, None);
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut dev),
            None,
            Some(&mut ctx),
        )?;
    }
    let (dev, ctx) = (dev.expect("device"), ctx.expect("context"));
    let dxgi: IDXGIDevice = dev.cast()?;
    let winrt: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
    let gpu = Arc::new(Mutex::new(Gpu { dev, ctx }));

    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd)? };
    let size = item.Size()?;
    println!(
        "окно «{title}», размер захвата {}x{}, режим {}",
        size.Width,
        size.Height,
        if parked {
            "parked (x=-20000)"
        } else {
            "onscreen"
        }
    );

    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &winrt,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        size,
    )?;
    let stats = Arc::new(Stats::default());
    {
        let (stats, gpu) = (stats.clone(), gpu.clone());
        pool.FrameArrived(&TypedEventHandler::new(
            move |p: windows::core::Ref<Direct3D11CaptureFramePool>, _| {
                if let Some(pool) = p.as_ref()
                    && let Ok(frame) = pool.TryGetNextFrame()
                {
                    let n = stats.frames.fetch_add(1, Ordering::Relaxed) + 1;
                    if n % 20 == 1
                        && let Ok(acc) = frame.Surface()?.cast::<IDirect3DDxgiInterfaceAccess>()
                        && let Ok(tex) = unsafe { acc.GetInterface::<ID3D11Texture2D>() }
                        && let Ok(g) = gpu.lock()
                    {
                        *stats.last.lock().unwrap() = read_back(&g, &tex);
                    }
                }
                Ok(())
            },
        ))?;
    }
    let session = pool.CreateCaptureSession(&item)?;
    session.StartCapture()?;

    // меняем размер собственного окна на 1-2 px: перемещение само кадров не даёт, перерисовка — даёт
    let (ow, oh) = (r.right - r.left, r.bottom - r.top);
    let t0 = Instant::now();
    let mut i = 0;
    while t0.elapsed() < Duration::from_secs(secs) {
        let dw = if i % 2 == 0 { 0 } else { 2 };
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                x0,
                y0,
                ow + dw,
                oh,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )?;
        }
        i += 1;
        std::thread::sleep(Duration::from_millis(16));
    }
    session.Close()?;
    pool.Close()?;
    // вернуть окно на место
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            r.left,
            r.top,
            ow,
            oh,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )?;
    }
    let n = stats.frames.load(Ordering::Relaxed);
    println!(
        "кадров: {n} за {secs} с ({:.1} к/с), перерисовок окна: {i}",
        f64::from(n) / secs as f64
    );
    match *stats.last.lock().unwrap() {
        Some((max, lit, w, h)) => println!(
            "последний прочитанный кадр {w}x{h}: самая яркая точка {max}/255, непустых точек {:.1}% (белая шапка окна даёт 255)",
            lit * 100.0
        ),
        None => println!("ни один кадр не удалось прочитать"),
    }
    Ok(())
}
