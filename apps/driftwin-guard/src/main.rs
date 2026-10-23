//! Страж: ждёт завершения демона; если демон ушёл без маркера `clean_exit`, возвращает окна по baseline.json.
//! Использование: driftwin-guard <pid демона> <каталог состояния>

use std::path::PathBuf;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

fn main() -> anyhow::Result<()> {
    dw_win::init_dpi();
    let mut a = std::env::args().skip(1);
    let pid: u32 = a
        .next()
        .ok_or_else(|| anyhow::anyhow!("нужен pid"))?
        .parse()?;
    let dir = PathBuf::from(
        a.next()
            .ok_or_else(|| anyhow::anyhow!("нужен каталог состояния"))?,
    );

    unsafe {
        let p = OpenProcess(PROCESS_SYNCHRONIZE, false, pid)?;
        WaitForSingleObject(p, INFINITE);
        let _ = CloseHandle(p);
    }
    if dir.join("clean_exit").exists() {
        return Ok(());
    }
    match dw_win::Baseline::load(&dir.join("baseline.json")) {
        Ok(b) => {
            let n = b.restore();
            eprintln!("driftwin-guard: демон завершился без чистого выхода, возвращено окон: {n}");
        }
        Err(e) => eprintln!("driftwin-guard: baseline.json не прочитан: {e}"),
    }
    Ok(())
}
