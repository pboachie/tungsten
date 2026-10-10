// SPDX-License-Identifier: AGPL-3.0-only
//! `file:` URIs and lexically normalized paths.

use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use lsp_types::Uri;

/// The path of a `file:` URI; `None` for any other scheme.
pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let text = uri.as_str();
    let rest = text.strip_prefix("file://")?;
    // Skip the authority (empty or `localhost`).
    let path = &rest[rest.find('/')?..];
    let decoded = percent_decode(path)?;
    #[cfg(windows)]
    let decoded = {
        let trimmed = decoded.strip_prefix('/').unwrap_or(&decoded);
        trimmed.to_string()
    };
    Some(normalize(Path::new(&decoded)))
}

/// The `file:` URI of an absolute path.
pub fn path_to_uri(path: &Path) -> Option<Uri> {
    let text = path.to_str()?;
    let mut out = String::from("file://");
    if !text.starts_with('/') {
        out.push('/');
    }
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(b as char);
            }
            #[cfg(windows)]
            b'\\' => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    Uri::from_str(&out).ok()
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `path` with `.` and `..` resolved lexically (no filesystem access).
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// An absolute, normalized form of `path` (relative to the current
/// directory when it is relative).
pub fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize(path)
    } else {
        match std::env::current_dir() {
            Ok(cwd) => normalize(&cwd.join(path)),
            Err(_) => normalize(path),
        }
    }
}
