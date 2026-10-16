// SPDX-License-Identifier: AGPL-3.0-only
//! Conservative lexical budgets before building a recursive Rust AST.

/// Unbroken tokens allowed in one delimiter group between `;`/`,` separators.
/// Postfix `.name` links are budgeted separately so long builder chains such
/// as `Router::new().route(..).route(..)` do not exhaust it.
const MAX_GROUP_TOKENS: usize = 64;
/// Postfix `.name` links open at once across all enclosing groups. Each link
/// adds one level to syn's left-nested AST, so this bounds the depth reached
/// by recursive AST visits and drops on the parser's dedicated stack.
pub(super) const MAX_CHAIN_LINKS: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Exceeded {
    Expression,
    Chain,
}

pub(super) fn bounded(text: &str, max_depth: usize, max_nodes: usize) -> Result<(), Exceeded> {
    use Exceeded::Expression;
    let bytes = text.as_bytes();
    let mut at = 0;
    // (opening delimiter, unbroken tokens, open chain links)
    let mut groups = vec![(0u8, 0usize, 0usize)];
    let mut nodes = 0usize;
    let mut links = 0usize;
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if bytes[at..].starts_with(b"//") {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        if bytes[at..].starts_with(b"/*") {
            let mut depth = 1;
            at += 2;
            while at < bytes.len() && depth > 0 {
                if bytes[at..].starts_with(b"/*") {
                    depth += 1;
                    at += 2;
                } else if bytes[at..].starts_with(b"*/") {
                    depth -= 1;
                    at += 2;
                } else {
                    at += 1;
                }
                if depth > max_depth {
                    return Err(Expression);
                }
            }
            if depth != 0 {
                return Err(Expression);
            }
            continue;
        }
        nodes += 1;
        if nodes > max_nodes {
            return Err(Expression);
        }
        // A postfix `.name` link (method call, field or `.await`) nests the AST
        // by one level like any other token, but is counted against the chain
        // budget; its `.name` and an immediately following call parenthesis do
        // not consume the flat expression budget. `..` ranges and tuple
        // indices remain ordinary tokens.
        if bytes[at] == b'.'
            && bytes
                .get(at + 1)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
            && (at == 0 || bytes[at - 1] != b'.')
        {
            links += 1;
            if links > MAX_CHAIN_LINKS {
                return Err(Exceeded::Chain);
            }
            if let Some((_, _, open)) = groups.last_mut() {
                *open += 1;
            }
            at += 1;
            while bytes
                .get(at)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                at += 1;
            }
            while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
                at += 1;
            }
            if bytes.get(at) == Some(&b'(') {
                if groups.len() > max_depth {
                    return Err(Expression);
                }
                groups.push((b'(', 0, 0));
                at += 1;
            }
            continue;
        }
        if let Some((_, tokens, _)) = groups.last_mut() {
            *tokens += 1;
            if *tokens > MAX_GROUP_TOKENS {
                return Err(Expression);
            }
        }
        // Raw string bodies are opaque, including delimiter-looking contents.
        if bytes[at] == b'r' {
            let mut quote = at + 1;
            while bytes.get(quote) == Some(&b'#') {
                quote += 1;
            }
            if quote - at > 256 {
                return Err(Expression);
            }
            if bytes.get(quote) == Some(&b'"') {
                let end = format!("\"{}", "#".repeat(quote - at - 1));
                let Some(offset) = text[quote + 1..].find(&end) else {
                    return Err(Expression);
                };
                at = quote + 1 + offset + end.len();
                continue;
            }
        }
        if bytes[at] == b'"' {
            at += 1;
            loop {
                match bytes.get(at) {
                    Some(b'\\') => at += 2,
                    Some(b'"') => {
                        at += 1;
                        break;
                    }
                    Some(_) => at += 1,
                    None => return Err(Expression),
                }
            }
            continue;
        }
        // Distinguish character literals from lifetimes without consuming code.
        if bytes[at] == b'\'' && at + 1 < bytes.len() {
            let closing = if bytes[at + 1] == b'\\' {
                let end = match bytes.get(at + 2) {
                    Some(b'x') => Some(at + 5),
                    Some(b'u') if bytes.get(at + 3) == Some(&b'{') => text[at + 4..]
                        .find('}')
                        .filter(|offset| *offset < 8)
                        .map(|offset| at + 5 + offset),
                    Some(_) => Some(at + 3),
                    None => None,
                };
                end.filter(|end| bytes.get(*end) == Some(&b'\''))
            } else {
                text[at + 1..]
                    .chars()
                    .next()
                    .map(|ch| at + 1 + ch.len_utf8())
                    .filter(|end| bytes.get(*end) == Some(&b'\''))
            };
            if let Some(end) = closing {
                at = end + 1;
                continue;
            }
        }
        match bytes[at] {
            b'(' | b'[' | b'{' => {
                if groups.len() > max_depth {
                    return Err(Expression);
                }
                groups.push((bytes[at], 0, 0));
            }
            b')' | b']' | b'}' => {
                let expected = match bytes[at] {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                let Some((open, _, closed)) = groups.pop() else {
                    return Err(Expression);
                };
                if open != expected || groups.is_empty() {
                    return Err(Expression);
                }
                links -= closed;
                if groups.len() == 1 {
                    links -= groups[0].2;
                    groups[0].1 = 0;
                    groups[0].2 = 0;
                }
            }
            b';' | b',' => {
                if let Some((_, tokens, open)) = groups.last_mut() {
                    *tokens = 0;
                    links -= *open;
                    *open = 0;
                }
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => {
                while bytes
                    .get(at + 1)
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                {
                    at += 1;
                }
            }
            _ => {}
        }
        at += 1;
    }
    if groups.len() == 1 {
        Ok(())
    } else {
        Err(Expression)
    }
}
