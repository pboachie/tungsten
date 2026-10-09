// SPDX-License-Identifier: AGPL-3.0-only
//! `$ref` strings: parsing, percent-decoding and resolution.

use std::path::{Component, Path, PathBuf};

use crate::normalize::translate;
use crate::{DocId, RefTarget, Workspace};

/// A parsed `$ref`. Pointers are RFC 6901 strings (escaped, `""` = root)
/// with percent-encoding already decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reference {
    /// `#/components/schemas/Pet`
    Local { pointer: String },
    /// `common.yaml#/Pet`, `./dir/pet.json`
    File { path: String, pointer: String },
    /// `https://example.com/spec.yaml#/Pet`
    Remote { url: String },
}

/// Parse a `$ref` value. The error is a reason suitable for a diagnostic.
pub(crate) fn parse_reference(reference: &str) -> Result<Reference, String> {
    let (location, fragment) = match reference.split_once('#') {
        Some((l, f)) => (l, f),
        None => (reference, ""),
    };
    if let Some(scheme) = scheme(location) {
        return match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" => Ok(Reference::Remote {
                url: reference.to_string(),
            }),
            other => Err(format!("unsupported URI scheme `{other}:`")),
        };
    }
    let pointer = percent_decode(fragment)
        .ok_or_else(|| "the fragment is not valid percent-encoded UTF-8".to_string())?;
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return Err(format!(
            "`#{pointer}` is not a JSON Pointer (anchor references are not supported)"
        ));
    }
    if !valid_pointer_escapes(&pointer) {
        return Err(format!(
            "`#{pointer}` has an invalid `~` escape (use ~0 or ~1)"
        ));
    }
    if location.is_empty() {
        return Ok(Reference::Local { pointer });
    }
    let location = location.split_once('?').map_or(location, |(l, _)| l);
    let path = percent_decode(location)
        .ok_or_else(|| "the file path is not valid percent-encoded UTF-8".to_string())?;
    if path.is_empty() {
        return Ok(Reference::Local { pointer });
    }
    Ok(Reference::File { path, pointer })
}

/// The URI scheme of `location`, if it has one. Single letters are Windows
/// drive letters, not schemes.
fn scheme(location: &str) -> Option<&str> {
    let (scheme, _) = location.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (scheme.len() > 1
        && first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
}

fn valid_pointer_escapes(pointer: &str) -> bool {
    let bytes = pointer.as_bytes();
    bytes
        .iter()
        .enumerate()
        .all(|(i, &b)| b != b'~' || matches!(bytes.get(i + 1), Some(b'0' | b'1')))
}

/// Decode `%XX` sequences. `None` when a sequence is malformed or the
/// result is not UTF-8.
pub(crate) fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return Some(s.to_string());
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Resolve `.` and `..` without touching the file system. `..` never climbs
/// above the root of an absolute path.
pub(crate) fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// See [`Workspace::resolve`].
pub(crate) fn resolve(ws: &Workspace, from: DocId, reference: &str) -> Option<RefTarget> {
    let doc = ws.documents.get(from)?;
    let (target, pointer) = match parse_reference(reference).ok()? {
        Reference::Local { pointer } => (from, pointer),
        Reference::File { path, pointer } => {
            let base = doc.path.as_deref()?.parent()?;
            let file = lexical_normalize(&base.join(path));
            (*ws.files.get(&file)?, pointer)
        }
        Reference::Remote { .. } => return None,
    };
    let pointer = translate(&ws.documents.get(target)?.moves, &pointer);
    let t = RefTarget {
        doc: target,
        pointer,
    };
    ws.get(&t).map(|_| t)
}
