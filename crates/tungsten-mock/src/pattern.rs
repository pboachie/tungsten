// SPDX-License-Identifier: AGPL-3.0-only
//! Sampling strings from simple regular expressions: literals, classes,
//! the common escapes, groups, alternation (first branch) and quantifiers.
//! Look-around is skipped; backreferences, Unicode properties and inline
//! flags make the pattern unsupported. Callers verify samples with the
//! regex engine.

/// Group nesting accepted.
const MAX_GROUP_DEPTH: usize = 32;
/// Longest sample produced.
const MAX_SAMPLE_CHARS: usize = 4096;
/// Characters a negated class or `.` picks from.
const SAFE_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-";

/// A string matching `pattern`, or `None` when the pattern uses syntax the
/// sampler does not support. Each quantified item repeats its minimum (at
/// least once) plus `extra` times, within its maximum.
pub(crate) fn sample(pattern: &str, seed: u64, extra: u32) -> Option<String> {
    let mut parser = Parser {
        chars: pattern.chars().collect(),
        pos: 0,
        depth: 0,
    };
    let alternatives = parser.alternatives()?;
    if parser.pos != parser.chars.len() {
        return None;
    }
    let mut out = String::new();
    let mut rng = Rng(seed);
    emit_alternatives(&alternatives, extra, &mut rng, &mut out)?;
    Some(out)
}

type Sequence = Vec<Item>;

#[derive(Debug)]
struct Item {
    atom: Atom,
    min: u32,
    /// `u32::MAX` for unbounded.
    max: u32,
}

#[derive(Debug)]
enum Atom {
    Char(char),
    Set {
        ranges: Vec<(char, char)>,
        negated: bool,
    },
    Group(Vec<Sequence>),
    /// Anchors: match without producing text.
    Empty,
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn eat(&mut self, text: &str) -> bool {
        let wanted: Vec<char> = text.chars().collect();
        if self.chars[self.pos..].starts_with(&wanted) {
            self.pos += wanted.len();
            true
        } else {
            false
        }
    }

    fn alternatives(&mut self) -> Option<Vec<Sequence>> {
        let mut out = vec![self.sequence()?];
        while self.peek() == Some('|') {
            self.pos += 1;
            out.push(self.sequence()?);
        }
        Some(out)
    }

