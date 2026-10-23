# Архитектура DriftWin

## Workspace

| Путь | Назначение |
|---|---|
| `crates/dw-core` | Чистая логика без Win32: `Canvas`, `Viewport`, геометрия, трейт `Platform`. Полностью юнит-тестируема |
| `crates/dw-win` | Windows-бэкенд: реализация `Platform` (WinEvent-хуки, пакетное перемещение, фокус) |
| `crates/dw-render` | Оверлей: фон, обзор (DWM Thumbnails), контуры мониторов, заглушки |
| `crates/dw-input` | Хуки клавиатуры/мыши, Raw Input тачпада, распознаватель жестов |
| `crates/dw-ipc` | Протокол, именованный канал `\\.\pipe\driftwin-<SID>` |
| `crates/dw-config` | Схема TOML, валидация, миграции |
| `apps/driftwin` | Демон: трей, главный цикл |
| `apps/driftwinctl` | CLI (`msg`, `--check-config`) |
| `apps/driftwin-guard` | Страж: возвращает окна, если демон упал |
| `apps/driftwin-settings` | GUI настроек (поздний этап) |
| `prototypes/` | Прототипы Этапа 0 (одноразовые) |

## Границы

- `dw-core` ничего не знает про Win32; взаимодействует с ОС только через трейт `Platform` и поток `PlatformEvent`.
  Для тестов будет `FakePlatform`.
- Координаты мира: центр окна/камеры, **Y вверх**. Экран: пиксели, Y вниз.
  `screen = (world − camera_center) * zoom + monitor_center`, с переворотом Y (реализовано в `Viewport`).
- Реальные окна двигаются только при `zoom == 1.0`; при `zoom < 1.0` активен оверлей обзора.

## Потоки (план)

Дирижёр (Win32 message loop) ← каналы `crossbeam-channel` ← потоки хуков ввода и IPC. Хук клавиатуры только ставит
событие в очередь (разовый вызов ~десятки мкс, см. отчёт Этапа 0).
