// SPDX-License-Identifier: Apache-2.0
//! Schema patterns a validator could not check. A pattern the SDK's regex
//! engine cannot compile accepts the value (the server still validates it);
//! the validator notes it here, and the runtime reports each one as a
//! `PATTERN_UNCHECKED` diagnostic through `on_diagnostic`. Outside a call
//! (a validator run directly) nothing is collected and nothing is reported.

use std::cell::RefCell;

use crate::client::ClientCore;
use crate::envelope::Diag;
use crate::types::{Category, PathSegment, Retryable};

/// A schema `pattern` that was not checked: where, which pattern and why.
/// Never carries the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternNote {
    pub path: Vec<PathSegment>,
    pub pattern: String,
    pub reason: String,
}

thread_local! {
    static NOTES: RefCell<Option<Vec<PatternNote>>> = const { RefCell::new(None) };
}

/// Called by generated validators for a pattern they could not compile. A
/// no-op unless the runtime is collecting for the current call.
pub fn note_unchecked_pattern(path: &[PathSegment], pattern: &str, reason: &str) {
    NOTES.with(|notes| {
        if let Some(list) = notes.borrow_mut().as_mut() {
            list.push(PatternNote {
                path: path.to_vec(),
                pattern: pattern.to_owned(),
                reason: reason.to_owned(),
            });
        }
    });
}

/// Run `f`, collecting the patterns it could not check (also when it
/// panics: the notes are dropped with the unwinding).
pub fn collect<R>(f: impl FnOnce() -> R) -> (R, Vec<PatternNote>) {
    struct Restore(Option<Vec<PatternNote>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            NOTES.with(|notes| *notes.borrow_mut() = previous);
        }
    }
    let previous = NOTES.with(|notes| notes.borrow_mut().replace(Vec::new()));
    let restore = Restore(previous);
    let result = f();
    let notes = NOTES
        .with(|notes| notes.borrow_mut().take())
        .unwrap_or_default();
    drop(restore);
    (result, notes)
}

/// A path below `response` or `events[n]` as the diagnostics write it: an
/// all-digit segment is an index (`[0]`), any other is a member (`.name`).
pub(crate) fn path_text(path: &[PathSegment]) -> String {
    path.iter()
        .map(|segment| match segment {
            PathSegment::Index(n) => format!("[{n}]"),
            PathSegment::Key(k) if !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()) => {
                format!("[{k}]")
            }
            PathSegment::Key(k) => format!(".{k}"),
        })
        .collect()
}

impl ClientCore {
    /// Report the unchecked patterns of one validation: once per path and
    /// pattern, never with the value. `where_` renders a path as the
    /// diagnostic's `failed_parameter`.
    pub(crate) fn report_unchecked(
        &self,
        operation: &str,
        category: Category,
        notes: &[PatternNote],
        where_: impl Fn(&[PathSegment]) -> String,
    ) {
        let mut seen: Vec<(&[PathSegment], &str)> = Vec::new();
        for note in notes {
            if seen.len() >= 16 {
                break;
            }
            let key = (note.path.as_slice(), note.pattern.as_str());
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let param = where_(&note.path);
            let diagnostic = Diag::new(operation.to_owned(), category)
                .code(Some("PATTERN_UNCHECKED".to_owned()))
                .retryable(Retryable::Never)
                .failed_parameter(param.clone())
                .expected(format!("a value matching the pattern /{}/", note.pattern))
                .remediation(format!(
                    "The pattern /{}/ at {param} was not checked because {}; the value was accepted without it. The server still validates it.",
                    note.pattern, note.reason
                ))
                .build();
            self.emit(&diagnostic);
        }
    }
}
