// SPDX-License-Identifier: AGPL-3.0-only
//! Strict standard-alphabet base64 decoding (RFC 4648, padded).

fn value(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decode `text`. Fails on a character outside the alphabet, on a length
/// that is not a multiple of four, and on misplaced padding.
pub(crate) fn decode(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err("length is not a multiple of 4".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let chunks = bytes.len() / 4;
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        let last = i + 1 == chunks;
        let pad = chunk.iter().rev().take_while(|&&b| b == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return Err("misplaced padding".into());
        }
        let mut acc = 0u32;
        for &b in &chunk[..4 - pad] {
            let v = value(b).ok_or_else(|| format!("invalid character {:?}", char::from(b)))?;
            acc = (acc << 6) | v;
        }
        acc <<= 6 * pad as u32;
        let triple = acc.to_be_bytes();
        out.extend_from_slice(&triple[1..4 - pad]);
    }
    Ok(out)
}
