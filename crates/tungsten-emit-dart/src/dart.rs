// SPDX-License-Identifier: AGPL-3.0-only
//! Dart source helpers: string and JSON literals, doc comments, and a line
//! writer that lays expressions out the way `dart format` does.
//!
//! Generated packages declare the language version 3.6, so `dart format`
//! uses its short style, which keeps a line that fits in 80 columns as it
//! is and keeps every argument list that ends with a trailing comma split,
//! one argument per line. [`Code::expr`] writes an expression on one line
//! when it fits and otherwise splits the outermost call with a trailing
//! comma (recursively), so the formatter has nothing to change.

use serde_json::Value;

/// The line width of `dart format`.
pub const WIDTH: usize = 80;

/// A single-quoted Dart string literal of `text`: `\`, `'` and `$` (which
/// would start an interpolation) are escaped, control characters are
/// written as escapes, everything else as is.
pub fn str_lit(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// A Dart expression for a JSON value: a literal for `null`, booleans,
/// numbers and strings, `Wire.literal('<json>')` for arrays and objects.
pub fn json_lit(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
                    format!("{}", f as i64)
                } else {
                    format!("{f:?}")
                }
            }
        }
        Value::String(s) => str_lit(s),
        Value::Array(_) | Value::Object(_) => {
            format!("Wire.literal({})", str_lit(&v.to_string()))
        }
    }
}

/// A constant Dart expression for a JSON value (an enum constant's
/// argument): arrays and objects are collection literals.
pub fn const_lit(v: &Value) -> String {
    match v {
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(const_lit).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(members) => {
            let members: Vec<String> = members
                .iter()
                .map(|(k, v)| format!("{}: {}", str_lit(k), const_lit(v)))
                .collect();
            format!("{{{}}}", members.join(", "))
        }
        other => json_lit(other),
    }
}

