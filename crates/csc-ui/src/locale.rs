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

fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

pub fn settings() -> &'static str {
    if english() {
        "Settings"
    } else {
        "设置"
    }
}

pub fn paste() -> &'static str {
    if english() {
        "Paste files copied on the other computer"
    } else {
        "粘贴其他电脑复制的文件到这里"
    }
}

pub fn pause_sharing() -> &'static str {
    if english() {
        "Pause sharing"
    } else {
        "暂停共享"
    }
}

pub fn resume_sharing() -> &'static str {
    if english() {
        "Resume sharing"
    } else {
        "继续共享"
    }
}

pub fn quit() -> &'static str {
    if english() {
        "Quit"
    } else {
        "退出"
    }
}
