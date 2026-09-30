#[derive(Debug, Clone, Copy, Default)]
pub struct ScreenSize {
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Default)]
pub struct MouseEvent {
    pub dx: i32,
    pub dy: i32,
    pub x: i32,
    pub y: i32,
    pub buttons: u32,
    pub wheel_v: i32,
    pub wheel_h: i32,
}

#[derive(Debug, Clone, Default)]
pub struct KeyEvent {
    pub hid_usage: u32,
    pub down: bool,
    pub modifiers: u32,
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub relative_path: String,
    pub size: u64,
    pub mtime_unix: i64,
    pub is_directory: bool,
}

pub trait InputCapture: Send {
    fn start(
        &mut self,
        on_mouse: Box<dyn FnMut(MouseEvent) -> bool + Send>,
        on_key: Box<dyn FnMut(KeyEvent) -> bool + Send>,
    ) -> anyhow::Result<()>;
    fn stop(&mut self);
    fn set_grab(&mut self, grab: bool);
    fn warp_cursor(&mut self, x: i32, y: i32);
}

pub trait InputInjector: Send {
    fn mouse_move_abs(&mut self, x: i32, y: i32);
    fn mouse_move_rel(&mut self, dx: i32, dy: i32);
    fn mouse_button(&mut self, button: u32, down: bool);
    fn mouse_wheel(&mut self, v: i32, h: i32);
    fn key(&mut self, hid_usage: u32, down: bool);
}

pub trait Clipboard: Send {
    fn watch(&mut self, on_changed: Box<dyn FnMut() + Send>);
    fn get_text(&mut self) -> Option<String>;
    fn set_text(&mut self, text: &str);
    fn get_image_png(&mut self) -> Option<Vec<u8>>;
    fn set_image_png(&mut self, png: &[u8]);
    fn get_files(&mut self) -> Option<Vec<std::path::PathBuf>>;
    fn set_files_lazy(&mut self, entries: &[FileEntry]);
}

pub trait DragDrop: Send {
    fn current_drag_files(&mut self) -> Option<Vec<std::path::PathBuf>>;
    fn begin_drag(&mut self, entries: &[FileEntry]);
    fn cancel_drag(&mut self);
}

/// Monitor area in virtual-desktop pixels, relative to the desktop origin.
/// `right` and `bottom` are exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn center(&self) -> (i32, i32) {
        (self.left + self.width() / 2, self.top + self.height() / 2)
    }
}

/// Input read from the local devices while the pointer is on a remote screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Captured {
    Move {
        dx: i32,
        dy: i32,
    },
    /// 0 left, 1 right, 2 middle, 3 back, 4 forward.
    Button {
        button: u32,
        down: bool,
    },
    /// Whole notches. Positive `v` scrolls up, positive `h` scrolls right.
    Wheel {
        v: i32,
        h: i32,
    },
    Key {
        hid: u32,
        down: bool,
    },
}

pub trait ScreenInfo: Send {
    fn virtual_size(&self) -> ScreenSize;
    /// Each physical monitor. The default is one monitor covering the desktop.
    fn monitors(&self) -> Vec<Rect> {
        let s = self.virtual_size();
        vec![Rect {
            left: 0,
            top: 0,
            right: s.width,
            bottom: s.height,
        }]
    }
}

pub trait AutoStart: Send {
    fn is_enabled(&self) -> bool;
    fn set_enabled(&mut self, on: bool) -> anyhow::Result<()>;
}
