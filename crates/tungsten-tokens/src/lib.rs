// SPDX-License-Identifier: AGPL-3.0-only
//! Token counting for agent-facing budgets (planning/01 NFR-3,
//! planning/05 "MCP server").
//!
//! Budgets are enforced with a deterministic estimate so the compiler has no
//! model-specific dependency; the test harness cross-checks the estimate
//! against a real BPE tokenizer (`cl100k_base`).
//!
//! The estimate mirrors how a BPE tokenizer works. Text is first split into
//! pieces exactly as `cl100k_base`'s pre-tokenizer splits it (contractions,
//! letter runs with one leading non-letter, runs of at most three digits,
//! punctuation runs, newline runs, space runs); a BPE never merges across
//! pieces, so the count is a sum over pieces. Each piece is then costed
//! from its shape: a digit group or contraction is one token; a letter run
//! is split into words at case boundaries (`submitAlphaMessage`) and each
//! word is one token when a lexicon of common English and API words knows
//! it, else costs by length, case and what precedes it; punctuation is
//! matched against runs known to be one token (`":{"`, `"},"`, `://`);
//! whitespace and other characters cost by length. Costs are kept in
//! thousandths of a token and rounded up once, so the result is an integer
//! that depends only on the text.
//!
//! The cost tables are calibrated on generated `tools.json`, `llms.txt`,
//! `llms-full.txt` and JSON Schemas so that the estimate is within 10% of
//! `cl100k_base` and leans high: a budget checked with the estimate is
//! safe.
//!
//! PHASE-3 CONTRACT: `count` and `Counter` are shared by the docs and MCP
//! emitters and the report.

mod lexicon;

/// How tokens are counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Counter {
    /// `ceil(chars / 4)`: the historical approximation.
    CharsDiv4,
    /// The calibrated deterministic estimate used for budgets.
    #[default]
    Estimate,
}

impl Counter {
    /// Stable name of the counter, recorded next to counts in generated
    /// files (for example the MCP manifest's `tokenCounter`).
    pub fn id(self) -> &'static str {
        match self {
            Counter::CharsDiv4 => "chars-div-4",
            Counter::Estimate => "tungsten-estimate-v1",
        }
    }
}

/// Number of tokens in `text` under `counter`.
pub fn count(text: &str, counter: Counter) -> usize {
    match counter {
        Counter::CharsDiv4 => text.chars().count().div_ceil(4),
        Counter::Estimate => estimate(text),
    }
}

/// One cost unit is a thousandth of a token.
const UNIT: u64 = 1000;

fn estimate(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut milli: u64 = 0;
    let mut i = 0;
    while i < chars.len() {
        let (kind, end) = next_piece(&chars, i);
        milli += cost(kind, &chars[i..end]);
        i = end;
    }
    // Lean high by 3% so that a budget checked with the estimate is safe.
    let milli = milli.saturating_mul(103) / 100;
    usize::try_from(milli.div_ceil(UNIT)).unwrap_or(usize::MAX)
}

/// The class of a pre-tokenizer piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Piece {
    /// `'s`, `'t`, `'re`, `'ve`, `'m`, `'ll`, `'d`.
    Contraction,
    /// Letters, optionally after one character that is not a letter, digit
    /// or line break (` word`, `_id`, `.json`).
    Letters,
    /// One to three digits.
    Digits,
    /// Punctuation and symbols, optionally after one space, with the line
    /// breaks that follow.
    Punct,
    /// Whitespace ending in line breaks.
    Newlines,
    /// Other whitespace.
    Spaces,
}

fn is_letter(c: char) -> bool {
    c.is_alphabetic()
}

fn is_digit(c: char) -> bool {
    c.is_numeric()
}

fn is_break(c: char) -> bool {
    c == '\r' || c == '\n'
}

/// Not whitespace, letter or digit.
fn is_punct(c: char) -> bool {
    !c.is_whitespace() && !is_letter(c) && !is_digit(c)
}

