//! Бэкенд обзора `capture` (эксперимент): окна снимаются Windows Graphics Capture, кадры лежат в текстурах D3D11
//! и рисуются на цепочке обмена оверлея. Кадры приходят из потоков WGC, рисует главный поток.
//!
//! WGC отдаёт видимую рамку окна (без невидимых границ), содержимое настоящее и у окна, запаркованного за экраном.
//! Кадры идут только при изменении содержимого, поэтому перерисовка — по счётчику кадров `seq`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2, IDXGISwapChain1,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::core::{Interface, PCSTR, s};

use crate::{BgCfg, BgKind, Item, Scene, screen_rect};

const SHADER: &str = r#"
cbuffer C : register(b0) {
    float4 rect;   // x0, y0, x1, y1 в NDC (y0 — верх)
    float4 color;  // сплошной цвет / цвет фона
    float4 color2; // цвет линий сетки
    float4 par;    // x: режим (0 цвет, 1 текстура, 2 сетка); y, z: начало сетки в пикселях; w: шаг сетки
};
Texture2D tex : register(t0);
SamplerState smp : register(s0);
struct V { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
V vs(uint id : SV_VertexID) {
    float2 c = float2(id & 1, id >> 1);
    V o;
    o.pos = float4(lerp(rect.x, rect.z, c.x), lerp(rect.y, rect.w, c.y), 0, 1);
    o.uv = c;
    return o;
}
float4 ps(V i) : SV_Target {
    if (par.x < 0.5) return color;
    if (par.x < 1.5) return float4(tex.Sample(smp, i.uv).rgb, 1);
    float2 m = fmod(abs(i.pos.xy - par.yz), par.w);
    float2 d = min(m, par.w - m);
    return (d.x < 0.75 || d.y < 0.75) ? color2 : color;
}
"#;

/// Через сколько повторять захват окна, которое не удалось снять.
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// Акцент рамки окна в фокусе.
const FOCUS_RGB: [f32; 4] = [0.36, 0.55, 0.94, 1.0];

struct Frame {
    tex: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    w: i32,
    h: i32,
}

/// Состояние окна, общее с потоком кадров WGC.
struct Shared {
    frame: Mutex<Option<Frame>>,
    /// Размер, под который настроен пул кадров.
    pool_size: Mutex<(i32, i32)>,
}

struct Session {
    shared: Arc<Shared>,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

struct Inner {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    winrt: IDirect3DDevice,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    smp: ID3D11SamplerState,
    cb: ID3D11Buffer,
    sessions: Mutex<HashMap<u64, Session>>,
    /// Окна, которые не удалось снять, и когда пробовали (повтор не чаще раза в 5 с).
    failed: Mutex<HashMap<u64, std::time::Instant>>,
    /// Растёт с каждым принятым кадром.
    seq: Arc<AtomicU64>,
}

/// Устройство D3D11, шейдеры и захваты окон; общие для оверлеев всех мониторов.
#[derive(Clone)]
pub struct Hub(Arc<Inner>);

/// Устройство и контекст для обработчика кадров, который WGC вызывает из своих потоков.
struct GpuHandles {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    winrt: IDirect3DDevice,
}
unsafe impl Send for GpuHandles {}

// интерфейсы D3D11 безопасны между потоками: контекст защищён `SetMultithreadProtected`
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, String> {
    let mut blob = None;
    let mut err = None;
    let r = unsafe {
        D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            None,
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut blob,
            Some(&mut err),
        )
    };
    if let Err(e) = r {
        let msg = err
            .map(|b| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    b.GetBufferPointer().cast::<u8>(),
                    b.GetBufferSize(),
                ))
                .into_owned()
            })
            .unwrap_or_default();
        return Err(format!("шейдер: {e} {msg}"));
    }
    let b = blob.ok_or("шейдер не скомпилирован")?;
    Ok(
        unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
            .to_vec(),
    )
}

impl Hub {
    pub fn new() -> Result<Self, String> {
        Self::new_inner().map_err(|e| e.to_string())
    }

