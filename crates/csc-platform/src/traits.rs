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

pub trait ScreenInfo: Send {
    fn virtual_size(&self) -> ScreenSize;
}

pub trait AutoStart: Send {
    fn is_enabled(&self) -> bool;
    fn set_enabled(&mut self, on: bool) -> anyhow::Result<()>;
}
