// SPDX-License-Identifier: AGPL-3.0-only
//! Structured indentation writer (planning/02 D1).

/// Comment syntax for doc comments and headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentStyle {
    /// `/** ... */` (TypeScript, JavaScript).
    JsDoc,
    /// `///` (Rust items).
    RustDoc,
    /// `//` (Rust, TypeScript line comments).
    DoubleSlash,
    /// `#` (Python, YAML, TOML).
    Hash,
    /// Triple-quoted docstring (Python), written at the current indentation.
    PyDocstring,
}

/// Builds source text line by line with tracked indentation. Lines never
/// carry trailing whitespace; output always ends with exactly one newline.
#[derive(Debug, Clone)]
pub struct Writer {
    indent_unit: String,
    depth: usize,
    out: String,
}

impl Writer {
    /// A writer indenting with `indent_unit` (for example two spaces).
    pub fn new(indent_unit: &str) -> Self {
        Self {
            indent_unit: indent_unit.to_string(),
            depth: 0,
            out: String::new(),
        }
    }

    /// Write one line at the current indentation. Embedded newlines are
    /// split into separate indented lines.
    pub fn line(&mut self, text: impl AsRef<str>) -> &mut Self {
        for l in text.as_ref().split('\n') {
            let l = l.trim_end();
            if l.is_empty() {
                self.out.push('\n');
            } else {
                for _ in 0..self.depth {
                    self.out.push_str(&self.indent_unit);
                }
                self.out.push_str(l);
                self.out.push('\n');
            }
        }
        self
    }

    /// An empty line (never two in a row).
    pub fn blank(&mut self) -> &mut Self {
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
        self
    }

    pub fn indent(&mut self) -> &mut Self {
        self.depth += 1;
        self
    }

    pub fn dedent(&mut self) -> &mut Self {
        self.depth = self.depth.saturating_sub(1);
        self
    }

    /// Write `open`, then run `body` one level deeper, then write `close`.
    pub fn block(
        &mut self,
        open: impl AsRef<str>,
        close: impl AsRef<str>,
        body: impl FnOnce(&mut Self),
    ) -> &mut Self {
        self.line(open);
        self.indent();
        body(self);
        self.dedent();
        self.line(close);
        self
    }

    /// Write a doc comment in `style`. Text is escaped so it cannot close the
    /// comment early (`*/` in JsDoc, `"""` in docstrings). Empty text writes
    /// nothing.
    pub fn doc(&mut self, style: CommentStyle, text: &str) -> &mut Self {
        let text = text.trim();
        if text.is_empty() {
            return self;
        }
        match style {
            CommentStyle::JsDoc => {
                let safe = text.replace("*/", "*\\/");
                self.line("/**");
                for l in safe.lines() {
                    self.line(format!(" * {l}"));
                }
                self.line(" */");
            }
            CommentStyle::RustDoc => {
                for l in text.lines() {
                    self.line(format!("/// {l}"));
                }
            }
            CommentStyle::DoubleSlash => {
                for l in text.lines() {
                    self.line(format!("// {l}"));
                }
            }
            CommentStyle::Hash => {
                for l in text.lines() {
                    self.line(format!("# {l}"));
                }
            }
            CommentStyle::PyDocstring => {
                let safe = text.replace('\\', "\\\\").replace("\"\"\"", "\\\"\\\"\\\"");
                self.line(format!("\"\"\"{safe}"));
                self.line("\"\"\"");
            }
        }
        self
    }

    /// The text written so far, ending with exactly one newline.
    pub fn finish(self) -> String {
        let mut s = self.out.trim_end_matches('\n').to_string();
        s.push('\n');
        s
    }
}
