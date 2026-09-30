//! Native "choose a folder" dialog for the receive folder. Runs on the UI thread.

/// The chosen folder, or None when the user cancelled.
pub fn pick(title: &str, current: &str) -> Option<String> {
    imp::pick(title, current)
}

#[cfg(target_os = "linux")]
mod imp {
    use gtk::prelude::*;

    pub fn pick(title: &str, current: &str) -> Option<String> {
        let dialog = gtk::FileChooserDialog::with_buttons(
            Some(title),
            None::<&gtk::Window>,
            gtk::FileChooserAction::SelectFolder,
            &[("取消", gtk::ResponseType::Cancel), ("选择", gtk::ResponseType::Accept)],
        );
        dialog.set_modal(true);
        dialog.set_keep_above(true);
        if !current.is_empty() && std::path::Path::new(current).is_dir() {
            dialog.set_current_folder(current);
        }
        let chosen = if dialog.run() == gtk::ResponseType::Accept {
            dialog.filename().map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        dialog.close();
        chosen
    }
}

#[cfg(windows)]
mod imp {
    use windows::core::HSTRING;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
        FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
    };

    pub fn pick(title: &str, current: &str) -> Option<String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            let options = dialog.GetOptions().ok()?;
            dialog
                .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
                .ok()?;
            let _ = dialog.SetTitle(&HSTRING::from(title));
            if !current.is_empty() && std::path::Path::new(current).is_dir() {
                if let Ok(item) =
                    SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(current), None)
                {
                    let _ = dialog.SetFolder(&item);
                }
            }
            dialog.Show(None).ok()?;
            let item = dialog.GetResult().ok()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let path = name.to_string().ok();
            CoTaskMemFree(Some(name.0 as _));
            path
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub fn pick(_title: &str, _current: &str) -> Option<String> {
        None
    }
}
