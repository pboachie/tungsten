// SPDX-License-Identifier: Apache-2.0
//! One HTTP attempt: a per-attempt deadline covering connect, send and the
//! body, redirects never followed by the HTTP client, and network failures
//! classified by whether the request can have reached the server.

use std::collections::BTreeMap;
use std::error::Error as _;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use reqwest::header::{HeaderName, HeaderValue};
use tokio::time::{Instant, timeout_at};

use crate::serialize::{MultipartPart, PartContent, Payload, header_value_bytes};
use crate::types::HttpMethod;

pub struct AttemptRequest<'a> {
    pub url: &'a str,
    pub method: HttpMethod,
    pub headers: &'a [(String, String)],
    pub body: &'a Payload,
    pub timeout: Duration,
}

/// Why a response body could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFailure {
    Timeout,
    Broken,
}

#[derive(Debug)]
pub enum AttemptOutcome {
    Response {
        status: u16,
        /// Lower-cased names; the last value of a repeated header wins.
        headers: BTreeMap<String, String>,
        /// The body, or `None` when reading it failed.
        body: Option<Vec<u8>>,
        body_failure: Option<BodyFailure>,
    },
    /// The request cannot have reached the server (DNS, refused, TLS).
    NotSent(String),
    /// Sent or possibly sent, then the connection failed without a response.
    Lost(String),
    Timeout,
}

fn io_code(kind: std::io::ErrorKind) -> Option<&'static str> {
    use std::io::ErrorKind as K;
    Some(match kind {
        K::ConnectionRefused => "ECONNREFUSED",
        K::ConnectionReset => "ECONNRESET",
        K::ConnectionAborted => "ECONNABORTED",
        K::NotConnected => "ENOTCONN",
        K::BrokenPipe => "EPIPE",
        K::TimedOut => "ETIMEDOUT",
        K::AddrNotAvailable => "EADDRNOTAVAIL",
        K::UnexpectedEof => "EOF",
        _ => return None,
    })
}

