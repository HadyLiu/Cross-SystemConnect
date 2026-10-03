//! Path safety for file transfer receive paths.

use std::path::{Component, Path, PathBuf};

/// Longest relative path accepted, in UTF-16 units, so that the receive folder
/// plus the path stays under the classic Windows 260-character limit.
pub const MAX_RELATIVE_PATH: usize = 200;

/// Why `name` cannot be a file name on every supported system, or None.
/// Windows rules are the strictest, so they apply everywhere.
pub fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() || name == "." || name == ".." {
        return Some("名称为空或为 . / ..");
    }
    if name.chars().any(|c| (c as u32) < 0x20) {
        return Some("包含控制字符");
    }
    if name
        .chars()
        .any(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
    {
        return Some("包含 Windows 不允许的字符 < > : \" / \\ | ? *");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("以点或空格结尾，Windows 不支持");
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if reserved {
        return Some("是 Windows 保留名称 (CON、NUL、COM1 等)");
    }
    if name.encode_utf16().count() > 255 {
        return Some("名称超过 255 个字符");
    }
    None
}

/// Rewrite one file name so Windows and Linux can both store it.
pub fn adapt_component(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if (c as u32) < 0x20 || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        out.push('_');
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if reserved {
        if let Some(dot) = out.find('.') {
            out.insert(dot, '_');
        } else {
            out.push('_');
        }
    }
    let bytes = out.as_bytes();
    if bytes.len() > 255 {
        let mut end = 255;
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

/// `/`-separated relative path with each part adapted. `..` and empty parts stay errors.
pub fn adapt_relative_path(rel: &str) -> Result<String, String> {
    if rel.is_empty() || rel.starts_with('/') || rel.contains("//") {
        return Err("不安全的路径".into());
    }
    let mut parts = Vec::new();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err("不安全的路径".into());
        }
        parts.push(adapt_component(part));
    }
    Ok(parts.join("/"))
}

/// Check a `/`-separated relative path from a peer. Returns the local path or the reason.
pub fn check_relative_path(rel: &str) -> Result<PathBuf, String> {
    if rel.encode_utf16().count() > MAX_RELATIVE_PATH {
        return Err(format!("路径超过 {MAX_RELATIVE_PATH} 个字符"));
    }
    let mut out = PathBuf::new();
    for part in rel.split('/') {
        if let Some(why) = name_problem(part) {
            return Err(format!("“{part}” {why}"));
        }
        out.push(part);
    }
    sanitize_relative_path(&out.to_string_lossy()).ok_or_else(|| "不安全的路径".to_string())
}

/// Reject absolute paths and `..` components. Returns a sanitized relative path.
pub fn sanitize_relative_path(p: &str) -> Option<PathBuf> {
    let path = Path::new(p);
    if path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir => return None,
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_name_rules() {
        assert!(name_problem("报告.docx").is_none());
        assert!(name_problem("a:b").is_some());
        assert!(name_problem("what?").is_some());
        assert!(name_problem("con").is_some());
        assert!(name_problem("NUL.txt").is_some());
        assert!(name_problem("COM3").is_some());
        assert!(name_problem("COM0").is_none());
        assert!(name_problem("console").is_none());
        assert!(name_problem("trailing.").is_some());
        assert!(name_problem("tab\there").is_some());
        assert!(check_relative_path("dir/sub/file.txt").is_ok());
        assert!(check_relative_path("dir/../x").is_err());
        assert!(check_relative_path("/abs").is_err());
        assert!(check_relative_path("a//b").is_err());
        assert_eq!(adapt_component("bad:name.txt"), "bad_name.txt");
        assert_eq!(adapt_component("CON.txt"), "CON_.txt");
        assert_eq!(adapt_relative_path("dir/a:b").unwrap(), "dir/a_b");
        assert!(adapt_relative_path("dir/../x").is_err());
    }

    #[test]
    fn rejects_traversal() {
        assert!(sanitize_relative_path("../etc/passwd").is_none());
        assert!(sanitize_relative_path("/abs").is_none());
        assert_eq!(
            sanitize_relative_path("a/b.txt").unwrap(),
            PathBuf::from("a/b.txt")
        );
    }

    #[test]
    fn rejects_nested_escape_and_empty() {
        assert!(sanitize_relative_path("a/../../etc/passwd").is_none());
        assert!(sanitize_relative_path("").is_none());
        assert!(sanitize_relative_path(".").is_none());
        assert_eq!(
            sanitize_relative_path("foo/./bar").unwrap(),
            PathBuf::from("foo").join("bar")
        );
    }
}