    fn new_inner() -> windows::core::Result<Self> {
        unsafe {
            // уже инициализировано другим режимом — не ошибка
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
            let (mut dev, mut ctx) = (None, None);
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
            let (dev, ctx) = (dev.unwrap(), ctx.unwrap());
            if let Ok(mt) = ctx.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }
            let dxgi: IDXGIDevice = dev.cast()?;
            let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;

            let vs_code = compile(s!("vs"), s!("vs_4_0"))
                .map_err(|m| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, m))?;
            let ps_code = compile(s!("ps"), s!("ps_4_0"))
                .map_err(|m| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, m))?;
            let mut vs = None;
            dev.CreateVertexShader(&vs_code, None, Some(&mut vs))?;
            let mut ps = None;
            dev.CreatePixelShader(&ps_code, None, Some(&mut ps))?;
            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut smp = None;
            dev.CreateSamplerState(&sd, Some(&mut smp))?;
            let bd = D3D11_BUFFER_DESC {
                ByteWidth: 64,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                ..Default::default()
            };
            let mut cb = None;
            dev.CreateBuffer(&bd, None, Some(&mut cb))?;
            Ok(Hub(Arc::new(Inner {
                dev,
                ctx,
                winrt,
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                smp: smp.unwrap(),
                cb: cb.unwrap(),
                sessions: Mutex::new(HashMap::new()),
                failed: Mutex::new(HashMap::new()),
                seq: Arc::new(AtomicU64::new(1)),
            })))
        }
    }

    /// Номер последнего принятого кадра (для решения «пора перерисовать»).
    pub fn seq(&self) -> u64 {
        self.0.seq.load(Ordering::Relaxed)
    }

    /// Привести набор захватов к списку окон: недостающие запустить, лишние остановить.
    pub fn sync(&self, hwnds: &[(u64, isize)]) {
        let Ok(mut map) = self.0.sessions.lock() else {
            return;
        };
        map.retain(|k, _| hwnds.iter().any(|(key, _)| key == k));
        let mut failed = self.0.failed.lock().unwrap_or_else(|e| e.into_inner());
        failed.retain(|k, t| t.elapsed() < RETRY_AFTER && hwnds.iter().any(|(key, _)| key == k));
        for (key, hwnd) in hwnds {
            if map.contains_key(key) || failed.contains_key(key) {
                continue;
            }
            match self.start(HWND(*hwnd as *mut _)) {
                Ok(s) => {
                    map.insert(*key, s);
                }
                Err(e) => {
                    tracing::debug!("capture: окно {key} не снимается: {e}");
                    failed.insert(*key, std::time::Instant::now());
                }
            }
        }
    }

    fn start(&self, hwnd: HWND) -> windows::core::Result<Session> {
        let inner = &self.0;
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd)? };
        let size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &inner.winrt,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let shared = Arc::new(Shared {
            frame: Mutex::new(None),
            pool_size: Mutex::new((size.Width, size.Height)),
        });
        {
            let (shared, seq) = (shared.clone(), inner.seq.clone());
            let gpu = GpuHandles {
                dev: inner.dev.clone(),
                ctx: inner.ctx.clone(),
                winrt: inner.winrt.clone(),
            };
            pool.FrameArrived(&TypedEventHandler::new(
                move |p: windows::core::Ref<Direct3D11CaptureFramePool>, _| {
                    let GpuHandles { dev, ctx, winrt } = &gpu;
                    let Some(pool) = p.as_ref() else {
                        return Ok(());
                    };
                    let Ok(frame) = pool.TryGetNextFrame() else {
                        return Ok(());
                    };
                    let cs = frame.ContentSize()?;
                    let acc = frame.Surface()?.cast::<IDirect3DDxgiInterfaceAccess>()?;
                    let src: ID3D11Texture2D = unsafe { acc.GetInterface()? };
                    if let Ok(mut slot) = shared.frame.lock() {
                        if slot
                            .as_ref()
                            .is_none_or(|f| (f.w, f.h) != (cs.Width, cs.Height))
                        {
                            *slot = make_frame(dev, cs.Width, cs.Height).ok();
                        }
                        if let Some(f) = slot.as_ref() {
                            let b = D3D11_BOX {
                                left: 0,
                                top: 0,
                                front: 0,
                                right: cs.Width as u32,
                                bottom: cs.Height as u32,
                                back: 1,
                            };
                            unsafe {
                                ctx.CopySubresourceRegion(&f.tex, 0, 0, 0, 0, &src, 0, Some(&b));
                                ctx.GenerateMips(&f.srv);
                            }
                            seq.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    // размер окна изменился: пул нужно подстроить, иначе кадры обрезаются
                    if let Ok(mut ps) = shared.pool_size.lock()
                        && *ps != (cs.Width, cs.Height)
                    {
                        *ps = (cs.Width, cs.Height);
                        let _ = pool.Recreate(
                            winrt,
                            DirectXPixelFormat::B8G8R8A8UIntNormalized,
                            2,
                            SizeInt32 {
                                Width: cs.Width,
                                Height: cs.Height,
                            },
                        );
                    }
                    Ok(())
                },
            ))?;
        }
        let session = pool.CreateCaptureSession(&item)?;
        // без жёлтой рамки и курсора; на системах без этих свойств вызовы просто не сработают
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(false);
        session.StartCapture()?;
        Ok(Session {
            shared,
            pool,
            session,
        })
    }

    /// Сколько окон снимается сейчас.
    pub fn active(&self) -> usize {
        self.0.sessions.lock().map_or(0, |m| m.len())
    }

    /// Окна, для которых уже есть хотя бы один кадр.
    pub fn with_frames(&self) -> usize {
        self.0.sessions.lock().map_or(0, |m| {
            m.values()
                .filter(|s| s.shared.frame.lock().is_ok_and(|f| f.is_some()))
                .count()
        })
    }
}