/// Short secret-free codes describing a transport error and its causes.
fn error_codes(error: &reqwest::Error) -> Vec<&'static str> {
    let mut codes: Vec<&'static str> = Vec::new();
    let mut push = |code: &'static str| {
        if !codes.contains(&code) {
            codes.push(code);
        }
    };
    let mut current: Option<&(dyn std::error::Error + 'static)> = error.source();
    let mut depth = 0;
    while let Some(cause) = current {
        if depth > 8 {
            break;
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>()
            && let Some(code) = io_code(io.kind())
        {
            push(code);
        }
        let text = cause.to_string().to_lowercase();
        if text.contains("dns error") || text.contains("failed to lookup") {
            push("ENOTFOUND");
        }
        if text.contains("certificate") {
            push("CERT_VERIFY_FAILED");
        } else if text.contains("tls") || text.contains("handshake") {
            push("TLS_ERROR");
        }
        if text.contains("connection closed") || text.contains("incomplete message") {
            push("CONNECTION_CLOSED");
        }
        current = cause.source();
        depth += 1;
    }
    codes
}

fn detail(error: &reqwest::Error) -> String {
    let codes = error_codes(error);
    if codes.is_empty() {
        if error.is_connect() {
            "connect error".to_owned()
        } else {
            "network error".to_owned()
        }
    } else {
        codes.join(", ")
    }
}

fn failure(error: &reqwest::Error) -> AttemptOutcome {
    if error.is_timeout() {
        AttemptOutcome::Timeout
    } else if error.is_builder() || error.is_connect() {
        AttemptOutcome::NotSent(detail(error))
    } else {
        AttemptOutcome::Lost(detail(error))
    }
}

fn multipart_form(parts: &[MultipartPart]) -> reqwest::multipart::Form {
    let mut form = reqwest::multipart::Form::new();
    for part in parts {
        let built = match &part.content {
            PartContent::Text(text) => reqwest::multipart::Part::text(text.clone()),
            PartContent::Json(text) => reqwest::multipart::Part::text(text.clone())
                .mime_str("application/json")
                .unwrap_or_else(|_| reqwest::multipart::Part::text(text.clone())),
            PartContent::File {
                data,
                filename,
                content_type,
            } => reqwest::multipart::Part::bytes(data.clone())
                .file_name(filename.clone())
                .mime_str(content_type)
                .unwrap_or_else(|_| {
                    reqwest::multipart::Part::bytes(data.clone()).file_name(filename.clone())
                }),
        };
        form = form.part(part.name.clone(), built);
    }
    form
}

/// Send once. Never fails: every outcome is an [`AttemptOutcome`].
pub async fn attempt(client: &reqwest::Client, req: &AttemptRequest<'_>) -> AttemptOutcome {
    let now = Instant::now();
    let deadline = now
        .checked_add(req.timeout)
        .unwrap_or_else(|| now + Duration::from_secs(86_400 * 365));
    let Ok(method) = reqwest::Method::from_bytes(req.method.as_str().as_bytes()) else {
        return AttemptOutcome::NotSent("INVALID_METHOD".to_owned());
    };
    let mut builder = client.request(method, req.url);
    for (name, value) in req.headers {
        let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_bytes(&header_value_bytes(value)),
        ) else {
            return AttemptOutcome::NotSent("INVALID_HEADER".to_owned());
        };
        builder = builder.header(name, value);
    }
    builder = match req.body {
        // A method that defines a body says so even when it is empty
        // (RFC 9110 8.6), as fetch and httpx do.
        Payload::Empty
            if matches!(
                req.method,
                HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch
            ) =>
        {
            builder.header(reqwest::header::CONTENT_LENGTH, "0")
        }
        Payload::Empty => builder,
        Payload::Bytes(bytes) => builder.body(bytes.clone()),
        Payload::Multipart(parts) => builder.multipart(multipart_form(parts)),
    };
    let response = match timeout_at(deadline, builder.send()).await {
        Err(_) => return AttemptOutcome::Timeout,
        Ok(Err(error)) => return failure(&error),
        Ok(Ok(response)) => response,
    };
    let status = response.status().as_u16();
    let mut headers = BTreeMap::new();
    for (name, value) in response.headers() {
        headers.insert(
            name.as_str().to_lowercase(),
            String::from_utf8_lossy(value.as_bytes()).into_owned(),
        );
    }
    let (body, body_failure) = match timeout_at(deadline, response.bytes()).await {
        Err(_) => (None, Some(BodyFailure::Timeout)),
        Ok(Err(_)) => (None, Some(BodyFailure::Broken)),
        Ok(Ok(bytes)) => (Some(bytes.to_vec()), None),
    };
    AttemptOutcome::Response {
        status,
        headers,
        body,
        body_failure,
    }
}

// -------------------------------------------------------------- retry-after

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn month_number(token: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = token.to_lowercase();
    let prefix = lower.get(..3)?;
    let index = MONTHS.iter().position(|m| *m == prefix)?;
    let full = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    (lower.len() == 3 || lower == full[index] || lower == "sept").then_some(index as i64 + 1)
}

fn parse_clock(token: &str) -> Option<i64> {
    let mut parts = token.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: f64 = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() || !(0..24).contains(&hour) || !(0..60).contains(&minute) {
        return None;
    }
    if !(0.0..61.0).contains(&second) {
        return None;
    }
    Some((hour * 3600 + minute * 60) * 1000 + (second * 1000.0).round() as i64)
}

fn parse_zone(token: &str) -> Option<i64> {
    match token.to_uppercase().as_str() {
        "GMT" | "UTC" | "UT" | "Z" => return Some(0),
        _ => {}
    }
    let sign = match token.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let digits: String = token[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 60 + minutes) * 60_000)
}

