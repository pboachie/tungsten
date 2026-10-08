// SPDX-License-Identifier: AGPL-3.0-only
//! Line-based unified diffs for `tungsten diff`.
//!
//! Lines are compared with Myers' O(ND) algorithm after the common prefix
//! and suffix are removed. The edit distance searched is bounded
//! ([`MAX_EDITS`]): beyond it the differing middle is shown as one
//! replacement, so a diff of two unrelated large files stays linear in
//! memory and time. The result depends only on the two texts.

use std::fmt::Write as _;

/// Edit distance beyond which the middle is replaced as a whole.
const MAX_EDITS: usize = 2000;

/// Unchanged lines shown around each change.
const CONTEXT: usize = 3;

/// A unified diff of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Patch {
    /// `---` / `+++` headers and hunks, at most `max_lines` hunk lines.
    pub text: String,
    /// Lines only in the new text.
    pub added: usize,
    /// Lines only in the old text.
    pub removed: usize,
    /// Hunk lines left out because of `max_lines`.
    pub truncated: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// Old line index, new line index.
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// What one side of the diff is called in the headers: `a/<path>`, or
/// `/dev/null` for a file that does not exist on that side.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Side<'a> {
    File(&'a str),
    Absent,
}

/// The unified diff from `old` to `new`, with hunk bodies cut after
/// `max_lines` lines.
pub(crate) fn unified(
    old_name: Side<'_>,
    new_name: Side<'_>,
    old: &str,
    new: &str,
    max_lines: usize,
) -> Patch {
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let ops = diff_lines(&a, &b);
    let added = ops.iter().filter(|o| matches!(o, Op::Insert(_))).count();
    let removed = ops.iter().filter(|o| matches!(o, Op::Delete(_))).count();
    let mut body = vec![];
    for hunk in hunks(&ops) {
        body.push(hunk_header(&ops[hunk.clone()]));
        for op in &ops[hunk] {
            let (mark, line) = match *op {
                Op::Equal(i, _) => (' ', a[i]),
                Op::Delete(i) => ('-', a[i]),
                Op::Insert(j) => ('+', b[j]),
            };
            body.push(format!("{mark}{}", line.strip_suffix('\n').unwrap_or(line)));
            if !line.ends_with('\n') {
                body.push("\\ No newline at end of file".to_string());
            }
        }
    }
    let mut text = String::new();
    if !body.is_empty() {
        let name = |side: Side<'_>, prefix: &str| match side {
            Side::File(path) => format!("{prefix}/{path}"),
            Side::Absent => "/dev/null".to_string(),
        };
        let _ = writeln!(text, "--- {}", name(old_name, "a"));
        let _ = writeln!(text, "+++ {}", name(new_name, "b"));
    }
    let truncated = body.len().saturating_sub(max_lines);
    for line in body.iter().take(max_lines) {
        text.push_str(line);
        text.push('\n');
    }
    Patch {
        text,
        added,
        removed,
        truncated,
    }
}

/// The edit script from `a` to `b`.
fn diff_lines(a: &[&str], b: &[&str]) -> Vec<Op> {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut ops: Vec<Op> = (0..prefix).map(|i| Op::Equal(i, i)).collect();
    let middle = myers(am, bm).unwrap_or_else(|| {
        (0..am.len())
            .map(Op::Delete)
            .chain((0..bm.len()).map(Op::Insert))
            .collect()
    });
    ops.extend(middle.into_iter().map(|op| match op {
        Op::Equal(i, j) => Op::Equal(i + prefix, j + prefix),
        Op::Delete(i) => Op::Delete(i + prefix),
        Op::Insert(j) => Op::Insert(j + prefix),
    }));
    let (old_end, new_end) = (a.len() - suffix, b.len() - suffix);
    ops.extend((0..suffix).map(|k| Op::Equal(old_end + k, new_end + k)));
    ops
}

/// Myers' shortest edit script, or `None` when it needs more than
/// [`MAX_EDITS`] edits. `trace[d]` holds the furthest x on each diagonal
/// `k` in `-d..=d` (index `k + d`) after `d` edits.
fn myers(a: &[&str], b: &[&str]) -> Option<Vec<Op>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let limit = (a.len() + b.len()).min(MAX_EDITS) as isize;
    let mut trace: Vec<Vec<isize>> = vec![];
    for d in 0..=limit {
        let mut v = vec![0isize; (2 * d + 1) as usize];
        for k in (-d..=d).step_by(2) {
            let mut x = if d == 0 {
                0
            } else {
                let prev = &trace[(d - 1) as usize];
                let at = |k: isize| prev[(k + d - 1) as usize];
                if k == -d || (k != d && at(k - 1) < at(k + 1)) {
                    at(k + 1)
                } else {
                    at(k - 1) + 1
                }
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[(k + d) as usize] = x;
            if x >= n && y >= m {
                trace.push(v);
                return Some(backtrack(&trace, n, m));
            }
        }
        trace.push(v);
    }
    None
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize) -> Vec<Op> {
    let mut ops = vec![];
    let (mut x, mut y) = (n, m);
    for d in (1..trace.len() as isize).rev() {
        let prev = &trace[(d - 1) as usize];
        let at = |k: isize| prev[(k + d - 1) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = at(prev_k);
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push(Op::Equal(x as usize, y as usize));
        }
        if x == prev_x {
            y -= 1;
            ops.push(Op::Insert(y as usize));
        } else {
            x -= 1;
            ops.push(Op::Delete(x as usize));
        }
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        ops.push(Op::Equal(x as usize, y as usize));
    }
    ops.reverse();
    ops
}

/// Ranges of `ops` forming hunks: each change with [`CONTEXT`] equal lines
/// around it, merged when they touch.
fn hunks(ops: &[Op]) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = vec![];
    for (i, _) in ops
        .iter()
        .enumerate()
        .filter(|(_, o)| !matches!(o, Op::Equal(..)))
    {
        let start = i.saturating_sub(CONTEXT);
        let end = (i + 1 + CONTEXT).min(ops.len());
        match out.last_mut() {
            Some(last) if start <= last.end => last.end = end,
            _ => out.push(start..end),
        }
    }
    out
}

/// `@@ -l,s +l,s @@` for a hunk (1-based starts, a count of 1 omitted).
/// A side without lines is `0,0`: hunks carry context, so a side is empty
/// only when that whole text is.
fn hunk_header(ops: &[Op]) -> String {
    let side = |old: bool| -> String {
        let lines: Vec<usize> = ops
            .iter()
            .filter_map(|o| match (*o, old) {
                (Op::Equal(i, _) | Op::Delete(i), true) => Some(i),
                (Op::Equal(_, j) | Op::Insert(j), false) => Some(j),
                _ => None,
            })
            .collect();
        match lines.as_slice() {
            [] => "0,0".to_string(),
            [first] => format!("{}", first + 1),
            [first, ..] => format!("{},{}", first + 1, lines.len()),
        }
    };
    format!("@@ -{} +{} @@", side(true), side(false))
}
