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
    /// Модификаторы перетаскивания холста левой кнопкой мыши.
    pub drag_pan: Option<Mods>,
}