fn parse_iso_date(text: &str) -> Option<i64> {
    let (date, rest) = match text.split_once(['T', ' ']) {
        Some((d, r)) => (d, Some(r)),
        None => (text, None),
    };
    let mut fields = date.split('-');
    let year: i64 = fields.next()?.parse().ok()?;
    let month: i64 = fields.next()?.parse().ok()?;
    let day: i64 = fields.next()?.parse().ok()?;
    if fields.next().is_some()
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=9999).contains(&year)
    {
        return None;
    }
    let mut clock = 0;
    let mut zone = 0;
    if let Some(rest) = rest {
        let split = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
        clock = parse_clock(&rest[..split])?;
        if split < rest.len() {
            zone = parse_zone(&rest[split..])?;
        }
    }
    Some(days_from_civil(year, month, day) * 86_400_000 + clock - zone)
}

/// Epoch milliseconds of an HTTP date (IMF-fixdate, RFC 850, asctime) or an
/// ISO 8601 timestamp.
pub fn parse_http_date(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && text.len() >= 8
        && text.as_bytes().get(4) == Some(&b'-')
    {
        return parse_iso_date(text);
    }
    let (mut day, mut month, mut year, mut clock, mut zone) = (None, None, None, None, 0i64);
    for token in text.split([' ', ',']).filter(|t| !t.is_empty()) {
        if token.contains(':') {
            clock = Some(parse_clock(token)?);
        } else if token.contains('-') && token.starts_with(|c: char| c.is_ascii_digit()) {
            let mut parts = token.split('-');
            day = Some(parts.next()?.parse::<i64>().ok()?);
            month = Some(month_number(parts.next()?)?);
            let y: i64 = parts.next()?.parse().ok()?;
            year = Some(if y < 50 {
                2000 + y
            } else if y < 100 {
                1900 + y
            } else {
                y
            });
        } else if token.bytes().all(|b| b.is_ascii_digit()) {
            let n: i64 = token.parse().ok()?;
            if day.is_none() && token.len() <= 2 {
                day = Some(n);
            } else {
                year = Some(n);
            }
        } else if let Some(m) = month_number(token) {
            month = Some(m);
        } else if let Some(z) = parse_zone(token) {
            zone = z;
        } else if !token.chars().all(|c| c.is_ascii_alphabetic() || c == '.') {
            return None;
        }
    }
    let (day, month, year) = (day?, month?, year?);
    if !(1..=31).contains(&day) || !(0..=9999).contains(&year) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400_000 + clock.unwrap_or(0) - zone)
}

/// Retry-After in milliseconds (delta seconds or HTTP date), or `None`.
pub fn parse_retry_after(value: Option<&str>, now_ms: u64) -> Option<u64> {
    let trimmed = value?.trim();
    if !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return trimmed.parse::<u64>().ok().map(|s| s.saturating_mul(1000));
    }
    let date = parse_http_date(trimmed)?;
    let delta = date - i64::try_from(now_ms).unwrap_or(i64::MAX);
    Some(u64::try_from(delta).unwrap_or(0))
}

static REL_NEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i);\s*rel\s*=\s*"?([^";]*\s)?next(\s[^";]*)?"?\s*(;|$)"#).expect("static regex")
});

static LINK_TARGET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*<([^>]*)>(.*)$").expect("static regex"));

/// Next page URL from a `Link` header (`rel="next"`), or `None`.
pub fn next_link(header: Option<&str>) -> Option<String> {
    let header = header.filter(|h| !h.is_empty())?;
    let bytes = header.as_bytes();
    let mut parts: Vec<&str> = Vec::new();
    let mut start = 0;
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == b',' && header[i + 1..].trim_start().starts_with('<') {
            parts.push(&header[start..i]);
            start = i + 1;
        }
    }
    parts.push(&header[start..]);
    parts.into_iter().find_map(|part| {
        let caps = LINK_TARGET.captures(part)?;
        REL_NEXT
            .is_match(caps.get(2).map_or("", |m| m.as_str()))
            .then(|| caps[1].to_owned())
    })
}
