// SPDX-License-Identifier: AGPL-3.0-only
//! Opt-in, user-supplied support-bundle attachments (`--attach-log`,
//! `--attach-config`), recorded only in bundle schema version 2.
//!
//! Each file is read as one bounded, regular, nonsymlink file; only its last
//! `MAX_ATTACHMENT_BYTES` are kept, starting at a line boundary so that a
//! secret is never cut into an unrecognizable fragment. The text is then
//! redacted: environment values, credential-like assignments, authorization
//! schemes, private-key blocks, URLs, user names in home paths and long
//! token-like strings. The path is never recorded. Redaction reduces risk; it
//! is not a guarantee that arbitrary project text is safe to share, and the
//! bundle labels every attachment as user supplied.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

/// At most this many attachments in one bundle.
pub(crate) const MAX_ATTACHMENTS: usize = 4;
/// Kept tail of each attached file, before redaction.
pub(crate) const MAX_ATTACHMENT_BYTES: usize = 8 * 1024;
/// Largest attachment text a version 2 reader accepts after redaction.
pub(crate) const MAX_ATTACHMENT_TEXT_BYTES: usize = 12 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Log,
    Configuration,
}

/// The payload of a `user.attachment[N]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AttachmentPayload {
    /// What the user said the file is.
    pub kind: AttachmentKind,
    /// Size of the file when read; its path is never recorded.
    pub source_bytes: u64,
    /// True when only the end of the file is kept.
    pub truncated: bool,
    /// Number of replacements made by automatic redaction.
    pub redactions: usize,
    /// The redacted text. User supplied: review it before sharing.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachError {
    /// Not a readable regular, nonsymlink file.
    Unreadable,
}

/// Entry id of the attachment at `index`.
pub(crate) fn entry_id(index: usize) -> String {
    format!("user.attachment[{index}]")
}

pub(crate) fn read(
    path: &Path,
    kind: AttachmentKind,
    secrets: &Secrets,
) -> Result<AttachmentPayload, AttachError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| AttachError::Unreadable)?;
    if !metadata.is_file() {
        return Err(AttachError::Unreadable);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never follow a swapped-in symlink or block on a swapped-in FIFO.
        options
            .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let mut file = options.open(path).map_err(|_| AttachError::Unreadable)?;
    let metadata = file.metadata().map_err(|_| AttachError::Unreadable)?;
    if !metadata.is_file() {
        return Err(AttachError::Unreadable);
    }
    let source_bytes = metadata.len();
    let start = source_bytes.saturating_sub(MAX_ATTACHMENT_BYTES as u64);
    file.seek(SeekFrom::Start(start))
        .map_err(|_| AttachError::Unreadable)?;
    let mut bytes = Vec::new();
    file.take(MAX_ATTACHMENT_BYTES as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| AttachError::Unreadable)?;
    let mut truncated = start > 0;
    if truncated {
        // Drop the partial first line.
        let cut = bytes
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |n| n + 1);
        bytes.drain(..cut);
    }
    let text = String::from_utf8_lossy(&bytes);
    let (mut text, redactions) = redact(&text, secrets);
    // Markers can be longer than what they replace; keep whole lines from the
    // end within the reader's bound.
    while text.len() > MAX_ATTACHMENT_TEXT_BYTES {
        truncated = true;
        let cut = text.find('\n').map_or(text.len(), |n| n + 1);
        text.drain(..cut);
    }
    Ok(AttachmentPayload {
        kind,
        source_bytes,
        truncated,
        redactions,
        text,
    })
}

/// Values that must never appear in an attachment: the process environment
/// values long enough to be recognizable.
#[derive(Debug, Default)]
pub(crate) struct Secrets {
    values: Vec<String>,
}

impl Secrets {
    pub(crate) fn from_environment() -> Self {
        let mut values: Vec<String> = std::env::vars_os()
            .filter_map(|(_, value)| value.into_string().ok())
            .filter(|value| value.trim().len() >= 6)
            .collect();
        // Longest first, so a value containing another is replaced whole.
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();
        Self { values }
    }
}

const SECRET_KEYS: [&str; 16] = [
    "password",
    "passwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "api-key",
    "access_key",
    "access-key",
    "private_key",
    "private-key",
    "credential",
    "authorization",
    "cookie",
    "session",
    "signature",
];
const SCHEMES: [&str; 3] = ["bearer ", "basic ", "token "];
const REDACTED: &str = "[redacted]";

/// Redacts `text` line by line and returns it with the replacement count.
pub(crate) fn redact(text: &str, secrets: &Secrets) -> (String, usize) {
    let mut count = 0;
    let mut out = String::with_capacity(text.len());
    let mut in_key = false;
    for line in text.split_inclusive('\n') {
        let line = line.trim_end_matches(['\n', '\r']);
        let lower = line.to_ascii_lowercase();
        if lower.contains("-----begin") && lower.contains("private key") {
            in_key = true;
            count += 1;
            out.push_str("[redacted private key]\n");
            continue;
        }
        if in_key {
            if lower.contains("-----end") {
                in_key = false;
            }
            continue;
        }
        let mut line: String = line
            .chars()
            .map(|c| if c.is_control() && c != '\t' { '?' } else { c })
            .collect();
        for value in &secrets.values {
            if line.contains(value.as_str()) {
                count += line.matches(value.as_str()).count();
                line = line.replace(value.as_str(), "[redacted env]");
            }
        }
        line = redact_urls(&line, &mut count);
        line = redact_assignments(&line, &mut count);
        line = redact_home_users(&line, &mut count);
        line = redact_tokens(&line, &mut count);
        out.push_str(&line);
        out.push('\n');
    }
    if !text.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    (out, count)
}

