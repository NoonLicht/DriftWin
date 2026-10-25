//! IPC DriftWin: однострочный JSON по именованному каналу `\\.\pipe\driftwin-<SID>`.
//! Один запрос — один ответ (`{"Ok":…}` или `{"Err":"…"}`); `Subscribe` превращает соединение в поток
//! снимков состояния. Канал доступен только текущему пользователю (DACL) и не принимает сетевых клиентов.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
    LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, NAMED_PIPE_MODE, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// Выбор окна: число — стабильный `id`, строка — подстрока `app_id` без учёта регистра.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Selector {
    Id(u64),
    App(String),
}

impl Selector {
    /// Из аргумента командной строки: число — `id`, иначе имя приложения.
    pub fn parse(s: &str) -> Self {
        s.parse()
            .map_or_else(|_| Selector::App(s.to_string()), Selector::Id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// Камера: в точку мира `x`,`y` или (`relative`) сдвиг.
    Camera {
        x: f64,
        y: f64,
        relative: bool,
    },
    /// Масштаб вида (1 — обычный).
    Zoom {
        level: f64,
    },
    Focus {
        window: Selector,
    },
    /// Центр окна в мире (Y вверх) или сдвиг.
    Move {
        window: Selector,
        x: f64,
        y: f64,
        relative: bool,
    },
    Resize {
        window: Selector,
        w: f64,
        h: f64,
    },
    /// Прозрачность окна 0.1..1.0.
    Opacity {
        window: Selector,
        value: f64,
    },
    Pin {
        window: Selector,
    },
    /// Мягко закрыть окно (без заглушки).
    Close {
        window: Selector,
    },
    /// Закрыть приложение и оставить заглушку.
    Suspend {
        window: Selector,
    },
    /// Запустить приложение заглушки.
    Relaunch {
        window: Selector,
    },
    /// Раскладки клавиатуры окна в фокусе: список, следующая/предыдущая или по номеру.
    Layout {
        op: LayoutOp,
    },
    /// Позиция курсора на экране и в мире; с `x`,`y` — переставить курсор.
    Cursor {
        to: Option<(i32, i32)>,
    },
    /// Любое действие из `[keybindings]`, например `"zoom-to-fit"` или `"focus-nearest left"`.
    Action {
        name: String,
    },
    /// Перейти к закладке или (`set`) запомнить текущее место.
    Bookmark {
        n: u8,
        set: bool,
    },
    /// Снимок в PNG: весь холст (мониторы под управлением), все мониторы, один монитор, окно или область.
    Screenshot {
        path: String,
        target: ShotTarget,
    },
    State,
    /// Поток снимков состояния, не чаще 30 в секунду.
    Subscribe,
    DebugCounters,
}

/// Что делать с раскладками клавиатуры.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LayoutOp {
    List,
    Next,
    Prev,
    Set(usize),
}

impl LayoutOp {
    /// Из аргумента командной строки: пусто/`list`, `next`, `prev` или номер.
    pub fn parse(s: Option<&str>) -> Result<Self, String> {
        match s.map(str::trim) {
            None | Some("" | "list") => Ok(Self::List),
            Some("next") => Ok(Self::Next),
            Some("prev") => Ok(Self::Prev),
            Some(n) => n
                .parse()
                .map(Self::Set)
                .map_err(|_| format!("ожидалось list, next, prev или номер раскладки, а не «{n}»")),
        }
    }
}

/// Что снимать.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ShotTarget {
    /// Мониторы под управлением (общий холст).
    Screen,
    /// Весь виртуальный рабочий стол.
    All,
    /// Один монитор по имени (`DISPLAY1`), `primary` или номеру.
    Output(String),
    /// Одно окно (видно и за краем экрана, и под другими).
    Window(Selector),
    /// Область экрана в пикселях виртуального рабочего стола.
    Region { x: i32, y: i32, w: i32, h: i32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Ok(serde_json::Value),
    Err(String),
}

impl Response {
    pub fn ok(v: impl Serialize) -> Self {
        Response::Ok(serde_json::to_value(v).unwrap_or(serde_json::Value::Null))
    }
}

/// Запрос к демону и канал для ответа.
pub struct Call {
    pub req: Request,
    pub reply: Sender<Response>,
}

/// Последний снимок состояния для подписчиков (демон обновляет, потоки соединений читают).
#[derive(Default)]
pub struct Bus {
    inner: Mutex<(u64, String)>,
}

impl Bus {
    pub fn publish(&self, json: String) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.1 != json {
            g.0 += 1;
            g.1 = json;
        }
    }

