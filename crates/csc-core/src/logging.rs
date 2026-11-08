//! Core log: level, file and on/off follow the settings without a restart.
//! The file is capped at 100 KB (see `csc_config::logfile`).

use csc_config::logfile::{CappedLog, SharedLog};
use csc_config::{fallback_log_path, LogConfig};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::warn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, reload, EnvFilter, Registry};

#[derive(Clone)]
pub struct LogControl {
    file: SharedLog,
    /// The path the settings ask for, None while off. The open file may be the fallback.
    wanted: Arc<Mutex<Option<PathBuf>>>,
    filter: reload::Handle<EnvFilter, Registry>,
}

fn filter_for(level: &str) -> EnvFilter {
    let lowered = level.trim().to_ascii_lowercase();
    let level = if lowered.is_empty() {
        "info"
    } else {
        lowered.as_str()
    };
    // Global level = user setting so `debug` shows every crate's DEBUG/INFO/WARN/ERROR.
    // Keep legacy binary crate name and current package names so reloads still match.
    let expr = format!(
        "{level},Cross_SystemConnect_core={level},csc_core={level},csc_platform={level},csc_config={level},csc_proto={level}"
    );
    EnvFilter::try_new(&expr).unwrap_or_else(|_| EnvFilter::new(level))
}

/// The configured file, or the fallback when it cannot be opened.
fn open_file(cfg: &LogConfig) -> Option<CappedLog> {
    let wanted = cfg.path();
    match CappedLog::open(&wanted) {
        Ok(log) => Some(log),
        Err(err) => {
            let fallback = fallback_log_path();
            eprintln!(
                "log file {} unavailable ({err}); using {}",
                wanted.display(),
                fallback.display()
            );
            CappedLog::open(&fallback).ok()
        }
    }
}

impl LogControl {
    /// Install the global subscriber. Never fails: without a usable file nothing is written.
    pub fn init(cfg: &LogConfig) -> Self {
        let file = SharedLog::default();
        if cfg.to_file {
            file.set(open_file(cfg));
        }
        let (filter, handle) = reload::Layer::new(filter_for(&cfg.level));
        let writer = file.clone();
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(
                fmt::layer()
                    .with_ansi(false)
                    .with_target(true)
                    .with_level(true)
                    .with_writer(move || writer.clone()),
            )
            .try_init();
        Self {
            file,
            wanted: Arc::new(Mutex::new(cfg.to_file.then(|| cfg.path()))),
            filter: handle,
        }
    }

    /// Follow changed log settings.
    pub fn apply(&self, cfg: &LogConfig) {
        if let Err(e) = self.filter.reload(filter_for(&cfg.level)) {
            warn!("log level: {e}");
        }
        let wanted = cfg.to_file.then(|| cfg.path());
        let Ok(mut cur) = self.wanted.lock() else {
            return;
        };
        if *cur == wanted {
            return;
        }
        let shown = wanted.as_ref().map(|p| p.display().to_string());
        // Noted in the old file, so the switch is visible there too.
        tracing::info!("log file now {}", shown.as_deref().unwrap_or("off"));
        self.file
            .set(if cfg.to_file { open_file(cfg) } else { None });
        *cur = wanted;
    }

    /// File being written, None while writing to a file is off.
    pub fn path(&self) -> Option<PathBuf> {
        self.file.path()
    }
}