/// Replaces `scheme://...` up to whitespace or a quote.
fn redact_urls(line: &str, count: &mut usize) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("://") {
        let scheme_start = rest[..at]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || "+.-".contains(c)))
            .map_or(0, |n| n + 1);
        if scheme_start == at {
            out.push_str(&rest[..at + 3]);
            rest = &rest[at + 3..];
            continue;
        }
        let end = rest[at + 3..]
            .find(|c: char| c.is_whitespace() || "\"'<>`".contains(c))
            .map_or(rest.len(), |n| at + 3 + n);
        out.push_str(&rest[..at + 3]);
        out.push_str(REDACTED);
        *count += 1;
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Replaces the value of `key = value`, `key: value` and `"key": "value"`
/// where the key names a credential, and the credential after an
/// authorization scheme such as `Bearer`.
fn redact_assignments(line: &str, count: &mut usize) -> String {
    let lower = line.to_ascii_lowercase();
    let bytes = line.as_bytes();
    let mut spans: Vec<(usize, usize)> = vec![];
    for key in SECRET_KEYS {
        for (at, _) in lower.match_indices(key) {
            let mut i = at + key.len();
            // Allow longer key names such as `client_secret_value` or
            // `x-api-key`, and a closing quote.
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || b"_-.".contains(&bytes[i]))
            {
                i += 1;
            }
            while i < bytes.len() && b"\"' \t".contains(&bytes[i]) {
                i += 1;
            }
            if i >= bytes.len() || !b":=".contains(&bytes[i]) {
                continue;
            }
            i += 1;
            if i < bytes.len() && bytes[i] == b'>' {
                i += 1;
            }
            while i < bytes.len() && b" \t".contains(&bytes[i]) {
                i += 1;
            }
            spans.push(value_span(bytes, i));
        }
    }
    for scheme in SCHEMES {
        for (at, _) in lower.match_indices(scheme) {
            let start = at + scheme.len();
            spans.push(value_span(bytes, start));
        }
    }
    replace_spans(line, spans, REDACTED, count)
}

/// A quoted value without its quotes, or an unquoted one up to a separator.
fn value_span(bytes: &[u8], start: usize) -> (usize, usize) {
    match bytes.get(start) {
        Some(&quote) if quote == b'"' || quote == b'\'' => {
            let end = bytes[start + 1..]
                .iter()
                .position(|&b| b == quote)
                .map_or(bytes.len(), |n| start + 1 + n);
            (start + 1, end)
        }
        _ => {
            let end = bytes[start..]
                .iter()
                .position(|b| b.is_ascii_whitespace() || b",;&\"'".contains(b))
                .map_or(bytes.len(), |n| start + n);
            (start, end)
        }
    }
}

/// Replaces the user name after `/home/`, `/Users/` and `\Users\`.
fn redact_home_users(line: &str, count: &mut usize) -> String {
    let bytes = line.as_bytes();
    let mut spans = vec![];
    for prefix in ["/home/", "/Users/", "\\Users\\", "/users/", "\\users\\"] {
        for (at, _) in line.match_indices(prefix) {
            let start = at + prefix.len();
            let end = bytes[start..]
                .iter()
                .position(|b| b"/\\\"' \t:".contains(b))
                .map_or(bytes.len(), |n| start + n);
            spans.push((start, end));
        }
    }
    replace_spans(line, spans, "[user]", count)
}

/// Replaces runs of 32 or more token characters that mix letters and digits:
/// keys, digests and session identifiers.
fn redact_tokens(line: &str, count: &mut usize) -> String {
    let bytes = line.as_bytes();
    let token = |b: u8| b.is_ascii_alphanumeric() || b"+/=_-.".contains(&b);
    let mut spans = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if !token(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && token(bytes[i]) {
            i += 1;
        }
        let run = &bytes[start..i];
        if run.len() >= 32
            && run.iter().any(u8::is_ascii_digit)
            && run.iter().any(u8::is_ascii_alphabetic)
            // Paths are handled separately and are mostly not secrets.
            && run.iter().filter(|&&b| b == b'/').count() < 3
        {
            spans.push((start, i));
        }
    }
    replace_spans(line, spans, REDACTED, count)
}

/// Replaces nonempty byte spans (on char boundaries) with `marker`;
/// overlapping spans are merged and earlier markers are left alone.
fn replace_spans(
    line: &str,
    mut spans: Vec<(usize, usize)>,
    marker: &str,
    count: &mut usize,
) -> String {
    spans.retain(|&(start, end)| {
        end > start
            && line.is_char_boundary(start)
            && line.is_char_boundary(end)
            && !line[start..end].starts_with("[redacted")
            && &line[start..end] != "[user]"
    });
    if spans.is_empty() {
        return line.to_string();
    }
    spans.sort_unstable();
    let mut out = String::with_capacity(line.len());
    let mut cursor = 0;
    for (start, end) in spans {
        if end <= cursor {
            continue;
        }
        let start = start.max(cursor);
        out.push_str(&line[cursor..start]);
        out.push_str(marker);
        *count += 1;
        cursor = end;
    }
    out.push_str(&line[cursor..]);
    out
}
