// SPDX-License-Identifier: AGPL-3.0-only
//! `compact_doc`: the description agents see in tool schemas.
//!
//! The text is the summary followed by the description (without a leading
//! repeat of the summary). Markdown links keep their text, code spans and
//! emphasis their content, and whitespace collapses. The text is split into
//! sentences (a `.`, `!` or `?` followed by whitespace and an uppercase
//! letter, a digit or an opening quote or bracket, or a blank line);
//! sentences containing a `drop_phrases` entry are dropped, the first
//! `max_sentences` are kept and end with punctuation (a summary without a
//! full stop gets one), and the result is cut at a word boundary to
//! the token budget (a token is counted as four characters, rounded up),
//! ending with `…` when cut. When pruning leaves nothing, the first
//! sentence is used, cut to the budget, so an operation with any
//! documentation always has a compact description.

use tungsten_ir::Doc;

/// Sentences kept when the manifest does not say.
pub(crate) const DEFAULT_MAX_SENTENCES: usize = 2;

#[derive(Debug, Clone)]
pub(crate) struct Prune<'a> {
    pub max_sentences: usize,
    pub drop_phrases: &'a [String],
    pub budget_tokens: usize,
}

/// Approximate token count: characters / 4, rounded up.
pub(crate) fn tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

pub(crate) fn compact(doc: Option<&Doc>, p: &Prune<'_>) -> String {
    let Some(doc) = doc else {
        return String::new();
    };
    let summary = doc.summary.as_deref().map(plain).unwrap_or_default();
    let description = doc.description.as_deref().map(plain).unwrap_or_default();
    let mut sentences = sentences(&summary);
    for s in self::sentences(&description) {
        if !sentences.contains(&s) {
            sentences.push(s);
        }
    }
    let Some(first) = sentences.first().cloned() else {
        return String::new();
    };
    let kept: Vec<String> = sentences
        .into_iter()
        .filter(|s| {
            !p.drop_phrases
                .iter()
                .any(|ph| !ph.is_empty() && s.contains(ph.as_str()))
        })
        .take(p.max_sentences.max(1))
        .collect();
    let text = if kept.is_empty() {
        terminated(first)
    } else {
        kept.into_iter()
            .map(terminated)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let budget = p.budget_tokens.max(1);
    if tokens(&text) <= budget {
        text
    } else {
        cut(&text, budget * 4)
    }
}

/// A sentence ending in punctuation; a summary without a full stop gets
/// one, so it reads as a sentence before the description.
fn terminated(mut sentence: String) -> String {
    if !sentence.ends_with(['.', '!', '?', '…', ':', ';']) {
        sentence.push('.');
    }
    sentence
}

/// Cut to at most `max_chars` characters at a word boundary, marking the
/// cut with `…`.
fn cut(text: &str, max_chars: usize) -> String {
    let room = max_chars.saturating_sub(1);
    let head: String = text.chars().take(room).collect();
    let at_word_end = text.chars().nth(room).is_none_or(char::is_whitespace);
    let head = match head.rfind(char::is_whitespace) {
        Some(i) if i > 0 && !at_word_end => head[..i].trim_end().to_string(),
        _ => head,
    };
    let head = head.trim_end_matches([',', ';', ':', '.', ' ']);
    format!("{head}…")
}

/// Markdown reduced to plain text: links and images keep their text, code
/// spans and emphasis markers are dropped, headings and list markers lose
/// their prefix, and blank lines become paragraph breaks (`\n\n`) while
/// other whitespace collapses to one space.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut paragraph = String::new();
    for line in text.lines().chain(std::iter::once("")) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            let p = inline(&paragraph);
            if !p.is_empty() {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&p);
            }
            paragraph.clear();
            continue;
        }
        let trimmed = trimmed.trim_start_matches('#').trim_start();
        let trimmed = ["- ", "* ", "+ "]
            .iter()
            .find_map(|m| trimmed.strip_prefix(m))
            .unwrap_or(trimmed);
        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
    }
    out
}

/// Nesting of link labels processed as markdown; deeper labels are text.
const MAX_LINK_DEPTH: usize = 8;

/// Positions computed once per paragraph so inline parsing stays linear:
/// every scan the parser needs (a link's closing bracket, its target's
/// end, a code span's closing run) is a lookup instead of a search to the
/// end of the paragraph. A paragraph of `[` characters, or of unclosed
/// links, used to take quadratic time on every compile.
struct Marks {
    /// For each `[`, its matching `]` (bracket depth, as written).
    close: Vec<Option<usize>>,
    /// For each index, the next `)` at or after it.
    next_paren: Vec<Option<usize>>,
    /// For each index, the next `]` at or after it.
    next_bracket: Vec<Option<usize>>,
    /// Starts of backtick runs by run length, ascending.
    runs: std::collections::BTreeMap<usize, Vec<usize>>,
}

