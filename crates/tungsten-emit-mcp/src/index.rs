// SPDX-License-Identifier: AGPL-3.0-only
//! The precomputed BM25 index of `search_tools` and the
//! tokenizer it shares with `@tungsten/mcp` (`tokenize` and `STOP_WORDS`
//! in `runtimes/mcp/src/types.ts`): the runtime tokenizes queries with the
//! same rule, so both sides must produce identical terms.
//!
//! Rule: split on every character that is not an ASCII letter or digit and
//! between an ASCII lowercase letter or digit and a following ASCII
//! uppercase letter; lowercase; drop words shorter than two characters and
//! the stop words. No stemming.

use std::collections::BTreeMap;

use serde::Serialize;

/// BM25 term-frequency saturation.
pub const K1: f64 = 1.2;
/// BM25 document-length normalization.
pub const B: f64 = 0.75;

/// Words never indexed or searched (`STOP_WORDS` in the runtime contract).
pub const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on",
    "or", "the", "this", "to", "with",
];

/// The terms of `text` under the shared rule.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut words: Vec<String> = vec![];
    let mut current = String::new();
    let mut prev: Option<char> = None;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            let boundary = c.is_ascii_uppercase()
                && prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit());
            if boundary {
                words.push(std::mem::take(&mut current));
            }
            current.push(c.to_ascii_lowercase());
        } else {
            words.push(std::mem::take(&mut current));
        }
        prev = Some(c);
    }
    words.push(current);
    words
        .into_iter()
        .filter(|w| w.len() >= 2 && !STOP_WORDS.contains(&w.as_str()))
        .collect()
}

/// `SearchIndex` of the runtime contract.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndex {
    pub k1: f64,
    pub b: f64,
    /// Mean document length in terms, rounded to six decimals.
    pub avg_doc_length: f64,
    /// Terms per tool, in tool order.
    pub doc_lengths: Vec<usize>,
    /// Term → `[tool index, term frequency]`, terms sorted, postings sorted
    /// by tool index.
    pub postings: BTreeMap<String, Vec<(usize, usize)>>,
}

/// The index of `docs`, one term list per tool in tool order.
pub fn build(docs: &[Vec<String>]) -> SearchIndex {
    let mut postings: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut doc_lengths = Vec::with_capacity(docs.len());
    for (i, terms) in docs.iter().enumerate() {
        doc_lengths.push(terms.len());
        let mut tf: BTreeMap<&str, usize> = BTreeMap::new();
        for term in terms {
            *tf.entry(term.as_str()).or_default() += 1;
        }
        for (term, n) in tf {
            postings.entry(term.to_string()).or_default().push((i, n));
        }
    }
    let total: usize = doc_lengths.iter().sum();
    let avg_doc_length = if docs.is_empty() {
        0.0
    } else {
        round6(total as f64 / docs.len() as f64)
    };
    SearchIndex {
        k1: K1,
        b: B,
        avg_doc_length,
        doc_lengths,
        postings,
    }
}

/// `x` rounded to six decimals, so the manifest's float is the same on
/// every platform.
fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}
