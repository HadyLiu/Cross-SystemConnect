//! Windows elevated service mode, plain logic (tested on every platform).
//!
//! The service runs as LocalSystem and starts the real core in the active
//! console session with a copy of winlogon.exe's token, so the core runs as
//! SYSTEM on the user's desktop. Windows then lets it send input to
//! administrator windows, UAC prompts and the sign-in screen.

use std::path::Path;
use std::time::Duration;

pub const SERVICE_NAME: &str = "CrossSystemConnect";
pub const SERVICE_DISPLAY: &str = "Cross-SystemConnect 管理员服务";
pub const SERVICE_DESCRIPTION: &str =
    "以 SYSTEM 身份在当前会话启动 Cross-SystemConnect 核心，使被控端可以操作管理员窗口、UAC 提示和登录界面。";

pub const SERVICE_FLAG: &str = "--service";
pub const CHILD_FLAG: &str = "--service-child";
pub const INSTALL_FLAG: &str = "--install-service";
pub const UNINSTALL_FLAG: &str = "--uninstall-service";

/// Quote one argument the way `CommandLineToArgvW` reads it back.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

fn line(exe: &Path, args: &[&str]) -> String {
    let mut s = quote_arg(&exe.to_string_lossy());
    for a in args {
        s.push(' ');
        s.push_str(&quote_arg(a));
    }
    s
}

/// Command line the service manager runs.
pub fn service_command(exe: &Path, config: &Path) -> String {
    line(exe, &[SERVICE_FLAG, "--config", &config.to_string_lossy()])
}

/// Command line of the core the service starts in the user's session.
pub fn child_command(exe: &Path, config: &Path) -> String {
    line(exe, &["--config", &config.to_string_lossy(), CHILD_FLAG])
}

/// Parameters for the elevated `--install-service` run (no program name).
pub fn install_params(config: &Path) -> String {
    format!(
        "{INSTALL_FLAG} --config {}",
        quote_arg(&config.to_string_lossy())
    )
}

/// What the supervisor does on one tick.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// No core yet, or it exited: start one (after the backoff).
    Start(u32),
    /// The console moved to another session (user switch, RDP): replace it.
    Restart(u32),
    /// Nobody on the console: stop the core.
    Stop,
}

/// `child`: session of the running core and whether it has exited.
/// `active`: the active console session, if any.
pub fn decide(child: Option<(u32, bool)>, active: Option<u32>) -> Action {
    match (child, active) {
        (Some((_, false)), None) => Action::Stop,
        (_, None) => Action::Nothing,
        (None, Some(s)) | (Some((_, true)), Some(s)) => Action::Start(s),
        (Some((running, false)), Some(s)) if running != s => Action::Restart(s),
        _ => Action::Nothing,
    }
}

/// Restart delay: 1 s, doubling to 10 s while the core keeps dying quickly;
/// back to 1 s once it stayed up for 30 s.
pub struct Backoff {
    delay: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self { delay: Self::MIN }
    }
}

impl Backoff {
    pub const MIN: Duration = Duration::from_secs(1);
    pub const MAX: Duration = Duration::from_secs(10);
    pub const HEALTHY: Duration = Duration::from_secs(30);

    pub fn after_exit(&mut self, ran_for: Duration) -> Duration {
        if ran_for >= Self::HEALTHY {
            self.delay = Self::MIN;
        } else {
            let wait = self.delay;
            self.delay = (self.delay * 2).min(Self::MAX);
            return wait;
        }
        self.delay
    }
}

/// Replace or add `extra` in a `KEY=value` environment list (keys compare
/// case-insensitively, as on Windows).
pub fn merge_env(mut vars: Vec<String>, extra: &[(&str, String)]) -> Vec<String> {
    for (key, value) in extra {
        let prefix = format!("{key}=");
        vars.retain(|v| {
            !(v.len() >= prefix.len() && v[..prefix.len()].eq_ignore_ascii_case(&prefix))
        });
        vars.push(format!("{key}={value}"));
    }
    vars
}

/// UTF-16 environment block: each `KEY=value` NUL-terminated, then one more NUL.
pub fn encode_env_block(vars: &[String]) -> Vec<u16> {
    let mut out = Vec::new();
    for v in vars {
        out.extend(v.encode_utf16());
        out.push(0);
    }
    if vars.is_empty() {
        out.push(0);
    }
    out.push(0);
    out
}

/// Split a UTF-16 environment block back into `KEY=value` strings.
pub fn decode_env_block(block: &[u16]) -> Vec<String> {
    block
        .split(|&c| c == 0)
        .take_while(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn quoting_round_trips_windows_rules() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg(r"C:\Program Files\a"), r#""C:\Program Files\a""#);
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
    }

    #[test]
    fn command_lines() {
        let exe = PathBuf::from(r"C:\Program Files\CSC\Cross-SystemConnect-core.exe");
        let cfg =
            PathBuf::from(r"C:\Users\Hady Liu\AppData\Roaming\Cross-SystemConnect\config.json");
        assert_eq!(
            service_command(&exe, &cfg),
            r#""C:\Program Files\CSC\Cross-SystemConnect-core.exe" --service --config "C:\Users\Hady Liu\AppData\Roaming\Cross-SystemConnect\config.json""#
        );
        assert!(child_command(&exe, &cfg).ends_with(" --service-child"));
        assert!(install_params(&cfg).starts_with("--install-service --config \""));
    }

    #[test]
    fn supervisor_decisions() {
        assert_eq!(decide(None, Some(1)), Action::Start(1));
        assert_eq!(decide(Some((1, true)), Some(1)), Action::Start(1));
        assert_eq!(decide(Some((1, false)), Some(1)), Action::Nothing);
        assert_eq!(decide(Some((1, false)), Some(2)), Action::Restart(2));
        assert_eq!(decide(Some((1, false)), None), Action::Stop);
        assert_eq!(decide(None, None), Action::Nothing);
    }

    #[test]
    fn backoff_grows_and_resets() {
        let mut b = Backoff::default();
        let quick = Duration::from_secs(2);
        assert_eq!(b.after_exit(quick), Duration::from_secs(1));
        assert_eq!(b.after_exit(quick), Duration::from_secs(2));
        assert_eq!(b.after_exit(quick), Duration::from_secs(4));
        assert_eq!(b.after_exit(quick), Duration::from_secs(8));
        assert_eq!(b.after_exit(quick), Duration::from_secs(10));
        assert_eq!(b.after_exit(quick), Duration::from_secs(10));
        assert_eq!(
            b.after_exit(Duration::from_secs(60)),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn env_block_merge_and_encode() {
        let vars = vec![
            "Path=C:\\x".to_string(),
            "CSC_USER_CONFIG_DIR=old".to_string(),
        ];
        let merged = merge_env(
            vars,
            &[("csc_user_config_dir", "new".into()), ("A", "1".into())],
        );
        assert_eq!(merged, vec!["Path=C:\\x", "csc_user_config_dir=new", "A=1"]);
        let block = encode_env_block(&merged);
        assert_eq!(&block[block.len() - 2..], &[0, 0]);
        assert_eq!(decode_env_block(&block), merged);
        assert_eq!(encode_env_block(&[]), vec![0, 0]);
    }
}
