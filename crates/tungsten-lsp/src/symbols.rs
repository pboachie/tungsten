// SPDX-License-Identifier: AGPL-3.0-only
//! Document symbols: the outline of a manifest.

use lsp_types::{DocumentSymbol, SymbolKind};

use crate::position::LineIndex;
use crate::scan::{Entry, Seg, scan};

/// Deepest level of the outline: section, entry, field.
const MAX_DEPTH: usize = 3;

/// Keys that name a sequence item, in order of preference.
const NAME_KEYS: [&str; 7] = [
    "operation",
    "name",
    "namespace",
    "call",
    "poll",
    "paginate",
    "target",
];

struct Node {
    path: Vec<Seg>,
    first: usize,
    last: usize,
    select: (usize, usize),
    name: String,
    detail: Option<String>,
    item: bool,
    children: Vec<Node>,
}

pub fn symbols(text: &str, lines: &LineIndex) -> Vec<DocumentSymbol> {
    let scan = scan(text, lines);
    let mut roots: Vec<Node> = vec![];
    let mut flat: Vec<Node> = vec![];
    for e in scan.entries.iter().filter(|e| !e.flow) {
        if e.item {
            let path = match e.key {
                Some(_) => e.path[..e.path.len() - 1].to_vec(),
                None => e.path.clone(),
            };
            let (s, end) = lines.line_range(e.line, text);
            flat.push(Node {
                path,
                first: e.line,
                last: e.line,
                select: (s + e.indent, end),
                name: String::new(),
                detail: None,
                item: true,
                children: vec![],
            });
        }
        if let (Some(key), Some(range)) = (&e.key, e.key_range) {
            flat.push(Node {
                path: e.path.clone(),
                first: e.line,
                last: e.line,
                select: range,
                name: key.clone(),
                detail: value_text(text, e),
                item: false,
                children: vec![],
            });
        }
    }
    // Every node ends where the last entry below it ends.
    let last_of = |path: &[Seg]| {
        scan.entries
            .iter()
            .filter(|e| e.path.starts_with(path))
            .map(|e| e.line)
            .max()
            .unwrap_or(0)
    };
    for n in &mut flat {
        n.last = last_of(&n.path).max(n.first);
    }
    nest(flat, &mut roots);
    name_items(&mut roots, text);
    roots.iter().map(|n| convert(n, text, lines, 1)).collect()
}

fn value_text(text: &str, e: &Entry) -> Option<String> {
    let (s, end) = e.value_range?;
    let v = text[s..end].trim_matches(['"', '\'']);
    (!v.is_empty() && !v.starts_with(['|', '>', '{', '['])).then(|| v.to_string())
}

/// Attach each node to the closest earlier node whose path is a prefix.
fn nest(flat: Vec<Node>, roots: &mut Vec<Node>) {
    let mut stack: Vec<Node> = vec![];
    let close = |stack: &mut Vec<Node>, roots: &mut Vec<Node>| {
        if let Some(done) = stack.pop() {
            match stack.last_mut() {
                Some(parent) => parent.children.push(done),
                None => roots.push(done),
            }
        }
    };
    for node in flat {
        while let Some(top) = stack.last() {
            if node.path.starts_with(&top.path) && node.path.len() > top.path.len() {
                break;
            }
            close(&mut stack, roots);
        }
        stack.push(node);
    }
    while !stack.is_empty() {
        close(&mut stack, roots);
    }
}

fn name_items(nodes: &mut [Node], text: &str) {
    for (i, node) in nodes.iter_mut().enumerate() {
        if node.item {
            let by_key = NAME_KEYS.iter().find_map(|k| {
                node.children
                    .iter()
                    .find(|c| c.name == *k)
                    .and_then(|c| c.detail.clone())
            });
            let scalar = text[node.select.0..node.select.1]
                .trim_matches(['"', '\''])
                .to_string();
            node.name = by_key
                .or_else(|| (!scalar.is_empty() && node.children.is_empty()).then_some(scalar))
                .unwrap_or_else(|| format!("item {}", i + 1));
        }
        name_items(&mut node.children, text);
    }
}

fn convert(n: &Node, text: &str, lines: &LineIndex, depth: usize) -> DocumentSymbol {
    let start = lines.line_start(n.first, text);
    let end = lines.line_range(n.last, text).1;
    let kind = if n.item {
        SymbolKind::OBJECT
    } else if n.children.iter().any(|c| c.item) {
        SymbolKind::ARRAY
    } else if n.children.is_empty() {
        SymbolKind::PROPERTY
    } else {
        SymbolKind::STRUCT
    };
    let children: Vec<DocumentSymbol> = if depth < MAX_DEPTH {
        n.children
            .iter()
            .map(|c| convert(c, text, lines, depth + 1))
            .collect()
    } else {
        vec![]
    };
    #[allow(deprecated)]
    DocumentSymbol {
        name: n.name.clone(),
        detail: n.detail.clone(),
        kind,
        tags: None,
        deprecated: None,
        range: lines.range(start, end, text),
        selection_range: lines.range(n.select.0, n.select.1.max(n.select.0), text),
        children: (!children.is_empty()).then_some(children),
    }
}
