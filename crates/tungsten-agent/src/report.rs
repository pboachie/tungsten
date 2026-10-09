// SPDX-License-Identifier: AGPL-3.0-only
//! Diagnostics about the manifest (with line and column when its source is
//! known) and about spec extensions.

use tungsten_config::ManifestSource;
use tungsten_core::{Diagnostic, Diagnostics, Severity};

/// `parent` extended by one JSON Pointer token.
pub(crate) fn child(parent: &str, token: &str) -> String {
    tungsten_config::pointer::child(parent, token)
}

/// Collects diagnostics; manifest labels name `file`.
#[derive(Debug)]
pub(crate) struct Reporter<'a> {
    pub file: &'a str,
    pub source: Option<&'a ManifestSource>,
    pub out: Diagnostics,
}

impl<'a> Reporter<'a> {
    pub fn new(file: &'a str, source: Option<&'a ManifestSource>) -> Self {
        Self {
            file,
            source,
            out: Diagnostics::new(),
        }
    }

    /// A diagnostic about the manifest node at `pointer`.
    pub fn manifest(&mut self, severity: Severity, code: &str, pointer: &str, message: String) {
        let message = match self.source.and_then(|s| s.line_col(pointer)) {
            Some((line, col)) => format!("{message} (line {line}, column {col})"),
            None => message,
        };
        self.out
            .push(Diagnostic::new(code, severity, message).at(self.file, pointer, None));
    }

    pub fn error(&mut self, code: &str, pointer: &str, message: String) {
        self.manifest(Severity::Error, code, pointer, message);
    }

    pub fn warning(&mut self, code: &str, pointer: &str, message: String) {
        self.manifest(Severity::Warning, code, pointer, message);
    }

    /// A diagnostic about a node of a spec document.
    pub fn spec(
        &mut self,
        severity: Severity,
        code: &str,
        file: &str,
        pointer: &str,
        message: String,
    ) {
        self.out
            .push(Diagnostic::new(code, severity, message).at(file, pointer, None));
    }
}
