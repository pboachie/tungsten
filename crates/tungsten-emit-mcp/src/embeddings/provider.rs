// SPDX-License-Identifier: AGPL-3.0-only
//! The two provider kinds of the embedding index.
//!
//! **Command.** The program runs from the directory of `tungsten.yml` with
//! the compiler's environment. It reads one JSON request from standard input
//! and writes one JSON response to standard output (exit status 0):
//!
//! ```json
//! { "protocol": 1, "kind": "document", "model": "m", "dimensions": 64,
//!   "inputs": ["text", "..."] }
//! { "embeddings": [[0.1, 0.2], [0.3, 0.4]] }
//! ```
//!
//! `dimensions` is null when the configuration does not say. The response has
//! one vector per input, in order, all of the same length.
//!
//! **HTTP.** `POST <url>` with `{"model", "input": [...], "dimensions"?,
//! "encoding_format": "float"}` and, when `api_key_env` names a set variable,
//! `Authorization: Bearer <value>`. The response is the common shape
//! `{"data": [{"index": 0, "embedding": [...]}, ...]}`. Plain `http://` only:
//! the compiler links no TLS stack; use a command provider (for example one
//! that runs `curl`) or a local gateway for an HTTPS endpoint.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungsten_core::Diagnostic;

use super::{MAX_DIMENSIONS, Provider, Run};

/// Most bytes read from a provider.
const MAX_RESPONSE_BYTES: usize = 256 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(2);

/// A provider call that failed.
#[derive(Debug)]
pub(super) struct ProviderError {
    code: &'static str,
    message: String,
    help: Option<String>,
}

impl ProviderError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            help: None,
        }
    }

    fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// An error when generation requires the index, else a warning that
    /// says the server falls back to BM25.
    pub(super) fn into_diagnostic(self, required: bool) -> Diagnostic {
        let d = if required {
            Diagnostic::error(self.code, self.message)
        } else {
            Diagnostic::warning(
                self.code,
                format!("{}; the MCP server searches with BM25 only", self.message),
            )
        };
        match self.help {
            Some(help) => d.with_help(help),
            None => d,
        }
    }
}

/// Embed `texts`; one vector per text. `dimensions` is the length to
/// expect, when known.
pub(super) fn embed(
    run: &Run<'_>,
    texts: &[&str],
    dimensions: Option<usize>,
) -> Result<Vec<Vec<f64>>, ProviderError> {
    let config = run.config;
    let timeout = Duration::from_millis(config.timeout_ms);
    let rows = match &config.provider {
        Provider::Command(command) => {
            let request = json!({
                "protocol": 1,
                "kind": "document",
                "model": config.model,
                "dimensions": dimensions,
                "inputs": texts,
            });
            let out = run_command(command, run.project_dir, request.to_string(), timeout)?;
            let value = parse_json(&out, "the command's output")?;
            let rows = value
                .get("embeddings")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    ProviderError::new(
                        "TG0572",
                        "the embedding command's response has no `embeddings` array",
                    )
                    .help(r#"write {"embeddings": [[...], ...]} to standard output"#)
                })?;
            rows.iter()
                .map(|row| vector(row, "embeddings"))
                .collect::<Result<Vec<_>, _>>()?
        }
        Provider::Http(url) => {
            let target = plain_http(url)?;
            let mut request = json!({
                "model": config.model,
                "input": texts,
                "encoding_format": "float",
            });
            if let (Some(n), Some(map)) = (config.dimensions, request.as_object_mut()) {
                map.insert("dimensions".into(), json!(n));
            }
            let key = match &config.api_key_env {
                Some(name) => match std::env::var(name) {
                    Ok(v) if !v.is_empty() => Some(v),
                    _ => {
                        return Err(ProviderError::new(
                            "TG0573",
                            format!("the environment variable `{name}` named by `api_key_env` is not set"),
                        )
                        .help("export the API key before running tungsten generate; it is never written to the output"));
                    }
                },
                None => None,
            };
            let body = http_post(
                url,
                &target,
                key.as_deref(),
                request.to_string().as_bytes(),
                timeout,
            )?;
            let value = parse_json(&body, "the endpoint's response")?;
            let data = value.get("data").and_then(Value::as_array).ok_or_else(|| {
                ProviderError::new("TG0572", "the endpoint's response has no `data` array")
                    .help(r#"expected {"data": [{"index": 0, "embedding": [...]}, ...]}"#)
            })?;
            let mut indexed = Vec::with_capacity(data.len());
            for (position, item) in data.iter().enumerate() {
                let index = item
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(position);
                let row = item.get("embedding").ok_or_else(|| {
                    ProviderError::new("TG0572", "an item of `data` has no `embedding`")
                })?;
                indexed.push((index, vector(row, "embedding")?));
            }
            indexed.sort_by_key(|(i, _)| *i);
            if indexed.iter().enumerate().any(|(p, (i, _))| p != *i) {
                return Err(ProviderError::new(
                    "TG0572",
                    "the `index` values of the endpoint's `data` are not 0 to n-1",
                ));
            }
            indexed.into_iter().map(|(_, v)| v).collect()
        }
    };
    check(&rows, texts.len(), dimensions.or(config.dimensions))?;
    Ok(rows)
}

