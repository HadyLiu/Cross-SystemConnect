//! Launch at login for the tray app. Off unless the user turns it on.

use anyhow::Context;

const ENTRY: &str = "Cross-SystemConnect";

/// Add or remove the login entry so it matches `enabled`.
pub fn apply(enabled: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("locate executable")?;
    imp::apply(enabled, &exe)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::ENTRY;
    use anyhow::Context;
    use std::path::Path;

    fn desktop_file() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|d| d.join("autostart").join("cross-systemconnect.desktop"))
    }

    pub fn apply(enabled: bool, exe: &Path) -> anyhow::Result<()> {
        let Some(path) = desktop_file() else {
            return Ok(());
        };
        if !enabled {
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("remove {}", path.display()))?;
            }
            return Ok(());
        }
        let body = format!(
            "[Desktop Entry]\nType=Application\nName={ENTRY}\nExec=\"{}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        if std::fs::read_to_string(&path).ok().as_deref() == Some(body.as_str()) {
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))
    }
}

#[cfg(windows)]
mod imp {
    use super::ENTRY;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn reg(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
        Command::new("reg")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status()
    }

    pub fn apply(enabled: bool, exe: &Path) -> anyhow::Result<()> {
        if enabled {
            let value = format!("\"{}\"", exe.display());
            let status = reg(&["add", RUN_KEY, "/v", ENTRY, "/t", "REG_SZ", "/d", &value, "/f"])?;
            anyhow::ensure!(status.success(), "reg add failed: {status}");
        } else if reg(&["query", RUN_KEY, "/v", ENTRY])?.success() {
            let status = reg(&["delete", RUN_KEY, "/v", ENTRY, "/f"])?;
            anyhow::ensure!(status.success(), "reg delete failed: {status}");
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub fn apply(_enabled: bool, _exe: &std::path::Path) -> anyhow::Result<()> {
        Ok(())
    }
}
