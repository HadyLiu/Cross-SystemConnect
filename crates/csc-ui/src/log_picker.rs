//! System "save as" dialog for choosing the log file. Runs on the UI thread.

use std::path::{Path, PathBuf};

/// Ask where to write the log, starting from `current`. None when cancelled.
#[cfg(target_os = "linux")]
pub fn pick(window: &tao::window::Window, current: &Path) -> Option<PathBuf> {
    use gtk::prelude::*;
    use tao::platform::unix::WindowExtUnix;
    let dialog = gtk::FileChooserDialog::with_buttons(
        Some("选择日志文件"),
        Some(window.gtk_window()),
        gtk::FileChooserAction::Save,
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("确定", gtk::ResponseType::Accept),
        ],
    );
    // The log is appended to, so an existing file is fine.
    dialog.set_do_overwrite_confirmation(false);
    if let Some(dir) = current.parent().filter(|d| d.is_dir()) {
        dialog.set_current_folder(dir);
    }
    dialog.set_current_name(
        current
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("log.txt"),
    );
    let chosen = if dialog.run() == gtk::ResponseType::Accept {
        dialog.filename()
    } else {
        None
    };
    unsafe { dialog.destroy() };
    chosen
}

#[cfg(windows)]
pub fn pick(window: &tao::window::Window, current: &Path) -> Option<PathBuf> {
    use tao::platform::windows::WindowExtWindows;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        FileSaveDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
    };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let name = current
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("log.txt");
        let _ = dialog.SetTitle(&HSTRING::from("选择日志文件"));
        let _ = dialog.SetFileName(&HSTRING::from(name));
        let _ = dialog.SetDefaultExtension(&HSTRING::from("txt"));
        if let Some(dir) = current.parent().filter(|d| d.is_dir()) {
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(
                &HSTRING::from(dir.as_os_str()),
                None,
            ) {
                let _ = dialog.SetFolder(&item);
            }
        }
        dialog.Show(HWND(window.hwnd() as _)).ok()?;
        let raw = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let path = raw.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(raw.0 as _));
        path
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn pick(_window: &tao::window::Window, _current: &Path) -> Option<PathBuf> {
    None
}
