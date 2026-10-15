// SPDX-License-Identifier: AGPL-3.0-only
//! Conservative lexical budgets before building a recursive Rust AST.

pub(super) fn bounded(text: &str, max_depth: usize, max_nodes: usize) -> bool {
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut groups = vec![(0u8, 0usize)];
    let mut nodes = 0usize;
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
                    return false;
                }
            }
            if depth != 0 {
                return false;
            }
            continue;
        }
        nodes += 1;
        if nodes > max_nodes {
            return false;
        }
        if let Some((_, tokens)) = groups.last_mut() {
            *tokens += 1;
            if *tokens > 64 {
                return false;
            }
        }
        // Raw string bodies are opaque, including delimiter-looking contents.
        if bytes[at] == b'r' {
            let mut quote = at + 1;
            while bytes.get(quote) == Some(&b'#') {
                quote += 1;
            }
            if quote - at > 256 {
                return false;
            }
            if bytes.get(quote) == Some(&b'"') {
                let end = format!("\"{}", "#".repeat(quote - at - 1));
                let Some(offset) = text[quote + 1..].find(&end) else {
                    return false;
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
                    None => return false,
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
                    return false;
                }
                groups.push((bytes[at], 0));
            }
            b')' | b']' | b'}' => {
                let expected = match bytes[at] {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if groups.pop().map(|group| group.0) != Some(expected) {
                    return false;
                }
                if groups.len() == 1 {
                    groups[0].1 = 0;
                }
            }
            b';' | b',' => {
                if let Some((_, tokens)) = groups.last_mut() {
                    *tokens = 0;
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
    groups.len() == 1
}
