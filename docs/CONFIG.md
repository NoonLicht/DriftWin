# Конфигурация DriftWin (Этапы 3–4)

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
| `[navigation]` | `pan_step`, `camera_speed`, `mouse_speed`, `auto_navigate`, `bookmarks`, `nudge_step` (20), `resize_step` (20) |
| `[navigation.edge_pan]` | `zone` (100), `speed_min` (4), `speed_max` (10), `cursor_pan` (false), `cursor_zone` (20), `latency_ms` (120) |
| `[snap]` | `enabled`, `gap` (12), `gap_step` (4; `Win+]` / `Win+[` меняют зазор на лету до перезагрузки конфига, стоящие кластеры пересобираются под новый зазор, действия `snap-gap-increase` / `snap-gap-decrease`), `outer_gap` (12), `distance` (24), `break_force` (32), `corners` (true), `centers` (false) |
| `[zoom]` | только `fit_padding` (80) — отступ при `fit-window`; остальные ключи пока не работают |
| `[hot_corners]` | `threshold` (4), `top_left`, `top_right`, `bottom_left`, `bottom_right` = любое действие; основной монитор (по мониторам — с Этапом 6) |
| `[bindings]` | `disable_defaults = ["keys", "mouse"]` |
| `[keybindings]` | `"комбинация" = "действие"`, `"none"` отключает стандартную привязку |
| `[mouse.anywhere]` | `"комбинация+кнопка" = "действие"`; кнопки `left`/`right`/`middle`, нужен Win или Alt. Перетаскивание: `pan-viewport`, `move-window`, `move-snapped-windows`, `resize-window`, `resize-window-snapped`; по нажатию — любое обычное действие (окно под курсором), `none` отключает |
| `[[window_rules]]` | критерии `app_id` (exe), `class`, `title` (точно / glob / `/regex/`, без учёта регистра); поля `widget`, `pinned_to_screen`, `position`, `size` |

Правила применяются все подходящие по порядку: скаляры (`position`, `size`) — last-wins, булевы (`widget`,
`pinned_to_screen`) — sticky-on. Эффекты применяются при появлении окна; при перезагрузке конфига для уже открытых окон
пересматриваются только `widget`/`pinned_to_screen`.
`size` меняет размер видимого кадра; исходный размер сохраняется в `baseline.json` и возвращается при выходе и при аварийном возврате.

### Действия
`pan-viewport <dir>`, `focus-nearest <dir>`, `center-nearest <dir>`, `cycle-windows forward|backward`, `center-window`,
`focus-center`, `home-toggle`, `go-to-bookmark <1..9>`, `set-bookmark <1..9>`, `toggle-pin-to-screen`, `close-window`,
`reload-config`, `quit`, `none`. Направления: `up`, `down`, `left`, `right` (диагонали — позже).

Этап 4 (над окном в фокусе, при нажатии мышью — над окном под курсором):
`nudge-window <dir>` (на `nudge_step`), `move-to-snap <dir>` (к следующей точке привязки), `grow-window <dir>` /
`shrink-window <dir>` (сторона на `resize_step`, кластер сохраняется), `resize-to-snap <dir>` (сторона до соседа),
`fit-window` (под вид с отступом `fit_padding`), `fit-window-snapped` (то же, соседи сдвигаются), `fill-window`
(занять свободное место до соседей и краёв вида; повтор возвращает), `toggle-fullscreen` (окно на весь монитор;
выход — повтором или любым действием с холстом), `toggle-cursor-pan`. Повторный `fit-window*` тоже возвращает прежнее
состояние, пока окно не трогали.

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
| `mod+shift+стрелки` | `move-to-snap` (только при `mod_key="super"`) |
| `mod+f` | `toggle-fullscreen` |
| `mod+m` / `mod+shift+m` | `fit-window-snapped` / `fit-window` |
| `mod+shift+f` | `fill-window` |
| `mod+ctrl` + ЛКМ-перетаскивание | панорама с инерцией |
| `Alt` + ЛКМ-перетаскивание / `Alt+Shift` | переместить окно / окно вместе с кластером |
| `Alt` + ПКМ-перетаскивание / `Alt+Shift` | изменить размер, соседи сдвигаются (кластер сохраняется) / только это окно |
| `Alt` + СКМ / `Alt+Shift` + СКМ | `fit-window-snapped` / `fit-window` |
| `mod` + СКМ | `fill-window` |

