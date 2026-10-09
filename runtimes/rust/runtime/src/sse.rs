// SPDX-License-Identifier: Apache-2.0
//! A server-sent events parser (WHATWG HTML 9.2 "Server-sent events"):
//! lines end with CRLF, LF or CR; a line is a comment (`:`), a field
//! (`event`, `data`, `id`, `retry`; the value loses one leading space) or the
//! blank line that dispatches the event. Several `data` lines are joined with
//! LF; an event without `data` lines is not dispatched; an event still open
//! when the stream ends is discarded. Same behaviour as `runtimes/ts/src/sse.ts`.
//!
//! An event may hold at most `max_event_bytes` (default
//! [`DEFAULT_MAX_EVENT_BYTES`], 1 MiB): the UTF-8 bytes of its field lines
//! (each counted with a one-byte terminator; a finished comment line is not
//! kept and does not count) plus the line being read, whatever it turns out to
//! be, so a comment longer than the limit with no line end also stops the
//! parser. A parser that sees more stops:
//! [`SseParser::exceeded`] becomes true, the events completed before the
//! oversize one are still returned, and later input is ignored, so nothing is
//! buffered without bound.

/// The default size limit of one event, in UTF-8 bytes.
pub const DEFAULT_MAX_EVENT_BYTES: usize = 1024 * 1024;

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event` field, `message` when the event has none.
    pub event: String,
    pub data: String,
    /// The last event id seen so far (it survives across events).
    pub id: Option<String>,
    /// The last valid `retry` value seen so far, in milliseconds.
    pub retry: Option<u64>,
}

#[derive(Debug)]
pub struct SseParser {
    buffer: String,
    started: bool,
    event: String,
    data: String,
    has_data: bool,
    id: Option<String>,
    retry: Option<u64>,
    event_bytes: usize,
    exceeded: bool,
    max: usize,
}

impl Default for SseParser {
    fn default() -> Self {
        Self::with_max_event_bytes(DEFAULT_MAX_EVENT_BYTES)
    }
}

impl SseParser {
    /// A parser with the default size limit.
    pub fn new() -> Self {
        Self::default()
    }

    /// A parser that stops at events larger than `max_event_bytes`.
    pub fn with_max_event_bytes(max_event_bytes: usize) -> Self {
        SseParser {
            buffer: String::new(),
            started: false,
            event: String::new(),
            data: String::new(),
            has_data: false,
            id: None,
            retry: None,
            event_bytes: 0,
            exceeded: false,
            max: max_event_bytes,
        }
    }

    /// The event being read is larger than the limit: the parser has stopped.
    pub fn exceeded(&self) -> bool {
        self.exceeded
    }

    /// Feed decoded text; returns the events it completes. A CR at the end of
    /// the text is held back until the next text shows whether an LF follows.
    pub fn push(&mut self, chunk: &str) -> Vec<SseEvent> {
        if self.exceeded {
            return Vec::new();
        }
        let mut text = std::mem::take(&mut self.buffer);
        text.push_str(chunk);
        if !self.started && !text.is_empty() {
            self.started = true;
            if let Some(rest) = text.strip_prefix('\u{feff}') {
                text = rest.to_owned();
            }
        }
        let mut out = Vec::new();
        let bytes = text.as_bytes();
        let mut start = 0;
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c != b'\n' && c != b'\r' {
                i += 1;
                continue;
            }
            if c == b'\r' && i + 1 == bytes.len() {
                self.buffer = text[start..].to_owned();
                return out;
            }
            let line = &text[start..i];
            if c == b'\r' && bytes[i + 1] == b'\n' {
                i += 1;
            }
            start = i + 1;
            i += 1;
            self.line(line, &mut out);
            if self.exceeded {
                return out;
            }
        }
        self.buffer = text[start..].to_owned();
        if self.event_bytes + self.buffer.len() > self.max {
            self.exceeded = true;
            self.buffer.clear();
        }
        out
    }

    /// The stream ended: a CR held back ends its line. Whatever is still open
    /// is discarded.
    pub fn end(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.exceeded
            && let Some(line) = self.buffer.strip_suffix('\r')
        {
            let line = line.to_owned();
            self.line(&line, &mut out);
        }
        self.buffer.clear();
        self.event.clear();
        self.data.clear();
        self.has_data = false;
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseEvent>) {
        if line.is_empty() {
            if self.has_data {
                let mut data = std::mem::take(&mut self.data);
                data.pop();
                out.push(SseEvent {
                    event: if self.event.is_empty() {
                        "message".to_owned()
                    } else {
                        self.event.clone()
                    },
                    data,
                    id: self.id.clone(),
                    retry: self.retry,
                });
            }
            self.event.clear();
            self.data.clear();
            self.has_data = false;
            self.event_bytes = 0;
            return;
        }
        if line.starts_with(':') {
            return;
        }
        self.event_bytes += line.len() + 1;
        if self.event_bytes > self.max {
            self.exceeded = true;
            return;
        }
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match name {
            "event" => value.clone_into(&mut self.event),
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
                self.has_data = true;
            }
            "id" => {
                if !value.contains('\0') {
                    self.id = Some(value.to_owned());
                }
            }
            "retry" => {
                if !value.is_empty()
                    && value.bytes().all(|b| b.is_ascii_digit())
                    && let Ok(ms) = value.parse::<u64>()
                    && ms <= 9_007_199_254_740_991
                {
                    self.retry = Some(ms);
                }
            }
            _ => {}
        }
    }
}

/// An incremental UTF-8 decoder: a sequence split across chunks is joined,
/// invalid bytes become U+FFFD (as `TextDecoder` and Python's `replace`).
#[derive(Debug, Default)]
pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn decode(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let data = std::mem::take(&mut self.pending);
        let mut out = String::new();
        let mut rest: &[u8] = &data;
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    out.push_str(text);
                    return out;
                }
                Err(error) => {
                    let (valid, tail) = rest.split_at(error.valid_up_to());
                    out.push_str(&String::from_utf8_lossy(valid));
                    match error.error_len() {
                        Some(n) => {
                            out.push('\u{fffd}');
                            rest = &tail[n..];
                        }
                        None => {
                            self.pending = tail.to_vec();
                            return out;
                        }
                    }
                }
            }
        }
    }

    /// The stream ended: an incomplete sequence becomes one U+FFFD.
    pub fn finish(&mut self) -> String {
        if std::mem::take(&mut self.pending).is_empty() {
            String::new()
        } else {
            "\u{fffd}".to_owned()
        }
    }
}
