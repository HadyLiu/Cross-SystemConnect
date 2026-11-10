//! Match Win32 popup menus (including the tray) to the app theme.
//!
//! `TrackPopupMenu` stays in the classic light style unless the process calls
//! the undocumented uxtheme `SetPreferredAppMode` API first.

use windows::core::PCSTR;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

/// PreferredAppMode::ForceDark / ForceLight. On Windows 10 1809 the same
/// ordinal was AllowDarkModeForApp(BOOL); a non-zero value enables dark there.
const FORCE_DARK: i32 = 2;
const FORCE_LIGHT: i32 = 3;
const SET_PREFERRED_APP_MODE: u16 = 135;
const FLUSH_MENU_THEMES: u16 = 136;

type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
type FlushMenuThemes = unsafe extern "system" fn();

pub fn set(dark: bool) {
    unsafe {
        let Ok(uxtheme) = LoadLibraryA(windows::core::s!("uxtheme.dll")) else {
            return;
        };
        if let Some(symbol) = ordinal(uxtheme, SET_PREFERRED_APP_MODE) {
            let set_mode: SetPreferredAppMode = std::mem::transmute(symbol);
            set_mode(if dark { FORCE_DARK } else { FORCE_LIGHT });
        }
        if let Some(symbol) = ordinal(uxtheme, FLUSH_MENU_THEMES) {
            let flush: FlushMenuThemes = std::mem::transmute(symbol);
            flush();
        }
    }
}

unsafe fn ordinal(
    module: windows::Win32::Foundation::HMODULE,
    ordinal: u16,
) -> Option<unsafe extern "system" fn() -> isize> {
    GetProcAddress(module, PCSTR(ordinal as usize as *const u8))
}