/// Text collapsed to one line.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `word` occurs in `text` as a whole Dart identifier (`$` and `_`
/// are identifier characters; `a.Word` counts, since a prefixed name uses
/// the word).
pub fn contains_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(at) = text[from..].find(word) {
        let start = from + at;
        let end = start + word.len();
        let before = start == 0 || !is_ident(bytes[start - 1]);
        let after = end >= bytes.len() || !is_ident(bytes[end]);
        if before && after {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// An expression the writer can split like `dart format`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// Written as is, never split.
    Raw(String),
    /// `callee(args)`; split one argument per line with a trailing comma
    /// when it does not fit.
    Call(String, Vec<Expr>),
    /// A named argument `name: value`.
    Named(String, Box<Expr>),
}

/// Whether a source line ends at this character; a doc comment must not
/// continue past it.
fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

impl Expr {
    pub fn raw(text: impl Into<String>) -> Self {
        Expr::Raw(text.into())
    }

    pub fn call(callee: impl Into<String>, args: Vec<Expr>) -> Self {
        Expr::Call(callee.into(), args)
    }

    pub fn named(name: impl Into<String>, value: Expr) -> Self {
        Expr::Named(name.into(), Box::new(value))
    }

    /// The expression on one line.
    pub fn flat(&self) -> String {
        match self {
            Expr::Raw(s) => s.clone(),
            Expr::Call(callee, args) => {
                let args: Vec<String> = args.iter().map(Expr::flat).collect();
                format!("{callee}({})", args.join(", "))
            }
            Expr::Named(name, value) => format!("{name}: {}", value.flat()),
        }
    }
}

/// Dart source, line by line, indented by two spaces per level.
#[derive(Debug, Clone, Default)]
pub struct Code {
    lines: Vec<String>,
    level: usize,
}

impl Code {
    pub fn new() -> Self {
        Self::default()
    }

    fn width(&self) -> usize {
        self.level * 2
    }

    /// Whether `text` fits on a line at the current indentation.
    pub fn fits(&self, text: &str) -> bool {
        self.width() + text.chars().count() <= WIDTH
    }

    /// One line at the current indentation (an empty text is a blank line).
    pub fn line(&mut self, text: impl AsRef<str>) {
        let text = text.as_ref().trim_end();
        if text.is_empty() {
            self.lines.push(String::new());
        } else {
            self.lines
                .push(format!("{}{text}", " ".repeat(self.width())));
        }
    }

    /// A blank line, never two in a row and never first.
    pub fn blank(&mut self) {
        if self.lines.last().is_some_and(|l| !l.is_empty()) {
            self.lines.push(String::new());
        }
    }

    pub fn indent(&mut self) {
        self.level += 1;
    }

    pub fn dedent(&mut self) {
        self.level = self.level.saturating_sub(1);
    }

    /// `prefix`, the expression, `suffix`: on one line when it fits, else
    /// with the outermost call split one argument per line with a trailing
    /// comma, each argument laid out the same way.
    pub fn expr(&mut self, prefix: &str, e: &Expr, suffix: &str) {
        let flat = format!("{prefix}{}{suffix}", e.flat());
        if self.fits(&flat) {
            self.line(flat);
            return;
        }
        match e {
            Expr::Call(callee, args) if !args.is_empty() => {
                self.line(format!("{prefix}{callee}("));
                self.indent();
                for a in args {
                    self.expr("", a, ",");
                }
                self.dedent();
                self.line(format!("){suffix}"));
            }
            Expr::Named(name, value) => self.expr(&format!("{prefix}{name}: "), value, suffix),
            _ => self.line(flat),
        }
    }

    /// An element of a collection literal guarded by `if (cond)`: on one
    /// line when it fits, else the condition on its own line and the
    /// element one level deeper.
    pub fn if_element(&mut self, cond: &str, prefix: &str, e: &Expr, suffix: &str) {
        let flat = format!("if ({cond}) {prefix}{}{suffix}", e.flat());
        if self.fits(&flat) {
            self.line(flat);
            return;
        }
        self.line(format!("if ({cond})"));
        self.indent();
        self.expr(prefix, e, suffix);
        self.dedent();
    }

    /// `/// ` doc comment lines of a text; blank lines of the text become
    /// `///`.
    pub fn doc(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let text = text.replace("\r\n", "\n");
        for l in text.split(is_line_terminator) {
            let l = l.trim_end();
            if l.is_empty() {
                self.line("///");
            } else {
                self.line(format!("/// {l}"));
            }
        }
    }

    /// A class header (`final class X implements Y {`): on one line when it
    /// fits, else the clause on the next line indented by four spaces, as
    /// `dart format` does.
    pub fn class_header(&mut self, head: &str, clause: &str) {
        let flat = format!("{head} {clause} {{");
        if self.fits(&flat) {
            self.line(flat);
        } else {
            self.line(head);
            self.line(format!("    {clause} {{"));
        }
    }

    /// The source, ending with exactly one newline.
    pub fn finish(self) -> String {
        let mut s = self.lines.join("\n").trim_end_matches('\n').to_string();
        s.push('\n');
        s
    }
}

/// An `import` or `export` directive with a `show` or `hide` combinator
/// (empty `names`: none), laid out like `dart format`: one line, else the
/// combinator on the next line, else one name per line.
pub fn directive(keyword: &str, uri: &str, combinator: &str, names: &[String]) -> String {
    let head = format!("{keyword} {}", str_lit(uri));
    if names.is_empty() {
        return format!("{head};");
    }
    let list = names.join(", ");
    let flat = format!("{head} {combinator} {list};");
    if flat.chars().count() <= WIDTH {
        return flat;
    }
    let second = format!("    {combinator} {list};");
    if second.chars().count() <= WIDTH {
        return format!("{head}\n{second}");
    }
    let mut out = format!("{head}\n    {combinator}");
    for (i, n) in names.iter().enumerate() {
        out.push_str(&format!(
            "\n        {n}{}",
            if i + 1 == names.len() { ";" } else { "," }
        ));
    }
    out
}