При `mod_key="alt"` буквенные привязки получают дополнительный Ctrl (`mod+ctrl+f`, `mod+ctrl+m`, …), `fill-window` на
мыши — `mod+ctrl`+СКМ, а `move-to-snap` по умолчанию не назначен.

## Привязка и кластеры
- Во время перетаскивания окно притягивается к краям соседей (зазор `gap`, срабатывает на расстоянии `distance`);
  `corners` выравнивает края стоящих рядом окон, `centers` — центры. Чтобы отлипнуть, надо отвести окно больше
  чем на `break_force`. Привязка никогда не загоняет окно в другое окно.
- Кластер — цепочка окон, стоящих вплотную (с зазором `gap` ± 4 px и общим участком границы).
- Изменение размера тянет ближайший к курсору угол; в режиме кластера окна, присоединённые с этой стороны,
  сдвигаются на столько же.
- У края экрана при перетаскивании окна камера прокручивается (`edge_pan`), окно остаётся под курсором.
- Если приложение не приняло размер (минимум, фиксированный размер), через ~0,35 с берётся фактический.
- Исходный размер окна, которому менялся размер, сохраняется в `baseline.json` и возвращается при выходе.

## Принято, но пока не работает
`autostart`; `[zoom]` (кроме `fit_padding`), `[decorations]`, `[effects]`, `[gestures]`, `[touch]`, `[outputs]`, `[session]`,
`[background]`; в `[navigation]` — `trackpad_speed`, `touch_speed`, `drift`, `auto_navigate_on_*`, `nudge_step`,
`anchors`; `[outputs]` и горячие углы по мониторам; в `[windows]` — `elevated="notify"`, `park_limit`, `on_exit="bring_to_view"`,
`disable_when_fullscreen`; в правилах — `fullscreen`, `focus_on_open`, `suspend_on_close`, `restore_windows`,
`preserve_aspect_ratio`, `decoration`, `blur`, `opacity`, `border_*`, `corner_radius`, `shadow`, `output`, `pass_keys`,
`pass_mouse`, `layer_order`; действия `exec*`, `spawn`, зум (`zoom-*`, `zoom-to-fit*`), `suspend-window`, `move-to-bookmark`, `send-to-output` и др. (§2.12 плана).
`toggle-fullscreen` растягивает окно на монитор, но стиль окна (рамку, заголовок) не меняет.
Мышь: `[mouse.on-window]`/`on-canvas`, `resize_on_border`, `decoration_*`.
Закреплённое окно пока не поднимается «поверх» остальных, а виджет не опускается «под» них.
Перезагрузка по mtime вместо `notify`.

## Флаги командной строки (дополняют конфиг)
`--only exe,…` (белый список), `--all` (явно все окна), `--config путь`, `--check-config`, `--state-dir путь`,
`--pinned exe,…` (закрепить на экране), `--widget exe,…` (виджеты), `--no-auto-navigate`.


## Окно настроек

Значок DriftWin в трее: правый клик открывает меню, пункт «Настройки…» запускает `driftwin-settings.exe`
(лежит рядом с `driftwin.exe`; можно запустить и вручную: `driftwin-settings.exe [--config путь]`).

- **Общие** — все числовые, логические и выбираемые ключи конфига с диапазонами и подсказками; «Сбросить» убирает ключ из файла.
- **Клавиши** и **Мышь** — все действующие привязки (стандартные и свои): изменить комбинацию и действие, отключить,
  вернуть стандартную, добавить новую; клавишу можно записать нажатием (Win система окну не передаёт, поэтому `mod`/`Win` — кнопки).
- **Прочее** — правила окон, исключения и закладки (пока правятся в файле), кнопка «Открыть в Блокноте».

Сохранение проверяет файл тем же разбором, что и демон; при ошибках файл не меняется. Комментарии и порядок ключей
сохраняются (правка через `toml_edit`). Запущенный DriftWin подхватывает файл сам.
