//! Действия и привязки в виде простых данных: их собирает `dw-config`, исполняют хуки и демон.

use crate::Dir;

/// Действие, которое можно повесить на клавишу или мышь (подмножество §2.12 плана).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    PanViewport(Dir),
    FocusNearest(Dir),
    /// Подвести камеру к ближайшему окну, не меняя фокус.
    CenterNearest(Dir),
    /// `true` — вперёд по MRU.
    CycleWindows(bool),
    CenterWindow,
    /// Дать фокус окну, ближайшему к центру экрана.
    FocusCenter,
    HomeToggle,
    GoToBookmark(u8),
    SetBookmark(u8),
    TogglePinToScreen,
    CloseWindow,
    ReloadConfig,
    Quit,
    /// Сдвинуть окно на `nudge_step`.
    NudgeWindow(Dir),
    /// Сдвинуть окно к следующей точке привязки.
    MoveToSnap(Dir),
    /// Растянуть сторону окна на `resize_step` (кластер сохраняется).
    GrowWindow(Dir),
    /// Подтянуть сторону окна внутрь на `resize_step`.
    ShrinkWindow(Dir),
    /// Растянуть сторону окна до ближайшего соседа.
    ResizeToSnap(Dir),
    ToggleFullscreen,
    /// Подогнать окно под вид, соседи сдвигаются, кластер сохраняется.
    FitWindowSnapped,
    /// Подогнать окно под вид, соседей не трогать.
    FitWindow,
    /// Занять свободное место вокруг окна; повтор отменяет.
    FillWindow,
    /// Включить/выключить прокрутку вида курсором у края экрана.
    ToggleCursorPan,
    /// Увеличить зазор привязки на `gap_step` (до перезагрузки конфига).
    GapIncrease,
    /// Уменьшить зазор привязки на `gap_step`.
    GapDecrease,
    /// Обзор всех окон; повтор возвращает камеру.
    ZoomToFit,
    /// Обзор кластера окна в фокусе.
    ZoomToFitSnapped,
    ZoomIn,
    ZoomOut,
    /// Вернуть масштаб 1.0 в текущем положении камеры.
    ZoomReset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Непрерывное действие мыши (пока кнопка зажата).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragKind {
    /// Двигать холст.
    Pan,
    /// Двигать окно под курсором.
    MoveWindow,
    /// Двигать окно вместе с кластером.
    MoveCluster,
    /// Менять размер окна, не трогая соседей.
    ResizeWindow,
    /// Менять размер окна, соседи сдвигаются (кластер сохраняется).
    ResizeCluster,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Drag(DragKind),
    /// Разовое действие по нажатию; применяется к окну под курсором.
    Click(Action),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseBinding {
    pub mods: Mods,
    pub button: MouseButton,
    pub action: MouseAction,
}

/// Состояние модификаторов (после раскрытия `mod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub win: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBinding {
    pub mods: Mods,
    /// Виртуальный код клавиши (не зависит от раскладки).
    pub vk: u16,
    pub action: Action,
}

/// Скомпилированные привязки для хука ввода.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    pub keys: Vec<KeyBinding>,
    /// Аварийная клавиша: обрабатывается прямо на потоке хука.
    pub panic: (Mods, u16),
    pub mouse: Vec<MouseBinding>,
}