fn make_frame(dev: &ID3D11Device, w: i32, h: i32) -> windows::core::Result<Frame> {
    let d = D3D11_TEXTURE2D_DESC {
        Width: w.max(1) as u32,
        Height: h.max(1) as u32,
        MipLevels: 0,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
    };
    unsafe {
        let mut tex = None;
        dev.CreateTexture2D(&d, None, Some(&mut tex))?;
        let tex = tex.unwrap();
        let mut srv = None;
        dev.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
        Ok(Frame {
            tex,
            srv: srv.unwrap(),
            w,
            h,
        })
    }
}

/// Цепочка обмена одного оверлея.
pub struct Surface {
    hub: Hub,
    swap: IDXGISwapChain1,
    rtv: ID3D11RenderTargetView,
    size: (i32, i32),
    seen: u64,
}

// используется только из потока окна
unsafe impl Send for Surface {}

impl Surface {
    pub fn new(hub: &Hub, hwnd: isize, size: (i32, i32)) -> Result<Self, String> {
        Self::new_inner(hub, HWND(hwnd as *mut _), size).map_err(|e| e.to_string())
    }

    fn new_inner(hub: &Hub, hwnd: HWND, size: (i32, i32)) -> windows::core::Result<Self> {
        let inner = &hub.0;
        unsafe {
            let dxgi: IDXGIDevice = inner.dev.cast()?;
            let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: size.0 as u32,
                Height: size.1 as u32,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_NONE,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let swap = factory.CreateSwapChainForHwnd(&inner.dev, hwnd, &desc, None, None)?;
            let back: ID3D11Texture2D = swap.GetBuffer(0)?;
            let mut rtv = None;
            inner
                .dev
                .CreateRenderTargetView(&back, None, Some(&mut rtv))?;
            Ok(Self {
                hub: hub.clone(),
                swap,
                rtv: rtv.unwrap(),
                size,
                seen: 0,
            })
        }
    }

    /// Пришли ли кадры с прошлой отрисовки.
    pub fn stale(&self) -> bool {
        self.hub.seq() != self.seen
    }