fn check(
    rows: &[Vec<f64>],
    expected: usize,
    dimensions: Option<usize>,
) -> Result<(), ProviderError> {
    if rows.len() != expected {
        return Err(ProviderError::new(
            "TG0572",
            format!(
                "the provider returned {} vectors for {expected} inputs",
                rows.len()
            ),
        ));
    }
    let want = dimensions.or_else(|| rows.first().map(Vec::len));
    for row in rows {
        if row.is_empty() || row.len() > MAX_DIMENSIONS || Some(row.len()) != want {
            return Err(ProviderError::new(
                "TG0572",
                format!(
                    "the provider returned a vector of {} values where {} were expected",
                    row.len(),
                    want.map_or_else(|| "1 to 8192".to_string(), |n| n.to_string())
                ),
            )
            .help("set `dimensions` to the length the model produces, and keep it constant"));
        }
    }
    Ok(())
}

fn vector(value: &Value, what: &str) -> Result<Vec<f64>, ProviderError> {
    value
        .as_array()
        .and_then(|a| {
            a.iter()
                .map(|x| x.as_f64().filter(|f| f.is_finite()))
                .collect::<Option<Vec<f64>>>()
        })
        .ok_or_else(|| {
            ProviderError::new(
                "TG0572",
                format!("`{what}` holds something that is not an array of finite numbers"),
            )
        })
}

fn parse_json(bytes: &[u8], what: &str) -> Result<Value, ProviderError> {
    serde_json::from_slice(bytes)
        .map_err(|e| ProviderError::new("TG0572", format!("{what} is not JSON ({e})")))
}

// -------------------------------------------------------------- command

fn run_command(
    command: &str,
    project_dir: &Path,
    input: String,
    timeout: Duration,
) -> Result<Vec<u8>, ProviderError> {
    let failed = |what: String| {
        ProviderError::new("TG0571", format!("embedding command `{command}` {what}"))
    };
    let mut words = tungsten_emit::external::command_line(command)
        .ok_or_else(|| failed("has an unbalanced quote".into()))?;
    let program = words.remove(0);
    let cwd = if project_dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        project_dir
    };
    let path = if program.contains('/') || program.contains('\\') {
        let candidate = Path::new(&program);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            cwd.join(candidate)
        }
    } else {
        let search = std::env::var_os("PATH").unwrap_or_default();
        tungsten_emit::external::find_executable(&program, &search)
            .ok_or_else(|| failed(format!("was not found: `{program}` is not on PATH")))?
    };
    let mut child = std::process::Command::new(&path)
        .args(&words)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| failed(format!("could not be started ({e})")))?;
    if let Some(mut stdin) = child.stdin.take() {
        thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let stdout = child.stdout.take().map(pipe_reader);
    let stderr = child.stderr.take().map(pipe_reader);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(
                    failed(format!("did not finish within {} ms", timeout.as_millis())).help(
                        "raise `timeout_ms`, or send fewer documents per call with `batch_size`",
                    ),
                );
            }
            Err(e) => {
                let _ = child.kill();
                return Err(failed(format!("could not be waited for ({e})")));
            }
        }
    };
    let collect = |rx: Option<mpsc::Receiver<Vec<u8>>>| {
        rx.and_then(|rx| rx.recv_timeout(Duration::from_secs(2)).ok())
            .unwrap_or_default()
    };
    let out = collect(stdout);
    let err = String::from_utf8_lossy(&collect(stderr)).into_owned();
    if !status.success() {
        let tail: Vec<&str> = err.lines().rev().take(5).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join(" / ");
        return Err(failed(format!(
            "exited with {}{}",
            status
                .code()
                .map_or_else(|| "a signal".to_string(), |c| format!("status {c}")),
            if tail.is_empty() {
                String::new()
            } else {
                format!(": {tail}")
            }
        )));
    }
    Ok(out)
}

