//! Desktop notices, gated by the settings on the language page.

use csc_config::NotifyConfig;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub enum Kind {
    Connect,
    Disconnect,
    Transfer,
    Error,
}

struct Prefs {
    connect: bool,
    disconnect: bool,
    transfer: bool,
    errors: bool,
}

fn slot() -> &'static Mutex<Prefs> {
    static PREFS: OnceLock<Mutex<Prefs>> = OnceLock::new();
    PREFS.get_or_init(|| {
        Mutex::new(Prefs {
            connect: false,
            disconnect: true,
            transfer: true,
            errors: true,
        })
    })
}

pub fn apply(cfg: &NotifyConfig) {
    if let Ok(mut prefs) = slot().lock() {
        *prefs = Prefs {
            connect: cfg.connect,
            disconnect: cfg.disconnect,
            transfer: cfg.transfer,
            errors: cfg.errors,
        };
    }
}

fn allowed(kind: Kind) -> bool {
    let Ok(prefs) = slot().lock() else {
        return true;
    };
    match kind {
        Kind::Connect => prefs.connect,
        Kind::Disconnect => prefs.disconnect,
        Kind::Transfer => prefs.transfer,
        Kind::Error => prefs.errors,
    }
}

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn set_language(language: &str) {
    let en = match language {
        "en-US" => true,
        "zh-CN" => false,
        _ => std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LC_MESSAGES"))
            .or_else(|_| std::env::var("LANG"))
            .map(|v| v.to_ascii_lowercase().starts_with("en"))
            .unwrap_or(false),
    };
    ENGLISH.store(en, Ordering::Relaxed);
}

pub fn text(zh: &str, en: &str) -> String {
    if ENGLISH.load(Ordering::Relaxed) {
        en.to_string()
    } else {
        zh.to_string()
    }
}

fn recent(kind: Kind) -> bool {
    static LAST: OnceLock<Mutex<HashMap<u8, Instant>>> = OnceLock::new();
    let slot = LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut last) = slot.lock() else {
        return false;
    };
    let key = match kind {
        Kind::Connect => 1,
        Kind::Disconnect => 2,
        Kind::Transfer => 3,
        Kind::Error => 4,
    };
    let now = Instant::now();
    if last
        .get(&key)
        .is_some_and(|t| now.duration_since(*t) < Duration::from_secs(5))
    {
        return true;
    }
    last.insert(key, now);
    false
}

/// Log `body`, and show it when this kind of notice is enabled.
/// The same kind is shown at most once every 5 seconds.
pub fn tell(kind: Kind, title: &str, body: &str, error: bool) {
    if error {
        tracing::warn!("{body}");
    } else {
        tracing::info!("{body}");
    }
    if allowed(kind) && !recent(kind) {
        csc_platform::notify(title, body, error);
    }
}