    fn draw(&self, rect: [f32; 4], color: [f32; 4], color2: [f32; 4], par: [f32; 4]) {
        let inner = &self.hub.0;
        let mut data = [0f32; 16];
        data[0..4].copy_from_slice(&rect);
        data[4..8].copy_from_slice(&color);
        data[8..12].copy_from_slice(&color2);
        data[12..16].copy_from_slice(&par);
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if inner
                .ctx
                .Map(&inner.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m))
                .is_ok()
            {
                std::ptr::copy_nonoverlapping(data.as_ptr(), m.pData.cast::<f32>(), 16);
                inner.ctx.Unmap(&inner.cb, 0);
            }
            inner.ctx.Draw(4, 0);
        }
    }

    fn ndc(&self, l: f32, t: f32, r: f32, b: f32) -> [f32; 4] {
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        [
            l / w * 2.0 - 1.0,
            1.0 - t / h * 2.0,
            r / w * 2.0 - 1.0,
            1.0 - b / h * 2.0,
        ]
    }

    /// Нарисовать сцену и показать. `off` — смещение этого оверлея относительно общего вида.
    pub fn render(&mut self, scene: &Scene, off: (i32, i32), bg: &BgCfg) {
        let inner = self.hub.0.clone();
        self.seen = self.hub.seq();
        let c = |v: [u8; 3]| {
            [
                f32::from(v[0]) / 255.0,
                f32::from(v[1]) / 255.0,
                f32::from(v[2]) / 255.0,
                1.0,
            ]
        };
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        unsafe {
            let ctx = &inner.ctx;
            ctx.OMSetRenderTargets(Some(&[Some(self.rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: w,
                Height: h,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.VSSetShader(&inner.vs, None);
            ctx.PSSetShader(&inner.ps, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(inner.cb.clone())]));
            ctx.PSSetConstantBuffers(0, Some(&[Some(inner.cb.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(inner.smp.clone())]));
        }
        // фон: сетка рисуется пиксельным шейдером; картинки пока только в режиме миниатюр
        let full = [-1.0, 1.0, 1.0, -1.0];
        let z = scene.zoom as f32;
        match bg.kind {
            BgKind::Grid if bg.grid_size * scene.zoom >= 6.0 => {
                let step = (bg.grid_size * scene.zoom) as f32;
                // мировая точка (0, 0) в пикселях этого оверлея
                let ox = (-scene.center.x as f32) * z + scene.view.0 as f32 / 2.0 - off.0 as f32;
                let oy = (scene.center.y as f32) * z + scene.view.1 as f32 / 2.0 - off.1 as f32;
                self.draw(full, c(bg.color), c(bg.grid_color), [2.0, ox, oy, step]);
            }
            BgKind::None => self.draw(full, [0.0, 0.0, 0.0, 1.0], [0.0; 4], [0.0; 4]),
            _ => self.draw(full, c(bg.color), [0.0; 4], [0.0; 4]),
        }
        for it in &scene.items {
            self.draw_item(&inner, scene, off, it);
        }
        unsafe {
            let _ = self
                .swap
                .Present(0, windows::Win32::Graphics::Dxgi::DXGI_PRESENT(0));
        }
    }

    fn draw_item(&self, inner: &Inner, scene: &Scene, off: (i32, i32), it: &Item) {
        let r = screen_rect(scene, off, &it.rect);
        let (l, t, rr, b) = (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
        if rr <= 0.0 || b <= 0.0 || l >= self.size.0 as f32 || t >= self.size.1 as f32 {
            return;
        }
        let srv = inner
            .sessions
            .lock()
            .ok()
            .and_then(|m| m.get(&it.key).map(|s| s.shared.clone()))
            .and_then(|sh| {
                sh.frame
                    .lock()
                    .ok()
                    .and_then(|f| f.as_ref().map(|f| f.srv.clone()))
            });
        match srv {
            Some(srv) => unsafe {
                inner.ctx.PSSetShaderResources(0, Some(&[Some(srv)]));
                self.draw(
                    self.ndc(l, t, rr, b),
                    [0.0; 4],
                    [0.0; 4],
                    [1.0, 0.0, 0.0, 0.0],
                );
            },
            // кадра ещё нет или окно не снимается: серая заглушка
            None => self.draw(
                self.ndc(l, t, rr, b),
                [0.2, 0.2, 0.22, 1.0],
                [0.0; 4],
                [0.0; 4],
            ),
        }
        if it.focused {
            let k = 2.0;
            for q in [
                (l - k, t - k, rr + k, t),
                (l - k, b, rr + k, b + k),
                (l - k, t, l, b),
                (rr, t, rr + k, b),
            ] {
                self.draw(self.ndc(q.0, q.1, q.2, q.3), FOCUS_RGB, [0.0; 4], [0.0; 4]);
            }
        }
    }
}
