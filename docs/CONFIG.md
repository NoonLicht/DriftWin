# Конфигурация DriftWin (Этап 3)

Файл: `%APPDATA%\DriftWin\config.toml` (или `--config путь`). Нет файла — стандартные настройки.
Проверка: `driftwin --check-config [--config путь]` (код выхода 0/1; ошибки с номером строки и столбца).
Горячая перезагрузка: демон раз в секунду сравнивает время изменения файла; также пункт трея «Перечитать конфиг» и
действие `reload-config`. Ошибочный конфиг не применяется, остаётся прежний.
Пример: [`config.example.toml`](../config.example.toml).

## Валидация
- Неизвестный ключ — предупреждение; неверное значение или диапазон — ошибка.
- Ключи из плана, которые ещё не реализованы, принимаются, но при явном задании дают предупреждение
  «пока не поддерживается».
- Комбинации без Win и без пары Ctrl+Alt отклоняются (иначе перехватывались бы Ctrl+C, Alt+F4 и т.п.);
  `Ctrl+Alt+Del` и `Win+L` отклоняются как системные. Клавиши сопоставляются по виртуальным кодам (раскладка не важна).

## Реализовано
| Раздел | Ключи |
|---|---|
| общее | `mod_key` (`super`/`alt`) |
| `[windows]` | `only`, `exclude` (`exe`, `class`), `max_fps`, `panic_key` (`on_exit="restore_baseline"`) |
| `[navigation]` | `pan_step`, `camera_speed`, `mouse_speed`, `auto_navigate`, `bookmarks` |
| `[bindings]` | `disable_defaults = ["keys", "mouse"]` |
| `[keybindings]` | `"комбинация" = "действие"`, `"none"` отключает стандартную привязку |
| `[mouse.anywhere]` | `"mod+ctrl+left" = "pan-viewport"` (перетаскивание холста) |
| `[[window_rules]]` | критерии `app_id` (exe), `class`, `title` (точно / glob / `/regex/`, без учёта регистра); поля `widget`, `pinned_to_screen`, `position`, `size` |

Правила применяются все подходящие по порядку: скаляры (`position`, `size`) — last-wins, булевы (`widget`,
`pinned_to_screen`) — sticky-on. Эффекты применяются при появлении окна; при перезагрузке конфига для уже открытых окон
пересматриваются только `widget`/`pinned_to_screen`.
`size` меняет размер видимого кадра; исходный размер сохраняется в `baseline.json` и возвращается при выходе и при аварийном возврате.

### Действия
`pan-viewport <dir>`, `focus-nearest <dir>`, `center-nearest <dir>`, `cycle-windows forward|backward`, `center-window`,
`focus-center`, `home-toggle`, `go-to-bookmark <1..9>`, `set-bookmark <1..9>`, `toggle-pin-to-screen`, `close-window`,
`reload-config`, `quit`, `none`. Направления: `up`, `down`, `left`, `right` (диагонали — позже).

### Стандартные привязки (`mod` = Win)
| Комбинация | Действие |
|---|---|
| `mod+ctrl+стрелки` | `pan-viewport` |
| `mod+alt+стрелки` | `focus-nearest` (при `mod_key="alt"` — `mod+ctrl+shift+стрелки`) |
| `mod+ctrl+tab` / `+shift` | `cycle-windows forward/backward` |
| `mod+ctrl+c` / `mod+ctrl+h` | `center-window` / `home-toggle` |
| `mod+ctrl+t` | `toggle-pin-to-screen` |
| `mod+ctrl+1..9` / `+shift` | `go-to-bookmark` / `set-bookmark` |
| `mod+ctrl+shift+r` | аварийный возврат (`panic_key`) |
| `mod+ctrl` + ЛКМ-перетаскивание | панорама с инерцией |

## Принято, но пока не работает
`autostart`; `[zoom]`, `[snap]`, `[decorations]`, `[effects]`, `[gestures]`, `[touch]`, `[outputs]`, `[session]`,
`[background]`; в `[navigation]` — `trackpad_speed`, `touch_speed`, `drift`, `auto_navigate_on_*`, `nudge_step`,
`resize_step`, `anchors`, `edge_pan`; в `[windows]` — `elevated="notify"`, `park_limit`, `on_exit="bring_to_view"`,
`disable_when_fullscreen`; в правилах — `fullscreen`, `focus_on_open`, `suspend_on_close`, `restore_windows`,
`preserve_aspect_ratio`, `decoration`, `blur`, `opacity`, `border_*`, `corner_radius`, `shadow`, `output`, `pass_keys`,
`pass_mouse`, `layer_order`; действия `exec*`, `spawn`, зум, snap/fit/fill, `suspend-window` и др. (§2.12 плана).
Закреплённое окно пока не поднимается «поверх» остальных, а виджет не опускается «под» них.
Перезагрузка по mtime вместо `notify`.

## Флаги командной строки (дополняют конфиг)
`--only exe,…` (белый список), `--all` (явно все окна), `--config путь`, `--check-config`, `--state-dir путь`,
`--pinned exe,…` (закрепить на экране), `--widget exe,…` (виджеты), `--no-auto-navigate`.