    fn sequence(&mut self) -> Option<Sequence> {
        let mut items = vec![];
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            let (min, max) = self.quantifier()?;
            items.push(Item { atom, min, max });
        }
        Some(items)
    }

    fn atom(&mut self) -> Option<Atom> {
        match self.next()? {
            '^' | '$' => Some(Atom::Empty),
            '(' => {
                // Look-around constrains without producing text: parse it
                // to skip it, and let the caller's regex check decide.
                let lookaround =
                    self.eat("?=") || self.eat("?!") || self.eat("?<=") || self.eat("?<!");
                if !lookaround && !self.eat("?:") && self.peek() == Some('?') {
                    // Named groups only; inline flags are unsupported.
                    if !self.eat("?<") {
                        return None;
                    }
                    while self.next()? != '>' {}
                }
                self.depth += 1;
                if self.depth > MAX_GROUP_DEPTH {
                    return None;
                }
                let inner = self.alternatives()?;
                self.depth -= 1;
                if self.next()? != ')' {
                    return None;
                }
                Some(if lookaround {
                    Atom::Empty
                } else {
                    Atom::Group(inner)
                })
            }
            '[' => self.class(),
            '.' => Some(Atom::Set {
                ranges: vec![('a', 'z')],
                negated: false,
            }),
            '\\' => self.escape(),
            '*' | '+' | '?' | '{' | ')' => None,
            c => Some(Atom::Char(c)),
        }
    }

    fn escape(&mut self) -> Option<Atom> {
        let set = |ranges: Vec<(char, char)>| {
            Some(Atom::Set {
                ranges,
                negated: false,
            })
        };
        match self.next()? {
            'd' => set(vec![('0', '9')]),
            'w' => set(word_ranges()),
            's' => Some(Atom::Char(' ')),
            'D' | 'S' => Some(Atom::Char('a')),
            'W' => Some(Atom::Char('-')),
            'b' | 'B' | 'p' | 'P' | 'k' | 'c' | '1'..='9' => None,
            c => self.escaped_char(c).map(Atom::Char),
        }
    }

    /// The character an escape like `\n`, `\x41`, `A` or `\.` denotes.
    fn escaped_char(&mut self, c: char) -> Option<char> {
        match c {
            'n' => Some('\n'),
            't' => Some('\t'),
            'r' => Some('\r'),
            'f' => Some('\u{c}'),
            'v' => Some('\u{b}'),
            '0' => Some('\0'),
            'x' => self.hex_char(2),
            'u' => {
                if self.peek() == Some('{') {
                    self.pos += 1;
                    let start = self.pos;
                    while self.next()? != '}' {}
                    let digits: String = self.chars[start..self.pos - 1].iter().collect();
                    u32::from_str_radix(&digits, 16)
                        .ok()
                        .and_then(char::from_u32)
                } else {
                    self.hex_char(4)
                }
            }
            c if c.is_ascii_alphanumeric() => None,
            c => Some(c),
        }
    }

    fn hex_char(&mut self, n: usize) -> Option<char> {
        let digits: String = self.chars.get(self.pos..self.pos + n)?.iter().collect();
        self.pos += n;
        u32::from_str_radix(&digits, 16)
            .ok()
            .and_then(char::from_u32)
    }

    fn class(&mut self) -> Option<Atom> {
        let negated = self.eat("^");
        let mut ranges = vec![];
        let mut first = true;
        loop {
            let c = self.next()?;
            if c == ']' && !first {
                break;
            }
            first = false;
            let low = if c == '\\' {
                match self.next()? {
                    'd' => {
                        ranges.push(('0', '9'));
                        continue;
                    }
                    'w' => {
                        ranges.extend(word_ranges());
                        continue;
                    }
                    's' => {
                        ranges.push((' ', ' '));
                        continue;
                    }
                    'D' | 'W' | 'S' | 'p' | 'P' => return None,
                    'b' => '\u{8}',
                    other => self.escaped_char(other)?,
                }
            } else {
                c
            };
            if self.peek() == Some('-') && self.chars.get(self.pos + 1).is_some_and(|&n| n != ']') {
                self.pos += 1;
                let high = match self.next()? {
                    '\\' => {
                        let e = self.next()?;
                        self.escaped_char(e)?
                    }
                    h => h,
                };
                if high < low {
                    return None;
                }
                ranges.push((low, high));
            } else {
                ranges.push((low, low));
            }
        }
        Some(Atom::Set { ranges, negated })
    }

    /// `*`, `+`, `?`, `{n}`, `{n,}`, `{n,m}` (a lazy `?` suffix is ignored);
    /// `(1, 1)` when there is none. A `{` that starts no valid quantifier
    /// is unsupported.
    fn quantifier(&mut self) -> Option<(u32, u32)> {
        let bounds = match self.peek() {
            Some('*') => (0, u32::MAX),
            Some('+') => (1, u32::MAX),
            Some('?') => (0, 1),
            Some('{') => {
                let close = self.chars[self.pos..].iter().position(|&c| c == '}')? + self.pos;
                let body: String = self.chars[self.pos + 1..close].iter().collect();
                let (min, max) = match body.split_once(',') {
                    None => {
                        let n = body.parse().ok()?;
                        (n, n)
                    }
                    Some((low, "")) => (low.parse().ok()?, u32::MAX),
                    Some((low, high)) => (low.parse().ok()?, high.parse().ok()?),
                };
                if max < min {
                    return None;
                }
                self.pos = close;
                (min, max)
            }
            _ => return Some((1, 1)),
        };
        self.pos += 1;
        if self.peek() == Some('?') {
            self.pos += 1;
        }
        Some(bounds)
    }
}

fn word_ranges() -> Vec<(char, char)> {
    vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')]
}

fn emit_alternatives(
    alternatives: &[Sequence],
    extra: u32,
    rng: &mut Rng,
    out: &mut String,
) -> Option<()> {
    for item in alternatives.first()? {
        let count = if item.max == 0 {
            0
        } else {
            item.min.max(1).saturating_add(extra).min(item.max)
        };
        for _ in 0..count {
            match &item.atom {
                Atom::Char(c) => out.push(*c),
                Atom::Set { ranges, negated } => out.push(pick(ranges, *negated, rng)?),
                Atom::Group(inner) => emit_alternatives(inner, extra, rng, out)?,
                Atom::Empty => {}
            }
            if out.chars().count() > MAX_SAMPLE_CHARS {
                return None;
            }
        }
    }
    Some(())
}

fn pick(ranges: &[(char, char)], negated: bool, rng: &mut Rng) -> Option<char> {
    let inside = |c: char| ranges.iter().any(|&(low, high)| low <= c && c <= high);
    if negated {
        let allowed: Vec<char> = SAFE_CHARS.chars().filter(|&c| !inside(c)).collect();
        if allowed.is_empty() {
            return None;
        }
        return Some(allowed[(rng.next() % allowed.len() as u64) as usize]);
    }
    // Prefer printable ASCII members so samples stay readable.
    let ascii: Vec<char> = SAFE_CHARS
        .chars()
        .chain(" .+:@/".chars())
        .filter(|&c| inside(c))
        .collect();
    if !ascii.is_empty() {
        return Some(ascii[(rng.next() % ascii.len() as u64) as usize]);
    }
    let &(low, high) = ranges.get((rng.next() % ranges.len().max(1) as u64) as usize)?;
    let span = high as u64 - low as u64 + 1;
    char::from_u32(low as u32 + (rng.next() % span) as u32).or(Some(low))
}

/// splitmix64: a deterministic stream from a seed.
pub(crate) struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}
