//! Первый запуск: краткая памятка по клавишам и безопасности. Тот же текст печатает `driftwin --onboarding`.

use std::path::Path;

pub(crate) const TEXT: &str = "\
DriftWin — бесконечный холст для окон.

Двигать холст:   Win+Ctrl+стрелки, или Win+Ctrl и перетаскивание мышью
Двигать окно:    Alt + левая кнопка мыши (с кластером — Alt+Shift)
Менять размер:   Alt + правая кнопка мыши
Обзор холста:    Win+W (повтор — вернуться), колесо мыши меняет масштаб
К закладке:      Win+Ctrl+1..9 (поставить — с Shift)
Заглушка:        Win+Shift+Q закрывает приложение и оставляет заглушку; клик по ней запускает снова
Настройки:       значок в области уведомлений → «Настройки…»

Если что-то пошло не так: Win+Ctrl+Shift+R возвращает все окна на экран.
Выход из программы тоже возвращает окна на места.

Окна, где DriftWin ничего не меняет: приложения с повышенными правами (если сам не запущен от администратора),
игры в эксклюзивном полноэкранном режиме. Список исключений — в [windows] exclude конфига.";

const MARKER: &str = "onboarding.done";

/// Показать памятку один раз для каталога состояния. Диалог в отдельном потоке: он не задерживает запуск.
pub(crate) fn first_run(state_dir: &Path) {
    let marker = state_dir.join(MARKER);
    if marker.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(state_dir);
    if std::fs::write(&marker, "1").is_err() {
        return;
    }
    std::thread::spawn(|| {
        use windows::Win32::UI::WindowsAndMessaging::{
            MB_ICONINFORMATION, MB_OK, MB_TOPMOST, MessageBoxW,
        };
        use windows::core::HSTRING;
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(TEXT),
                &HSTRING::from("DriftWin: первый запуск"),
                MB_OK | MB_ICONINFORMATION | MB_TOPMOST,
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_mentions_emergency_key() {
        assert!(TEXT.contains("Win+Ctrl+Shift+R"));
    }

    #[test]
    fn marker_is_written_once() {
        let d = std::env::temp_dir().join(format!("dw-onb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(MARKER), "1").unwrap();
        // маркер уже есть — поток с диалогом не создаётся, повторная запись не нужна
        first_run(&d);
        assert_eq!(std::fs::read_to_string(d.join(MARKER)).unwrap(), "1");
        let _ = std::fs::remove_dir_all(&d);
    }
}
