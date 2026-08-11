//! PURE state machine for the window system menu (the Alt+Space / titlebar
//! right-click menu).
//!
//! Contains no `unsafe` and no Win32 imports so it compiles and tests everywhere.
//! `lib.rs::imp` calls [`menu_state`] and then drives `EnableMenuItem` +
//! `TrackPopupMenu` from the result.
//!
//! ## Why the app needs this at all
//!
//! The system menu is normally supplied by the OS because the window has
//! `WS_SYSMENU`. On a frameless window with a custom titlebar the OS never sees a
//! non-client right-click (the titlebar is client area — see
//! `hit_test::HitTestMode::MaximizeButtonOnly`), so the menu has to be popped
//! explicitly. Windows will happily grey the wrong items if the caller does not
//! set them: `GetSystemMenu` hands back the *default* enable state, which assumes
//! a restored, resizable window.

/// The six standard system-menu commands, in the order Windows lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysMenuItem {
    Restore,
    Move,
    Size,
    Minimize,
    Maximize,
    Close,
}

impl SysMenuItem {
    /// The `SC_*` command id this item posts as `WM_SYSCOMMAND`.
    ///
    /// Spelled out (winuser.h values) to keep this module platform-independent;
    /// a windows-only test in `lib.rs` asserts each equals the `windows_sys`
    /// constant so a drift cannot go unnoticed.
    #[must_use]
    pub const fn sc_command(self) -> u32 {
        match self {
            Self::Size => 0xF000,     // SC_SIZE
            Self::Move => 0xF010,     // SC_MOVE
            Self::Minimize => 0xF020, // SC_MINIMIZE
            Self::Maximize => 0xF030, // SC_MAXIMIZE
            Self::Close => 0xF060,    // SC_CLOSE
            Self::Restore => 0xF120,  // SC_RESTORE
        }
    }

    /// Every item, in menu order.
    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::Restore,
            Self::Move,
            Self::Size,
            Self::Minimize,
            Self::Maximize,
            Self::Close,
        ]
    }
}

/// The window state the menu's enable/disable rules read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowState {
    pub maximized: bool,
    pub minimized: bool,
    pub resizable: bool,
    pub minimizable: bool,
    pub maximizable: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            maximized: false,
            minimized: false,
            resizable: true,
            minimizable: true,
            maximizable: true,
        }
    }
}

/// Whether `item` should be enabled for `state`.
///
/// Mirrors the native rules (verified against Explorer / Notepad on Win11):
///
/// | Item     | Enabled when                                        |
/// |----------|-----------------------------------------------------|
/// | Restore  | the window is maximized **or** minimized            |
/// | Move     | the window is neither maximized nor minimized       |
/// | Size     | resizable **and** neither maximized nor minimized   |
/// | Minimize | minimizable **and** not already minimized           |
/// | Maximize | maximizable **and** not already maximized           |
/// | Close    | always                                              |
#[must_use]
pub const fn item_enabled(item: SysMenuItem, state: WindowState) -> bool {
    let normal = !state.maximized && !state.minimized;
    match item {
        SysMenuItem::Restore => state.maximized || state.minimized,
        SysMenuItem::Move => normal,
        SysMenuItem::Size => state.resizable && normal,
        SysMenuItem::Minimize => state.minimizable && !state.minimized,
        SysMenuItem::Maximize => state.maximizable && !state.maximized,
        SysMenuItem::Close => true,
    }
}

/// The full enable/disable table for `state`, in menu order.
#[must_use]
pub fn menu_state(state: WindowState) -> [(SysMenuItem, bool); 6] {
    SysMenuItem::all().map(|i| (i, item_enabled(i, state)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled(state: WindowState) -> Vec<SysMenuItem> {
        menu_state(state)
            .into_iter()
            .filter_map(|(i, on)| on.then_some(i))
            .collect()
    }

    #[test]
    fn restored_window_disables_restore_only() {
        let s = WindowState::default();
        assert_eq!(
            enabled(s),
            vec![
                SysMenuItem::Move,
                SysMenuItem::Size,
                SysMenuItem::Minimize,
                SysMenuItem::Maximize,
                SysMenuItem::Close,
            ]
        );
        assert!(!item_enabled(SysMenuItem::Restore, s));
    }

    #[test]
    fn maximized_window_disables_move_size_and_maximize() {
        let s = WindowState {
            maximized: true,
            ..WindowState::default()
        };
        assert_eq!(
            enabled(s),
            vec![
                SysMenuItem::Restore,
                SysMenuItem::Minimize,
                SysMenuItem::Close,
            ]
        );
        assert!(!item_enabled(SysMenuItem::Move, s));
        assert!(!item_enabled(SysMenuItem::Size, s));
        assert!(!item_enabled(SysMenuItem::Maximize, s));
    }

    #[test]
    fn minimized_window_enables_restore_and_maximize_only() {
        let s = WindowState {
            minimized: true,
            ..WindowState::default()
        };
        assert_eq!(
            enabled(s),
            vec![
                SysMenuItem::Restore,
                SysMenuItem::Maximize,
                SysMenuItem::Close,
            ]
        );
        assert!(!item_enabled(SysMenuItem::Minimize, s), "already minimized");
    }

    #[test]
    fn non_resizable_window_disables_size_but_keeps_move() {
        let s = WindowState {
            resizable: false,
            ..WindowState::default()
        };
        assert!(!item_enabled(SysMenuItem::Size, s));
        assert!(item_enabled(SysMenuItem::Move, s));
    }

    #[test]
    fn capability_flags_are_honoured() {
        let s = WindowState {
            minimizable: false,
            maximizable: false,
            ..WindowState::default()
        };
        assert!(!item_enabled(SysMenuItem::Minimize, s));
        assert!(!item_enabled(SysMenuItem::Maximize, s));
    }

    #[test]
    fn close_is_always_enabled() {
        for maximized in [false, true] {
            for minimized in [false, true] {
                for resizable in [false, true] {
                    let s = WindowState {
                        maximized,
                        minimized,
                        resizable,
                        minimizable: false,
                        maximizable: false,
                    };
                    assert!(
                        item_enabled(SysMenuItem::Close, s),
                        "Close must never be greyed ({s:?})"
                    );
                }
            }
        }
    }

    #[test]
    fn restore_and_maximize_are_never_both_enabled_when_maximized() {
        let s = WindowState {
            maximized: true,
            ..WindowState::default()
        };
        assert!(item_enabled(SysMenuItem::Restore, s));
        assert!(!item_enabled(SysMenuItem::Maximize, s));
    }

    #[test]
    fn menu_state_is_in_native_order_and_complete() {
        let order: Vec<SysMenuItem> = menu_state(WindowState::default())
            .into_iter()
            .map(|(i, _)| i)
            .collect();
        assert_eq!(order, SysMenuItem::all().to_vec());
    }

    #[test]
    fn sc_commands_are_the_documented_win32_values() {
        assert_eq!(SysMenuItem::Size.sc_command(), 0xF000);
        assert_eq!(SysMenuItem::Move.sc_command(), 0xF010);
        assert_eq!(SysMenuItem::Minimize.sc_command(), 0xF020);
        assert_eq!(SysMenuItem::Maximize.sc_command(), 0xF030);
        assert_eq!(SysMenuItem::Close.sc_command(), 0xF060);
        assert_eq!(SysMenuItem::Restore.sc_command(), 0xF120);
    }
}