/// The piece starting at `i` and where it ends, following the
/// `cl100k_base` pattern
/// `'s|'t|'re|'ve|'m|'ll|'d | [^\r\n\pL\pN]?\pL+ | \pN{1,3} | ' '?[^\s\pL\pN]+[\r\n]* | \s*[\r\n]+ | \s+(?!\S) | \s+`
/// (contractions case-insensitive), alternatives tried in that order.
fn next_piece(chars: &[char], i: usize) -> (Piece, usize) {
    let at = |k: usize| chars.get(k).copied();
    let c = chars[i];
    if c == '\'' {
        let lower = |k: usize| at(k).map(|c| c.to_ascii_lowercase());
        match (lower(i + 1), lower(i + 2)) {
            (Some('r'), Some('e')) | (Some('v'), Some('e')) | (Some('l'), Some('l')) => {
                return (Piece::Contraction, i + 3);
            }
            (Some('s' | 't' | 'm' | 'd'), _) => return (Piece::Contraction, i + 2),
            _ => {}
        }
    }
    let letters_from = |start: usize| {
        let mut end = start;
        while at(end).is_some_and(is_letter) {
            end += 1;
        }
        end
    };
    if is_letter(c) {
        return (Piece::Letters, letters_from(i));
    }
    if !is_break(c) && !is_digit(c) && at(i + 1).is_some_and(is_letter) {
        return (Piece::Letters, letters_from(i + 1));
    }
    if is_digit(c) {
        let mut end = i + 1;
        while end < i + 3 && at(end).is_some_and(is_digit) {
            end += 1;
        }
        return (Piece::Digits, end);
    }
    let punct_start = if c == ' ' && at(i + 1).is_some_and(is_punct) {
        Some(i + 1)
    } else if is_punct(c) {
        Some(i)
    } else {
        None
    };
    if let Some(start) = punct_start {
        let mut end = start;
        while at(end).is_some_and(is_punct) {
            end += 1;
        }
        while at(end).is_some_and(is_break) {
            end += 1;
        }
        return (Piece::Punct, end);
    }
    // Whitespace from here on.
    let mut run_end = i;
    while at(run_end).is_some_and(char::is_whitespace) {
        run_end += 1;
    }
    if let Some(last_break) = (i..run_end).rev().find(|&k| is_break(chars[k])) {
        return (Piece::Newlines, last_break + 1);
    }
    if run_end == chars.len() || run_end - i == 1 {
        return (Piece::Spaces, run_end);
    }
    // Leave the last space for the piece that follows (` word`).
    (Piece::Spaces, run_end - 1)
}

/// Cost of one piece in thousandths of a token.
fn cost(kind: Piece, piece: &[char]) -> u64 {
    match kind {
        Piece::Contraction | Piece::Digits => UNIT,
        Piece::Letters => letters_cost(piece),
        Piece::Punct => punct_cost(piece),
        Piece::Newlines => {
            let breaks = piece.iter().filter(|c| is_break(**c)).count() as u64;
            let spaces = piece.len() as u64 - breaks;
            // A run of line breaks is usually one token; indentation before
            // them adds a little.
            UNIT + breaks.saturating_sub(2) * 300 + spaces.div_ceil(4) * 500
        }
        Piece::Spaces => spaces_cost(piece),
    }
}

/// Space runs: one token up to long indentation; other whitespace
/// characters (tabs, non-breaking spaces) cost one each.
fn spaces_cost(piece: &[char]) -> u64 {
    let spaces = piece.iter().filter(|c| **c == ' ').count() as u64;
    let other = piece.len() as u64 - spaces;
    (spaces.div_ceil(20) + other) * UNIT
}

/// Punctuation runs: a leading space and trailing line breaks merge into
/// the run; the rest is matched greedily against punctuation that is one
/// token (`":"`, `":{"`, `"},"`, `##`, `://`). Unmatched ASCII symbols
/// cost one token, less after another unmatched one; other symbols cost
/// as non-ASCII characters.
fn punct_cost(piece: &[char]) -> u64 {
    let core: Vec<char> = piece
        .iter()
        .copied()
        .skip_while(|c| *c == ' ')
        .filter(|c| !is_break(*c))
        .collect();
    let longest = lexicon::PUNCT.iter().map(|t| t.len()).max().unwrap_or(1);
    let mut total = 0;
    let mut i = 0;
    let mut after_unmatched = false;
    let mut probe = String::new();
    while i < core.len() {
        let matched = (2..=longest.min(core.len() - i)).rev().find(|&n| {
            probe.clear();
            probe.extend(&core[i..i + n]);
            lexicon::PUNCT.binary_search(&probe.as_str()).is_ok()
        });
        if let Some(n) = matched {
            total += UNIT;
            i += n;
            after_unmatched = false;
            continue;
        }
        let c = core[i];
        total += if !c.is_ascii() {
            non_ascii_cost(c)
        } else if after_unmatched {
            600
        } else {
            UNIT
        };
        after_unmatched = c.is_ascii();
        i += 1;
    }
    total
}

/// A character outside ASCII: two-byte characters (accented letters,
/// `·`) and common punctuation, arrows and math symbols (`—`, `→`, `≤`) are
/// about one token; other three-byte characters one and a half; four-byte
/// characters (emoji) three.
fn non_ascii_cost(c: char) -> u64 {
    match c.len_utf8() {
        1 | 2 => UNIT,
        3 if matches!(c, '\u{2000}'..='\u{22FF}') => UNIT,
        3 => 1500,
        _ => 3 * UNIT,
    }
}

