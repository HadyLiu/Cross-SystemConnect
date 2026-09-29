//! Path safety for file transfer receive paths.
#![allow(dead_code)] // exercised by file_transfer + unit tests

use std::path::{Component, Path, PathBuf};

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
