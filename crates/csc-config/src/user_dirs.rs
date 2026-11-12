//! The signed-in user's folders.
//!
//! In the Windows elevated service mode the core runs with the SYSTEM token,
//! where the usual lookups would return the system profile. The service then
//! passes the user's real folders through these variables, which win over
//! the normal lookup.

use std::ffi::OsString;
use std::path::PathBuf;

pub const CONFIG_VAR: &str = "CSC_USER_CONFIG_DIR";
pub const CACHE_VAR: &str = "CSC_USER_CACHE_DIR";
pub const DESKTOP_VAR: &str = "CSC_USER_DESKTOP_DIR";
pub const DOWNLOAD_VAR: &str = "CSC_USER_DOWNLOAD_DIR";
pub const HOME_VAR: &str = "CSC_USER_HOME_DIR";

/// A non-empty override wins over the normal lookup.
pub fn resolve(
    over: Option<OsString>,
    fallback: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    match over {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => fallback(),
    }
}

fn pick(var: &str, fallback: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    resolve(std::env::var_os(var), fallback)
}

/// Roaming app data on Windows, `~/.config` on Linux.
pub fn config_dir() -> Option<PathBuf> {
    pick(CONFIG_VAR, dirs::config_dir)
}

pub fn cache_dir() -> Option<PathBuf> {
    pick(CACHE_VAR, dirs::cache_dir)
}

pub fn desktop_dir() -> Option<PathBuf> {
    pick(DESKTOP_VAR, dirs::desktop_dir)
}

pub fn download_dir() -> Option<PathBuf> {
    pick(DOWNLOAD_VAR, dirs::download_dir)
}

pub fn home_dir() -> Option<PathBuf> {
    pick(HOME_VAR, dirs::home_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_unless_empty() {
        let fb = || Some(PathBuf::from("/fallback"));
        assert_eq!(
            resolve(Some("/user".into()), fb),
            Some(PathBuf::from("/user"))
        );
        assert_eq!(
            resolve(Some(OsString::new()), fb),
            Some(PathBuf::from("/fallback"))
        );
        assert_eq!(resolve(None, fb), Some(PathBuf::from("/fallback")));
        assert_eq!(resolve(None, || None), None);
    }
}