    fn latest(&self) -> (u64, String) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Строка SID текущего пользователя.
fn user_sid() -> Option<String> {
    unsafe {
        let mut tok = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok).ok()?;
        let mut need = 0u32;
        let _ = GetTokenInformation(tok, TokenUser, None, 0, &mut need);
        let mut buf = vec![0u8; need as usize];
        let ok = GetTokenInformation(
            tok,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            need,
            &mut need,
        )
        .is_ok();
        let _ = CloseHandle(tok);
        if !ok {
            return None;
        }
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(tu.User.Sid, &mut s).ok()?;
        let out = s.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(s.0.cast())));
        out
    }
}

/// Имя канала: `DRIFTWIN_PIPE` из окружения или `\\.\pipe\driftwin-<SID>`.
pub fn pipe_name() -> String {
    if let Ok(n) = std::env::var("DRIFTWIN_PIPE")
        && !n.is_empty()
    {
        return n;
    }
    format!(
        r"\\.\pipe\driftwin-{}",
        user_sid().unwrap_or_else(|| "user".into())
    )
}

/// Запустить сервер в фоновом потоке. Ошибка — канал уже занят другим процессом или нет прав.
pub fn serve(tx: Sender<Call>, bus: Arc<Bus>) -> Result<(), String> {
    let name = pipe_name();
    let sid = user_sid().ok_or("не удалось определить SID пользователя")?;
    // доступ только владельцу; без наследования
    let sddl = HSTRING::from(format!("D:P(A;;GA;;;{sid})"));
    let first = create_instance(&name, &sddl, true)?;
    let (n2, s2) = (name.clone(), sddl.clone());
    let first = first.0 as usize;
    std::thread::Builder::new()
        .name("driftwin-ipc".into())
        .spawn(move || {
            let mut next = Some(HANDLE(first as *mut _));
            loop {
                let h = match next.take() {
                    Some(h) => h,
                    None => match create_instance(&n2, &s2, false) {
                        Ok(h) => h,
                        Err(e) => {
                            tracing_log(&format!("IPC: канал не создан: {e}"));
                            std::thread::sleep(Duration::from_secs(1));
                            continue;
                        }
                    },
                };
                let connected = unsafe {
                    ConnectNamedPipe(h, None).is_ok() || GetLastError() == ERROR_PIPE_CONNECTED
                };
                if !connected {
                    unsafe {
                        let _ = CloseHandle(h);
                    }
                    continue;
                }
                let (tx, bus) = (tx.clone(), bus.clone());
                let hv = h.0 as usize;
                let _ = std::thread::Builder::new()
                    .name("driftwin-ipc-conn".into())
                    .spawn(move || {
                        let f = unsafe { File::from_raw_handle(hv as RawHandle) };
                        connection(f, &tx, &bus);
                    });
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn tracing_log(s: &str) {
    eprintln!("{s}");
}

fn create_instance(name: &str, sddl: &HSTRING, first: bool) -> Result<HANDLE, String> {
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .map_err(|e| e.to_string())?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let mut open = PIPE_ACCESS_DUPLEX;
        if first {
            open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let h = CreateNamedPipeW(
            &HSTRING::from(name),
            open,
            NAMED_PIPE_MODE(
                PIPE_TYPE_BYTE.0
                    | PIPE_READMODE_BYTE.0
                    | PIPE_WAIT.0
                    | PIPE_REJECT_REMOTE_CLIENTS.0,
            ),
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            Some(&sa),
        );
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        if h == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(h)
    }
}

const MAX_LINE: u64 = 1 << 20;

fn connection(mut f: File, tx: &Sender<Call>, bus: &Bus) {
    let mut line = String::new();
    {
        let mut rd = BufReader::new((&f).take(MAX_LINE));
        if rd.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
    }
    let resp = match serde_json::from_str::<Request>(line.trim()) {
        Err(e) => Response::Err(format!("запрос не разобран: {e}")),
        Ok(Request::Subscribe) => {
            subscribe_loop(&mut f, bus);
            return;
        }
        Ok(req) => {
            let (rtx, rrx) = crossbeam_channel::bounded(1);
            if tx.send(Call { req, reply: rtx }).is_err() {
                Response::Err("демон завершается".into())
            } else {
                rrx.recv_timeout(Duration::from_secs(60))
                    .unwrap_or_else(|_| Response::Err("демон не ответил за 60 с".into()))
            }
        }
    };
    let mut out =
        serde_json::to_string(&resp).unwrap_or_else(|_| r#"{"Err":"внутренняя ошибка"}"#.into());
    out.push('\n');
    let _ = f.write_all(out.as_bytes());
    unsafe {
        let _ = FlushFileBuffers(HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(&f)));
    }
}

fn subscribe_loop(f: &mut File, bus: &Bus) {
    let mut seen = 0u64;
    loop {
        let (seq, json) = bus.latest();
        if seq != seen && !json.is_empty() {
            seen = seq;
            let line = format!("{{\"Ok\":{json}}}\n");
            if f.write_all(line.as_bytes()).is_err() {
                return;
            }
        }
        // не чаще 30 снимков в секунду
        std::thread::sleep(Duration::from_millis(34));
    }
}

// ---------- клиент ----------

fn open_pipe() -> std::io::Result<File> {
    let name = pipe_name();
    let mut last = None;
    for _ in 0..20 {
        match OpenOptions::new().read(true).write(true).open(&name) {
            Ok(f) => return Ok(f),
            // все экземпляры заняты — подождать освобождения
            Err(e) if e.raw_os_error() == Some(231) => {
                std::thread::sleep(Duration::from_millis(50));
                last = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("канал занят")))
}

fn send(f: &mut File, req: &Request) -> std::io::Result<()> {
    let mut s = serde_json::to_string(req).map_err(std::io::Error::other)?;
    s.push('\n');
    f.write_all(s.as_bytes())
}

/// Отправить запрос и дождаться ответа.
pub fn call(req: &Request) -> std::io::Result<Response> {
    let mut f = open_pipe()?;
    send(&mut f, req)?;
    let mut line = String::new();
    BufReader::new(f).read_line(&mut line)?;
    serde_json::from_str(line.trim()).map_err(std::io::Error::other)
}

/// Подписка: `on_line` получает каждый снимок (одна строка JSON); `false` — закончить.
pub fn subscribe(mut on_line: impl FnMut(&str) -> bool) -> std::io::Result<()> {
    let mut f = open_pipe()?;
    send(&mut f, &Request::Subscribe)?;
    let rd = BufReader::new(f);
    for l in rd.lines() {
        if !on_line(&l?) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_op_parses() {
        assert_eq!(LayoutOp::parse(None), Ok(LayoutOp::List));
        assert_eq!(LayoutOp::parse(Some("next")), Ok(LayoutOp::Next));
        assert_eq!(LayoutOp::parse(Some("prev")), Ok(LayoutOp::Prev));
        assert_eq!(LayoutOp::parse(Some("2")), Ok(LayoutOp::Set(2)));
        assert!(LayoutOp::parse(Some("bogus")).is_err());
    }

    #[test]
    fn selector_parses_number_or_app() {
        assert_eq!(Selector::parse("42"), Selector::Id(42));
        assert_eq!(Selector::parse("notepad"), Selector::App("notepad".into()));
    }

    #[test]
    fn request_json_roundtrip() {
        for r in [
            Request::State,
            Request::Camera {
                x: 1.0,
                y: -2.0,
                relative: true,
            },
            Request::Focus {
                window: Selector::Id(7),
            },
            Request::Close {
                window: Selector::App("note".into()),
            },
            Request::Cursor { to: Some((1, 2)) },
        ] {
            let s = serde_json::to_string(&r).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), r, "{s}");
        }
        assert!(serde_json::from_str::<Request>(r#"{"Nope":1}"#).is_err());
        assert_eq!(
            serde_json::from_str::<Request>(r#""State""#).unwrap(),
            Request::State
        );
    }

    #[test]
    fn response_shape() {
        assert_eq!(
            serde_json::to_string(&Response::Err("x".into())).unwrap(),
            r#"{"Err":"x"}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::ok(5)).unwrap(),
            r#"{"Ok":5}"#
        );
    }

    #[test]
    fn bus_bumps_only_on_change() {
        let b = Bus::default();
        b.publish("a".into());
        let s1 = b.latest().0;
        b.publish("a".into());
        assert_eq!(b.latest().0, s1);
        b.publish("b".into());
        assert_eq!(b.latest().0, s1 + 1);
    }

    #[test]
    fn pipe_name_has_sid() {
        // без переопределения имя содержит SID пользователя
        if std::env::var("DRIFTWIN_PIPE").is_err() {
            assert!(pipe_name().starts_with(r"\\.\pipe\driftwin-S-1-"));
        }
    }
}