/// What precedes the first word of a letter run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lead {
    None,
    Space,
    /// An ASCII mark (`_`, `.`, `/`, `-`, `$`, ...).
    Mark,
}

/// A letter run, optionally with one leading non-letter character: the sum
/// of its words, split at lower→upper and acronym→word boundaries
/// (`submitAlphaMessage`, `HTTPServer`).
fn letters_cost(piece: &[char]) -> u64 {
    let (lead, letters, mut total) = match piece.first() {
        Some(&' ') => (Lead::Space, &piece[1..], 0),
        Some(&c) if !is_letter(c) && c.is_ascii() => (Lead::Mark, &piece[1..], 0),
        // Tabs and non-ASCII marks are a token of their own.
        Some(&c) if !is_letter(c) => (Lead::None, &piece[1..], non_ascii_cost(c).max(UNIT)),
        _ => (Lead::None, piece, 0),
    };
    let mut start = 0;
    let mut lead = lead;
    for k in 1..=letters.len() {
        let boundary = k == letters.len() || {
            let (prev, cur) = (letters[k - 1], letters[k]);
            let next = letters.get(k + 1).copied();
            (prev.is_lowercase() && cur.is_uppercase())
                || (prev.is_uppercase()
                    && cur.is_uppercase()
                    && next.is_some_and(char::is_lowercase))
        };
        if boundary {
            total += word_cost(&letters[start..k], lead);
            lead = Lead::None;
            start = k;
        }
    }
    total
}

/// One word: known words from the lexicon, otherwise by length and case.
/// Non-ASCII letters cost per byte pair.
fn word_cost(word: &[char], lead: Lead) -> u64 {
    let mark = if lead == Lead::Mark { 250 } else { 0 };
    if word.iter().any(|c| !c.is_ascii()) {
        return mark
            + word
                .iter()
                .map(|&c| non_ascii_cost(c))
                .sum::<u64>()
                .max(UNIT);
    }
    let len = word.len() as u64;
    if word.len() > 1 && word.iter().all(char::is_ascii_uppercase) {
        // Acronyms and constants (`HTTP`, `UUID`, `ENABLED`).
        return mark
            + match len {
                0..=5 => 1050,
                6 => 1400,
                7 | 8 => 1900,
                9..=12 => 2400,
                n => 2400 + (n - 12) * 350,
            };
    }
    if len < 5 {
        return mark + UNIT;
    }
    let lower: String = word.iter().map(char::to_ascii_lowercase).collect();
    if lexicon::SINGLE.binary_search(&lower.as_str()).is_ok() {
        return mark + UNIT;
    }
    if lexicon::SPACED.binary_search(&lower.as_str()).is_ok() {
        return mark + if lead == Lead::Space { UNIT } else { 2 * UNIT };
    }
    mark + if lead == Lead::Space {
        match len {
            5 | 6 => 1200,
            7..=10 => 1450,
            11..=13 => 1800,
            n => 1800 + (n - 13) * 350,
        }
    } else {
        match len {
            5 | 6 => 1500,
            7..=10 => 2100,
            11..=13 => 2700,
            n => 2700 + (n - 13) * 300,
        }
    }
}

/// Internals exposed to the private test harness (feature `testing`).
#[cfg(feature = "testing")]
#[doc(hidden)]
pub mod __testing {
    /// The pre-tokenizer pieces of `text` with their class name and
    /// estimated cost in thousandths of a token.
    pub fn pieces(text: &str) -> Vec<(&'static str, String, u64)> {
        let chars: Vec<char> = text.chars().collect();
        let mut out = vec![];
        let mut i = 0;
        while i < chars.len() {
            let (kind, end) = super::next_piece(&chars, i);
            let name = match kind {
                super::Piece::Contraction => "contraction",
                super::Piece::Letters => "letters",
                super::Piece::Digits => "digits",
                super::Piece::Punct => "punct",
                super::Piece::Newlines => "newlines",
                super::Piece::Spaces => "spaces",
            };
            out.push((
                name,
                chars[i..end].iter().collect(),
                super::cost(kind, &chars[i..end]),
            ));
            i = end;
        }
        out
    }

    /// The punctuation runs the estimate counts as one token.
    pub fn punct_tokens() -> &'static [&'static str] {
        super::lexicon::PUNCT
    }

    /// Whether `word` (lowercase) is in a word list: `single`, `spaced`.
    pub fn lexicon(word: &str) -> Option<&'static str> {
        if super::lexicon::SINGLE.binary_search(&word).is_ok() {
            Some("single")
        } else if super::lexicon::SPACED.binary_search(&word).is_ok() {
            Some("spaced")
        } else {
            None
        }
    }
}
