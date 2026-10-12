//! Fallback platform backends (used on non-primary targets / Linux stubs).
#![allow(dead_code)]

use crate::traits::*;
use tracing::debug;

pub struct StubInputCapture;
impl InputCapture for StubInputCapture {
    fn start(
        &mut self,
        _on_mouse: Box<dyn FnMut(MouseEvent) -> bool + Send>,
        _on_key: Box<dyn FnMut(KeyEvent) -> bool + Send>,
    ) -> anyhow::Result<()> {
        debug!("StubInputCapture::start");
        Ok(())
    }
    fn stop(&mut self) {}
    fn set_grab(&mut self, _grab: bool) {}
    fn warp_cursor(&mut self, _x: i32, _y: i32) {}
}

pub struct StubInputInjector;
impl InputInjector for StubInputInjector {
    fn mouse_move_abs(&mut self, _x: i32, _y: i32) {}
    fn mouse_move_rel(&mut self, _dx: i32, _dy: i32) {}
    fn mouse_button(&mut self, _button: u32, _down: bool) {}
    fn mouse_wheel(&mut self, _v: i32, _h: i32) {}
    fn key(&mut self, _hid_usage: u32, _down: bool) {}
}

#[derive(Default)]
pub struct StubClipboard {
    text: Option<String>,
}
impl Clipboard for StubClipboard {
    fn watch(&mut self, _on_changed: Box<dyn FnMut() + Send>) {}
    fn get_text(&mut self) -> Option<String> {
        self.text.clone()
    }
    fn set_text(&mut self, text: &str) {
        self.text = Some(text.to_string());
    }
    fn get_image_png(&mut self) -> Option<Vec<u8>> {
        None
    }
    fn set_image_png(&mut self, _png: &[u8]) {}
    fn get_files(&mut self) -> Option<Vec<std::path::PathBuf>> {
        None
    }
    fn set_files_lazy(&mut self, _entries: &[FileEntry]) {}
}

pub struct StubScreenInfo;
impl ScreenInfo for StubScreenInfo {
    fn virtual_size(&self) -> ScreenSize {
        ScreenSize {
            width: 1920,
            height: 1080,
        }
    }
}

#[derive(Default)]
pub struct StubAutoStart {
    enabled: bool,
}
impl AutoStart for StubAutoStart {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
    fn set_enabled(&mut self, on: bool) -> anyhow::Result<()> {
        self.enabled = on;
        Ok(())
    }
}
