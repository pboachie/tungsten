// SPDX-License-Identifier: AGPL-3.0-only
//! Request routing: method, path template and rpc discriminator.

use serde_json::Value;
use tungsten_ir::{PathSegment, PathTemplate};

use crate::model::{Model, method_str};
use crate::params::percent_decode;

/// Captured path parameters, in segment order.
pub(crate) type Captures = Vec<(String, String)>;

/// A path template prepared for matching.
#[derive(Debug)]
pub(crate) struct PathPattern {
    segments: Vec<Seg>,
}

#[derive(Debug)]
enum Seg {
    Literal(String),
    Param(String),
    Template(Vec<Part>),
}

#[derive(Debug)]
enum Part {
    Literal(String),
    Param(String),
}

impl PathPattern {
    pub fn new(path: &PathTemplate) -> PathPattern {
        let segments = path
            .segments
            .iter()
            .map(|s| match s {
                PathSegment::Literal { value } => Seg::Literal(value.clone()),
                PathSegment::Param { name } => Seg::Param(name.clone()),
                PathSegment::Template { parts } => Seg::Template(
                    parts
                        .iter()
                        .filter_map(|p| match p {
                            PathSegment::Literal { value } => Some(Part::Literal(value.clone())),
                            PathSegment::Param { name } => Some(Part::Param(name.clone())),
                            PathSegment::Template { .. } => None,
                        })
                        .collect(),
                ),
            })
            .collect();
        PathPattern { segments }
    }

    /// Match decoded request segments. Returns the specificity score (one
    /// entry per segment, higher is more literal) and the captured
    /// parameters.
    fn matches(&self, request: &[String]) -> Option<(Vec<u8>, Captures)> {
        if request.len() != self.segments.len() {
            return None;
        }
        let mut score = Vec::with_capacity(request.len());
        let mut captures = vec![];
        for (seg, value) in self.segments.iter().zip(request) {
            match seg {
                Seg::Literal(lit) => {
                    if lit != value {
                        return None;
                    }
                    score.push(3);
                }
                Seg::Param(name) => {
                    if value.is_empty() {
                        return None;
                    }
                    captures.push((name.clone(), value.clone()));
                    score.push(1);
                }
                Seg::Template(parts) => {
                    if !match_parts(parts, value, &mut captures) {
                        return None;
                    }
                    score.push(2);
                }
            }
        }
        Some((score, captures))
    }
}

/// Match a mixed segment; each placeholder takes at least one character,
/// the shortest that lets the rest match.
fn match_parts(parts: &[Part], text: &str, captures: &mut Captures) -> bool {
    let Some((first, rest)) = parts.split_first() else {
        return text.is_empty();
    };
    match first {
        Part::Literal(lit) => text
            .strip_prefix(lit.as_str())
            .is_some_and(|tail| match_parts(rest, tail, captures)),
        Part::Param(name) => {
            let ends = text
                .char_indices()
                .map(|(i, _)| i)
                .skip(1)
                .chain([text.len()]);
            for end in ends.filter(|&end| end > 0) {
                let mark = captures.len();
                captures.push((name.clone(), text[..end].to_string()));
                if match_parts(rest, &text[end..], captures) {
                    return true;
                }
                captures.truncate(mark);
            }
            false
        }
    }
}

/// Split a request path into percent-decoded segments.
pub(crate) fn split_path(path: &str) -> Vec<String> {
    match path.strip_prefix('/') {
        None | Some("") => vec![],
        Some(rest) => rest.split('/').map(|s| percent_decode(s, false)).collect(),
    }
}

#[derive(Debug)]
pub(crate) enum Routed {
    Op {
        index: usize,
        params: Captures,
    },
    /// The path and method belong to rpc operations, but the body names no
    /// method they serve (or could not be read).
    RpcUnknown {
        ns: usize,
    },
    /// The path is served under other methods only.
    NotAllowed(Vec<&'static str>),
    NotFound,
}

/// Route a request. `body` is `None` when it has not been read; rpc
/// operations then cannot be told apart.
pub(crate) fn route(model: &Model, method: &str, path: &str, body: Option<&[u8]>) -> Routed {
    let segments = split_path(path);
    let mut best: Option<Vec<u8>> = None;
    let mut group: Vec<(usize, Captures)> = vec![];
    let mut other_methods: Vec<&'static str> = vec![];
    for (index, entry) in model.ops.iter().enumerate() {
        let Some((score, params)) = entry.path.matches(&segments) else {
            continue;
        };
        let op_method = method_str(entry.op.method);
        if op_method != method {
            other_methods.push(op_method);
            continue;
        }
        match best.as_ref().map(|b| score.cmp(b)) {
            Some(std::cmp::Ordering::Less) => {}
            Some(std::cmp::Ordering::Equal) => group.push((index, params)),
            _ => {
                best = Some(score);
                group = vec![(index, params)];
            }
        }
    }
    if group.is_empty() {
        if other_methods.is_empty() {
            return Routed::NotFound;
        }
        other_methods.sort_unstable();
        other_methods.dedup();
        return Routed::NotAllowed(other_methods);
    }
    let has_rpc = group.iter().any(|(i, _)| model.ops[*i].op.rpc.is_some());
    if has_rpc {
        let parsed: Option<Value> = body.and_then(|b| serde_json::from_slice(b).ok());
        if let Some(object) = parsed.as_ref().and_then(Value::as_object) {
            for (index, params) in &group {
                if let Some(rpc) = &model.ops[*index].op.rpc
                    && object.get(&rpc.discriminator_field).and_then(Value::as_str)
                        == Some(rpc.discriminator_value.as_str())
                {
                    return Routed::Op {
                        index: *index,
                        params: params.clone(),
                    };
                }
            }
        }
    }
    let mut plain = group.iter().filter(|(i, _)| model.ops[*i].op.rpc.is_none());
    if let Some((index, params)) = plain.next() {
        return Routed::Op {
            index: *index,
            params: params.clone(),
        };
    }
    Routed::RpcUnknown {
        ns: model.ops[group[0].0].ns,
    }
}
