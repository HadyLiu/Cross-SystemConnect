//! Tray labels follow the language saved in the settings window.

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

fn system_english() -> bool {
    let lang = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    lang.to_ascii_lowercase().starts_with("en")
}

/// `auto`, `zh-CN`, or `en-US`.
pub fn apply(code: &str) {
    let english = match code {
        "en-US" => true,
        "zh-CN" => false,
        _ => system_english(),
    };
    ENGLISH.store(english, Ordering::Relaxed);
}

pub fn is_english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

fn english() -> bool {
    is_english()
}

pub fn settings() -> &'static str {
    if english() {
        "Settings"
    } else {
        "设置"
    }
}

pub fn quit() -> &'static str {
    if english() {
        "Quit"
    } else {
        "退出"
    }
}
