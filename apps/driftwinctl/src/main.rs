//! `driftwinctl` — управление запущенным демоном DriftWin через IPC.
//!
//! Примеры: `driftwinctl msg state`, `driftwinctl --json msg camera 100 -50`,
//! `driftwinctl msg action zoom-to-fit`, `driftwinctl msg subscribe`.

use clap::{Parser, Subcommand};
use dw_ipc::{Request, Response, Selector};
use serde_json::Value;

#[derive(Parser)]
#[command(name = "driftwinctl", version, about = "Управление демоном DriftWin")]
struct Cli {
    /// Печатать ответ как JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Top,
}

#[derive(Subcommand)]
enum Top {
    /// Отправить команду демону.
    Msg {
        #[command(subcommand)]
        cmd: Msg,
    },
}

#[derive(Subcommand)]
enum Msg {
    /// Переместить камеру в точку мира (или сдвинуть с --relative).
    Camera {
        #[arg(allow_negative_numbers = true)]
        x: f64,
        #[arg(allow_negative_numbers = true)]
        y: f64,
        #[arg(long)]
        relative: bool,
    },
    /// Масштаб вида: 1 — обычный, меньше — обзор.
    Zoom {
        #[arg(allow_negative_numbers = true)]
        level: f64,
    },
    /// Дать фокус окну (число — id, строка — часть имени приложения).
    Focus { window: String },
    /// Поставить центр окна в точку мира (или сдвинуть с --relative).
    Move {
        window: String,
        #[arg(allow_negative_numbers = true)]
        x: f64,
        #[arg(allow_negative_numbers = true)]
        y: f64,
        #[arg(long)]
        relative: bool,
    },
    /// Размер окна в пикселях.
    Resize { window: String, w: f64, h: f64 },
    /// Прозрачность окна 0.1..1.0 (1 — вернуть как было).
    Opacity { window: String, value: f64 },
    /// Закрепить окно на экране или открепить.
    Pin { window: String },
    /// Закрыть окно (без заглушки).
    Close { window: String },
    /// Закрыть приложение и оставить заглушку.
    Suspend { window: String },
    /// Запустить приложение заглушки.
    Relaunch { window: String },
    /// Раскладка клавиатуры (не поддерживается).
    Layout,
    /// Позиция курсора; с двумя числами — переставить курсор.
    Cursor {
        #[arg(requires = "y", allow_negative_numbers = true)]
        x: Option<i32>,
        #[arg(allow_negative_numbers = true)]
        y: Option<i32>,
    },
    /// Выполнить действие, например: action zoom-to-fit, action "focus-nearest left".
    Action {
        #[arg(required = true, num_args = 1..)]
        name: Vec<String>,
    },
    /// Перейти к закладке (или запомнить текущее место с --set).
    Bookmark {
        n: u8,
        #[arg(long)]
        set: bool,
    },
    /// Снимок экрана под управлением в PNG.
    Screenshot { path: String },
    /// Состояние: камера, окна, мониторы.
    State,
    /// Поток изменений состояния (по строке JSON, не чаще 30 в секунду).
    Subscribe,
    /// Счётчики для отладки.
    DebugCounters,
}

fn request(m: Msg) -> Request {
    match m {
        Msg::Camera { x, y, relative } => Request::Camera { x, y, relative },
        Msg::Zoom { level } => Request::Zoom { level },
        Msg::Focus { window } => Request::Focus {
            window: Selector::parse(&window),
        },
        Msg::Move {
            window,
            x,
            y,
            relative,
        } => Request::Move {
            window: Selector::parse(&window),
            x,
            y,
            relative,
        },
        Msg::Resize { window, w, h } => Request::Resize {
            window: Selector::parse(&window),
            w,
            h,
        },
        Msg::Opacity { window, value } => Request::Opacity {
            window: Selector::parse(&window),
            value,
        },
        Msg::Pin { window } => Request::Pin {
            window: Selector::parse(&window),
        },
        Msg::Close { window } => Request::Close {
            window: Selector::parse(&window),
        },
        Msg::Suspend { window } => Request::Suspend {
            window: Selector::parse(&window),
        },
        Msg::Relaunch { window } => Request::Relaunch {
            window: Selector::parse(&window),
        },
        Msg::Layout => Request::Layout,
        Msg::Cursor { x, y } => Request::Cursor { to: x.zip(y) },
        Msg::Action { name } => Request::Action {
            name: name.join(" "),
        },
        Msg::Bookmark { n, set } => Request::Bookmark { n, set },
        Msg::Screenshot { path } => Request::Screenshot { path },
        Msg::State => Request::State,
        Msg::Subscribe => Request::Subscribe,
        Msg::DebugCounters => Request::DebugCounters,
    }
}

/// Человекочитаемый вывод состояния; остальное — компактный JSON.
fn pretty(v: &Value) -> String {
    let f = |p: &Value| {
        p.as_array()
            .map(|a| {
                a.iter()
                    .map(|n| format!("{:.0}", n.as_f64().unwrap_or(0.0)))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    if let (Some(cam), Some(wins)) = (v.get("camera"), v.get("windows").and_then(Value::as_array)) {
        let mut s = format!(
            "камера ({}), масштаб {:.2}, окон {}\n",
            f(cam),
            v["zoom"].as_f64().unwrap_or(1.0),
            wins.len()
        );
        for w in wins {
            s += &format!(
                "  {:>10}  {}{}{}  центр ({})  {}x{}  {}  «{}»\n",
                w["id"],
                if w["is_focused"].as_bool() == Some(true) {
                    "*"
                } else {
                    " "
                },
                if w["suspended"].as_bool() == Some(true) {
                    "z"
                } else {
                    " "
                },
                if w["is_widget"].as_bool() == Some(true) {
                    "w"
                } else {
                    " "
                },
                f(&w["position"]),
                w["size"][0].as_f64().unwrap_or(0.0),
                w["size"][1].as_f64().unwrap_or(0.0),
                w["app_id"].as_str().unwrap_or(""),
                w["title"].as_str().unwrap_or(""),
            );
        }
        return s.trim_end().to_string();
    }
    v.to_string()
}

fn main() {
    let cli = Cli::parse();
    let Top::Msg { cmd } = cli.cmd;
    let req = request(cmd);
    if req == Request::Subscribe {
        let r = dw_ipc::subscribe(|line| {
            println!("{line}");
            true
        });
        if let Err(e) = r {
            eprintln!("driftwinctl: {}", explain(&e));
            std::process::exit(1);
        }
        return;
    }
    match dw_ipc::call(&req) {
        Ok(Response::Ok(v)) => {
            if cli.json {
                println!("{}", serde_json::json!({ "Ok": v }));
            } else if !v.is_null() {
                println!("{}", pretty(&v));
            }
        }
        Ok(Response::Err(e)) => {
            if cli.json {
                println!("{}", serde_json::json!({ "Err": e }));
            } else {
                eprintln!("ошибка: {e}");
            }
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("driftwinctl: {}", explain(&e));
            std::process::exit(1);
        }
    }
}

fn explain(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        format!("демон не запущен (канал {} не найден)", dw_ipc::pipe_name())
    } else {
        e.to_string()
    }
}
