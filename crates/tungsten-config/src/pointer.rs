// SPDX-License-Identifier: AGPL-3.0-only
//! JSON Pointer (RFC 6901) helpers.

/// `parent` extended by one reference token, escaped (`~` → `~0`,
/// `/` → `~1`).
pub(crate) fn child(parent: &str, token: &str) -> String {
    let mut out = String::with_capacity(parent.len() + token.len() + 1);
    out.push_str(parent);
    out.push('/');
    for c in token.chars() {
        match c {
            '~' => out.push_str("~0"),
            '/' => out.push_str("~1"),
            c => out.push(c),
        }
    }
    out
}

/// A pointer from unescaped reference tokens.
pub(crate) fn from_tokens<'a>(tokens: impl IntoIterator<Item = &'a str>) -> String {
    tokens
        .into_iter()
        .fold(String::new(), |acc, token| child(&acc, token))
}

/// The pointer of the parent node, or `None` for the root.
pub(crate) fn parent(pointer: &str) -> Option<&str> {
    pointer.rfind('/').map(|i| &pointer[..i])
}
