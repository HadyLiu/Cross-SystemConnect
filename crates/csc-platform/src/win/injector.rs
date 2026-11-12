use crate::traits::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MOUSE_EVENT_FLAGS,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
};

/// Motion and clicks share this stream so overlays like Win+Shift+S see both.
const ABS_MOVE: MOUSE_EVENT_FLAGS =
    MOUSE_EVENT_FLAGS(MOUSEEVENTF_MOVE.0 | MOUSEEVENTF_ABSOLUTE.0 | MOUSEEVENTF_VIRTUALDESK.0);

#[derive(Default)]
pub struct WinInputInjector {
    /// Last absolute position, in pixels relative to the virtual-desktop origin.
    last_pos: Option<(i32, i32)>,
}

impl WinInputInjector {
    pub fn new() -> Self {
        Self::default()
    }

    fn normalized_pos(&self) -> (i32, i32) {
        if let Some((x, y)) = self.last_pos {
            return normalize(x, y);
        }
        let (cx, cy) = super::cursor_pos();
        let (ox, oy) = super::virtual_origin();
        normalize(cx - ox, cy - oy)
    }

    fn remember_cursor(&mut self) {
        let (cx, cy) = super::cursor_pos();
        let (ox, oy) = super::virtual_origin();
        self.last_pos = Some((cx - ox, cy - oy));
    }
}

impl InputInjector for WinInputInjector {
    fn mouse_move_abs(&mut self, x: i32, y: i32) {
        super::desktop::follow_input_desktop();
        self.last_pos = Some((x, y));
        let (dx, dy) = normalize(x, y);
        unsafe {
            let _ = SendInput(&[mouse_input(dx, dy, 0, ABS_MOVE)], size_of_input());
        }
    }

    fn mouse_move_rel(&mut self, dx: i32, dy: i32) {
        super::desktop::follow_input_desktop();
        unsafe {
            let _ = SendInput(&[mouse_input(dx, dy, 0, MOUSEEVENTF_MOVE)], size_of_input());
        }
        self.remember_cursor();
    }

    fn mouse_button(&mut self, button: u32, down: bool) {
        const XBUTTON1: u32 = 1;
        const XBUTTON2: u32 = 2;
        let (flags, data) = match (button, down) {
            (0, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (0, false) => (MOUSEEVENTF_LEFTUP, 0),
            (1, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (1, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (2, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (2, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (3, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
            (3, false) => (MOUSEEVENTF_XUP, XBUTTON1),
            (4, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
            (4, false) => (MOUSEEVENTF_XUP, XBUTTON2),
            _ => return,
        };
        super::desktop::follow_input_desktop();
        if down {
            super::elevation::check_foreground();
        }
        let (dx, dy) = self.normalized_pos();
        // One SendInput: first park at the last absolute point, then press/release
        // with the same coordinates, so the events are not split across APIs.
        unsafe {
            let _ = SendInput(
                &[
                    mouse_input(dx, dy, 0, ABS_MOVE),
                    mouse_input(dx, dy, data, flags | ABS_MOVE),
                ],
                size_of_input(),
            );
        }
    }

    fn mouse_wheel(&mut self, v: i32, h: i32) {
        super::desktop::follow_input_desktop();
        let (dx, dy) = self.normalized_pos();
        unsafe {
            if v != 0 {
                let _ = SendInput(
                    &[mouse_input(dx, dy, v as u32, MOUSEEVENTF_WHEEL | ABS_MOVE)],
                    size_of_input(),
                );
            }
            if h != 0 {
                let _ = SendInput(
                    &[mouse_input(dx, dy, h as u32, MOUSEEVENTF_HWHEEL | ABS_MOVE)],
                    size_of_input(),
                );
            }
        }
    }

    fn key(&mut self, hid_usage: u32, down: bool) {
        let Some((scan, extended)) = crate::keymap::scan_from_hid(hid_usage) else {
            return;
        };
        let mut flags = KEYEVENTF_SCANCODE;
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !down {
            flags |= KEYEVENTF_KEYUP;
        }
        super::desktop::follow_input_desktop();
        if down {
            super::elevation::check_foreground();
        }
        unsafe {
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: scan,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            let _ = SendInput(&[input], size_of_input());
        }
    }
}

fn virtual_span() -> (i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        )
    }
}

/// Map a pixel on the virtual desktop to the 0..=65535 range SendInput expects.
fn normalize(x: i32, y: i32) -> (i32, i32) {
    let (w, h) = virtual_span();
    (map_axis(x, w), map_axis(y, h))
}

fn map_axis(pos: i32, span: i32) -> i32 {
    let max = (span - 1).max(1);
    let pos = pos.clamp(0, max);
    ((pos as i64 * 65535 + i64::from(max) / 2) / i64::from(max)) as i32
}

fn mouse_input(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn size_of_input() -> i32 {
    std::mem::size_of::<INPUT>() as i32
}