impl Marks {
    fn new(chars: &[char]) -> Marks {
        let n = chars.len();
        let mut close = vec![None; n];
        let mut open: Vec<usize> = vec![];
        for (j, &c) in chars.iter().enumerate() {
            match c {
                '[' => open.push(j),
                ']' => {
                    if let Some(o) = open.pop() {
                        close[o] = Some(j);
                    }
                }
                _ => {}
            }
        }
        let mut next_paren = vec![None; n + 1];
        let mut next_bracket = vec![None; n + 1];
        for j in (0..n).rev() {
            next_paren[j] = if chars[j] == ')' {
                Some(j)
            } else {
                next_paren[j + 1]
            };
            next_bracket[j] = if chars[j] == ']' {
                Some(j)
            } else {
                next_bracket[j + 1]
            };
        }
        let mut runs: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
        let mut j = 0;
        while j < n {
            if chars[j] == '`' {
                let len = chars[j..].iter().take_while(|&&c| c == '`').count();
                runs.entry(len).or_default().push(j);
                j += len;
            } else {
                j += 1;
            }
        }
        Marks {
            close,
            next_paren,
            next_bracket,
            runs,
        }
    }

    /// The first backtick run of exactly `ticks` starting at or after `from`.
    fn code_close(&self, from: usize, ticks: usize) -> Option<usize> {
        let starts = self.runs.get(&ticks)?;
        starts.get(starts.partition_point(|&s| s < from)).copied()
    }
}

/// Inline markdown of one paragraph.
fn inline(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let marks = Marks::new(&chars);
    let mut out = String::with_capacity(text.len());
    inline_range(&chars, &marks, 0, chars.len(), 0, &mut out);
    out.trim().to_string()
}

/// Inline markdown of `chars[from..to]`, appended to `out`. `depth` is the
/// number of link labels this range is nested in.
fn inline_range(
    chars: &[char],
    marks: &Marks,
    from: usize,
    to: usize,
    depth: usize,
    out: &mut String,
) {
    let mut i = from;
    while i < to {
        let c = chars[i];
        match c {
            '`' => {
                let ticks = chars[i..to].iter().take_while(|&&c| c == '`').count();
                match marks
                    .code_close(i + ticks, ticks)
                    .filter(|&j| j + ticks <= to)
                {
                    Some(j) => {
                        let inner: String = chars[i + ticks..j].iter().collect();
                        out.push_str(inner.trim());
                        i = j + ticks;
                    }
                    None => {
                        out.extend(&chars[i..i + ticks]);
                        i += ticks;
                    }
                }
            }
            '!' if i + 1 < to && chars[i + 1] == '[' => i += 1,
            '[' => match link(chars, marks, i, to).filter(|_| depth < MAX_LINK_DEPTH) {
                Some((label_end, next)) => {
                    let mut label = String::new();
                    inline_range(chars, marks, i + 1, label_end, depth + 1, &mut label);
                    out.push_str(label.trim());
                    i = next;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            },
            '*' | '_' if emphasis(chars, i) => i += 1,
            c if c.is_whitespace() => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
}

/// `[label](target)` or `[label][ref]` starting at `start` and ending
/// before `to`: the end of the label (its `]`) and the index after the link.
fn link(chars: &[char], marks: &Marks, start: usize, to: usize) -> Option<(usize, usize)> {
    let end = marks.close[start].filter(|&e| e < to)?;
    let target_end = match chars.get(end + 1) {
        Some('(') if end + 2 <= to => marks.next_paren[end + 2],
        Some('[') if end + 2 <= to => marks.next_bracket[end + 2],
        _ => return None,
    }
    .filter(|&j| j < to)?;
    Some((end, target_end + 1))
}

/// Whether the `*` or `_` at `i` is an emphasis marker: doubled, or
/// attached to text on one side only. Underscores inside words
/// (`recipient_e164`) and a `*` between spaces stay.
fn emphasis(chars: &[char], i: usize) -> bool {
    let c = chars[i];
    let before = i.checked_sub(1).map(|j| chars[j]);
    let after = chars.get(i + 1).copied();
    if before == Some(c) || after == Some(c) {
        return true;
    }
    let solid = |x: Option<char>| match c {
        '_' => x.is_some_and(char::is_alphanumeric),
        _ => x.is_some_and(|x| !x.is_whitespace()),
    };
    solid(before) != solid(after)
}

/// Sentences of plain text, trimmed, without empty ones.
fn sentences(text: &str) -> Vec<String> {
    let mut out = vec![];
    for paragraph in text.split("\n\n") {
        let chars: Vec<char> = paragraph.chars().collect();
        let mut start = 0;
        for i in 0..chars.len() {
            if !matches!(chars[i], '.' | '!' | '?') {
                continue;
            }
            let next = chars.get(i + 1);
            let after = chars.get(i + 2);
            let boundary = next.is_some_and(|c| c.is_whitespace())
                && after.is_some_and(|c| {
                    c.is_uppercase()
                        || c.is_ascii_digit()
                        || matches!(c, '"' | '\'' | '(' | '[' | '`')
                });
            if boundary {
                push(&mut out, &chars[start..=i]);
                start = i + 1;
            }
        }
        push(&mut out, &chars[start..]);
    }
    out
}

fn push(out: &mut Vec<String>, chars: &[char]) {
    let s: String = chars.iter().collect();
    let s = s.trim();
    if !s.is_empty() {
        out.push(s.to_string());
    }
}
