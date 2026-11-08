//! System "save as" dialog for choosing the log file.

use std::path::{Path, PathBuf};

/// Ask where to write the log, starting from `current`. None when cancelled.
pub fn pick(current: &Path) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .set_title("选择日志文件")
        .add_filter("Text", &["txt", "log"])
        .set_file_name(
            current
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("log.txt"),
        );
    if let Some(dir) = current.parent().filter(|d| d.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    dialog.save_file()
}