fn pipe_reader(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = vec![];
        let _ = pipe
            .by_ref()
            .take(MAX_RESPONSE_BYTES as u64)
            .read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    rx
}

// ----------------------------------------------------------------- http

/// The parsed `url`, or the reason the compiler cannot call it.
fn plain_http(url: &str) -> Result<super::HttpUrl, ProviderError> {
    let parsed = super::HttpUrl::parse(url).ok_or_else(|| {
        ProviderError::new(
            "TG0571",
            format!("embedding endpoint `{url}` is not an http:// URL"),
        )
    })?;
    if parsed.secure {
        return Err(ProviderError::new(
            "TG0573",
            format!("embedding endpoint `{url}` is not plain http://: the compiler has no TLS support"),
        )
        .help("use `provider: command` with a program that calls the endpoint (for example curl), or a local http:// gateway; the MCP server's own query embedder supports https"));
    }
    Ok(parsed)
}

fn http_post(
    url: &str,
    parsed: &super::HttpUrl,
    key: Option<&str>,
    body: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, ProviderError> {
    let failed =
        |what: String| ProviderError::new("TG0571", format!("embedding endpoint `{url}` {what}"));
    let host = parsed.host.clone();
    let port = parsed.port.unwrap_or(80);
    let deadline = Instant::now() + timeout;
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| failed(format!("did not answer within {} ms", timeout.as_millis())))
    };
    let target = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| failed(format!("cannot be resolved ({e})")))?
        .next()
        .ok_or_else(|| failed("resolves to no address".into()))?;
    let mut stream = TcpStream::connect_timeout(&target, remaining()?)
        .map_err(|e| failed(format!("cannot be reached ({e})")))?;
    let path = parsed.target.clone();
    let authority = if parsed.port.is_some() {
        format!("{host}:{port}")
    } else {
        host.clone()
    };
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(key) = key {
        head.push_str(&format!("Authorization: Bearer {key}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .set_write_timeout(Some(remaining()?))
        .map_err(|e| failed(format!("failed ({e})")))?;
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body))
        .map_err(|e| failed(format!("failed while sending ({e})")))?;
    let mut raw = vec![];
    let mut chunk = [0u8; 16 * 1024];
    loop {
        stream
            .set_read_timeout(Some(remaining()?))
            .map_err(|e| failed(format!("failed ({e})")))?;
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if raw.len() > MAX_RESPONSE_BYTES {
                    return Err(failed("sent more than 256 MiB".into()));
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(failed(format!(
                    "did not answer within {} ms",
                    timeout.as_millis()
                )));
            }
            Err(e) => return Err(failed(format!("failed while reading ({e})"))),
        }
    }
    let (status, headers, payload) = split_response(&raw).ok_or_else(|| {
        ProviderError::new(
            "TG0572",
            format!("embedding endpoint `{url}` sent no valid HTTP response"),
        )
    })?;
    let payload = if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(payload).ok_or_else(|| {
            ProviderError::new(
                "TG0572",
                format!("embedding endpoint `{url}` sent a malformed chunked body"),
            )
        })?
    } else {
        payload.to_vec()
    };
    if !(200..300).contains(&status) {
        let text = String::from_utf8_lossy(&payload);
        let text: String = text.chars().take(200).collect();
        return Err(ProviderError::new(
            "TG0573",
            format!(
                "embedding endpoint `{url}` answered HTTP {status}: {}",
                text.trim()
            ),
        ));
    }
    Ok(payload)
}

/// Status code, header block and body of a raw HTTP/1.x response.
fn split_response(raw: &[u8]) -> Option<(u16, String, &[u8])> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..end]).into_owned();
    let status = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;
    Some((status, head, &raw[end + 4..]))
}

/// The body of a `Transfer-Encoding: chunked` message.
fn dechunk(mut rest: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![];
    loop {
        let line_end = rest.windows(2).position(|w| w == b"\r\n")?;
        let size_text = std::str::from_utf8(&rest[..line_end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        if rest.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}
