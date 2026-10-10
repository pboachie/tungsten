// SPDX-License-Identifier: AGPL-3.0-only
//! The optional embedding index of `search_tools` (planning/02 D6 "As
//! built: embedding index"): configuration (`targets.mcp.search.embeddings`),
//! the provider calls and the two files written next to `manifest.json`.
//!
//! The index is off by default and no model ships with tungsten. When the
//! target configures a provider, `tungsten generate` sends one document per
//! tool (the terms of its BM25 document, in order, joined by spaces) to it in
//! batches and writes:
//!
//! - `index.embeddings.bin`: the vectors, `count x dimensions` little-endian
//!   IEEE-754 `f32` values, row-major, in tool order, each L2-normalized (so
//!   cosine similarity is a dot product);
//! - `index.embeddings.json`: the header (`format`, `model`, `dimensions`,
//!   `count`, `encoding`, `normalized`, `checksum` of the `.bin` file as
//!   `blake3:<hex>`, and `docs`: the tool name and the content hash of each
//!   document).
//!
//! The vectors are cached by content hash: a regeneration reads the files
//! it wrote last time and calls the provider only for documents whose hash
//! (or whose model or dimensions) changed. Without a provider call being
//! allowed (`generate --check`, `--dry-run`, `diff`, `report`) the files are
//! produced from the cache alone and left out when it does not cover every
//! tool, so `generate --check` reports them stale. A provider failure never
//! breaks generation unless the configuration says `required: true`: it is
//! a warning and the server falls back to BM25.

mod provider;

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};
use tungsten_core::{Diagnostic, Diagnostics, Digest};

/// File name of the vectors.
pub const BIN_FILE: &str = "index.embeddings.bin";
/// File name of the header.
pub const HEADER_FILE: &str = "index.embeddings.json";
/// Format version of the two files.
pub const FORMAT: u32 = 1;
/// Documents per provider call when `batch_size` is not set.
pub const DEFAULT_BATCH_SIZE: usize = 32;
/// Most documents per provider call.
pub const MAX_BATCH_SIZE: usize = 512;
/// Wall-clock limit of one provider call when `timeout_ms` is not set.
pub const DEFAULT_TIMEOUT_MS: u64 = 60_000;
/// Largest vector length accepted.
pub const MAX_DIMENSIONS: usize = 8192;

/// Where the vectors come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provider {
    /// A command line, run from the directory of `tungsten.yml`: one JSON
    /// request on standard input, one JSON response on standard output.
    Command(String),
    /// An endpoint that takes the common `/embeddings` request shape.
    Http(String),
}

/// `targets.mcp.search.embeddings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub provider: Provider,
    pub model: String,
    /// The vector length; checked against every response when set, taken
    /// from the first response otherwise.
    pub dimensions: Option<usize>,
    /// Name of the environment variable that holds the API key of an HTTP
    /// endpoint. The key itself is never written anywhere.
    pub api_key_env: Option<String>,
    /// A provider failure fails generation instead of falling back to BM25.
    pub required: bool,
    pub batch_size: usize,
    pub timeout_ms: u64,
}

const KEYS: &[&str] = &[
    "provider",
    "command",
    "url",
    "model",
    "dimensions",
    "api_key_env",
    "required",
    "batch_size",
    "timeout_ms",
];

/// Read `targets.mcp.search`. `Ok(None)` when no embeddings are configured;
/// a problem is a TG0570 diagnostic (an error when the configuration says
/// `required: true`, a warning otherwise, and then the index is off).
pub(crate) fn parse(options: &Value) -> Result<Option<Config>, Diagnostic> {
    let Some(search) = options.get("search") else {
        return Ok(None);
    };
    let Some(search) = search.as_object() else {
        return Err(invalid(false, "`search` must be an object".into(), search));
    };
    if let Some(unknown) = search.keys().find(|k| *k != "embeddings") {
        return Err(invalid(
            false,
            format!("unknown key `search.{unknown}`; the only key is `embeddings`"),
            &search[unknown],
        ));
    }
    let Some(cfg) = search.get("embeddings") else {
        return Ok(None);
    };
    let Some(cfg) = cfg.as_object() else {
        return Err(invalid(
            false,
            "`search.embeddings` must be an object".into(),
            cfg,
        ));
    };
    let required = match cfg.get("required") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(other) => {
            return Err(invalid(
                false,
                "`required` must be true or false".into(),
                other,
            ));
        }
    };
    let bad = |message: String, got: &Value| invalid(required, message, got);
    if let Some(unknown) = cfg.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(bad(
            format!(
                "unknown key `search.embeddings.{unknown}`; the keys are {}",
                KEYS.join(", ")
            ),
            &cfg[unknown],
        ));
    }
    let text = |key: &str| -> Result<Option<String>, Diagnostic> {
        match cfg.get(key) {
            None => Ok(None),
            Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.trim().to_string())),
            Some(other) => Err(bad(
                format!("`search.embeddings.{key}` must be a non-empty string"),
                other,
            )),
        }
    };
    let number = |key: &str, min: u64, max: u64| -> Result<Option<u64>, Diagnostic> {
        match cfg.get(key) {
            None => Ok(None),
            Some(v) => match v.as_u64() {
                Some(n) if (min..=max).contains(&n) => Ok(Some(n)),
                _ => Err(bad(
                    format!("`search.embeddings.{key}` must be an integer from {min} to {max}"),
                    v,
                )),
            },
        }
    };
    let provider = match text("provider")?.as_deref() {
        Some("command") => match text("command")? {
            Some(command) if tungsten_emit::external::command_line(&command).is_some() => {
                if cfg.contains_key("url") {
                    return Err(bad(
                        "`url` belongs to `provider: http`, not `provider: command`".into(),
                        &cfg["url"],
                    ));
                }
                Provider::Command(command)
            }
            Some(command) => {
                return Err(bad(
                    "`search.embeddings.command` has an unbalanced quote".into(),
                    &Value::String(command),
                ));
            }
            None => {
                return Err(bad(
                    "`provider: command` needs `command`".into(),
                    &Value::Null,
                ));
            }
        },
        Some("http") => match text("url")? {
            Some(url) if is_http_url(&url) => {
                if cfg.contains_key("command") {
                    return Err(bad(
                        "`command` belongs to `provider: command`, not `provider: http`".into(),
                        &cfg["command"],
                    ));
                }
                Provider::Http(url)
            }
            Some(url) => {
                return Err(bad(
                    "`search.embeddings.url` must be an http:// or https:// URL".into(),
                    &Value::String(url),
                ));
            }
            None => return Err(bad("`provider: http` needs `url`".into(), &Value::Null)),
        },
        Some(other) => {
            return Err(bad(
                "`search.embeddings.provider` must be `command` or `http`".into(),
                &Value::String(other.to_string()),
            ));
        }
        None => {
            return Err(bad(
                "`search.embeddings.provider` is required: `command` or `http`".into(),
                &Value::Null,
            ));
        }
    };
    let Some(model) = text("model")? else {
        return Err(bad(
            "`search.embeddings.model` is required (it is recorded in the index header and keys the cache)"
                .into(),
            &Value::Null,
        ));
    };
    let api_key_env = text("api_key_env")?;
    if let Some(name) = &api_key_env {
        if !is_env_name(name) {
            return Err(bad(
                "`search.embeddings.api_key_env` must be the name of an environment variable"
                    .into(),
                &Value::String(name.clone()),
            ));
        }
        if matches!(provider, Provider::Command(_)) {
            return Err(bad(
                "`api_key_env` belongs to `provider: http`; a command reads its own environment"
                    .into(),
                &Value::String(name.clone()),
            ));
        }
    }
    Ok(Some(Config {
        provider,
        model,
        dimensions: number("dimensions", 1, MAX_DIMENSIONS as u64)?.map(|n| n as usize),
        api_key_env,
        required,
        batch_size: number("batch_size", 1, MAX_BATCH_SIZE as u64)?
            .map_or(DEFAULT_BATCH_SIZE, |n| n as usize),
        timeout_ms: number("timeout_ms", 1, 3_600_000)?.unwrap_or(DEFAULT_TIMEOUT_MS),
    }))
}

fn invalid(required: bool, message: String, got: &Value) -> Diagnostic {
    let d = if required {
        Diagnostic::error(
            "TG0570",
            format!("target option `mcp.search.embeddings`: {message}"),
        )
    } else {
        Diagnostic::warning(
            "TG0570",
            format!("target option `mcp.search.embeddings`: {message}; the embedding index is off"),
        )
    };
    let help = "example: search: { embeddings: { provider: command, command: \"node embed.mjs\", model: my-model } }";
    if got.is_null() {
        d.with_help(help)
    } else {
        d.with_help(format!("got {got}; {help}"))
    }
}

fn is_http_url(s: &str) -> bool {
    HttpUrl::parse(s).is_some()
}

/// `http://` or `https://`, a host, an optional port, a path and query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpUrl {
    pub secure: bool,
    pub host: String,
    pub port: Option<u16>,
    /// Path and query, starting with `/`.
    pub target: String,
}

impl HttpUrl {
    pub(crate) fn parse(s: &str) -> Option<HttpUrl> {
        let (secure, rest) = match s.split_once("://")? {
            ("http", rest) => (false, rest),
            ("https", rest) => (true, rest),
            _ => return None,
        };
        let (authority, target) = match rest.find(['/', '?']) {
            Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
            Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
            None => (rest, "/".to_string()),
        };
        if authority.contains('@') || target.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return None;
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !p.contains(']') => (h, Some(p.parse::<u16>().ok()?)),
            _ => (authority, None),
        };
        let valid = !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-._".contains(c));
        valid.then(|| HttpUrl {
            secure,
            host: host.to_string(),
            port,
            target,
        })
    }
}

fn is_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// What `build` was asked to do and where.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Run<'a> {
    pub config: &'a Config,
    /// Working directory of a command provider: the directory of
    /// `tungsten.yml`.
    pub project_dir: &'a Path,
    /// The target's output directory, where the previous run's files are.
    pub out_dir: &'a Path,
    /// Whether the provider may be called (a write, not a check).
    pub live: bool,
}

/// The files of the embedding index and what happened.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// Path relative to the target directory and contents.
    pub files: Vec<(String, Vec<u8>)>,
    pub diagnostics: Diagnostics,
}

/// One document of the index: the tool and its text.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Doc<'a> {
    pub id: &'a str,
    pub text: &'a str,
}

/// The content hash that keys the cache: BLAKE3 of the text, first 32 hex
/// digits.
pub(crate) fn content_hash(text: &str) -> String {
    let digest = Digest::of(text.as_bytes());
    digest.0.trim_start_matches("blake3:")[..32].to_string()
}

/// Build the two files for `docs` (tool order).
pub(crate) fn build(run: &Run<'_>, docs: &[Doc<'_>]) -> Outcome {
    let mut outcome = Outcome::default();
    if docs.is_empty() {
        return outcome;
    }
    let config = run.config;
    let hashes: Vec<String> = docs.iter().map(|d| content_hash(d.text)).collect();
    let cache = load_cache(run.out_dir, config);
    let mut vectors: Vec<Option<Vec<f32>>> = hashes
        .iter()
        .map(|h| cache.vectors.get(h).cloned())
        .collect();
    let missing: Vec<usize> = (0..docs.len()).filter(|i| vectors[*i].is_none()).collect();
    let reused = docs.len() - missing.len();
    let mut dimensions = config.dimensions.or(cache.dimensions);
    if !missing.is_empty() {
        if !run.live {
            return outcome;
        }
        for batch in missing.chunks(config.batch_size) {
            let texts: Vec<&str> = batch.iter().map(|i| docs[*i].text).collect();
            match provider::embed(run, &texts, dimensions) {
                Ok(rows) => {
                    for (i, row) in batch.iter().zip(rows) {
                        dimensions = Some(row.len());
                        vectors[*i] = Some(normalize(&row));
                    }
                }
                Err(d) => {
                    outcome.diagnostics.push(d.into_diagnostic(config.required));
                    return outcome;
                }
            }
        }
    }
    let Some(dimensions) = dimensions else {
        return outcome;
    };
    let mut bin = Vec::with_capacity(docs.len() * dimensions * 4);
    for v in &vectors {
        let Some(v) = v else {
            return outcome;
        };
        for x in v {
            bin.extend_from_slice(&x.to_le_bytes());
        }
    }
    let header = json!({
        "format": FORMAT,
        "model": config.model,
        "dimensions": dimensions,
        "count": docs.len(),
        "encoding": "f32le",
        "normalized": true,
        "checksum": Digest::of(&bin).0,
        "docs": docs
            .iter()
            .zip(&hashes)
            .map(|(d, h)| json!({ "id": d.id, "hash": h }))
            .collect::<Vec<_>>(),
    });
    outcome.files.push((
        HEADER_FILE.into(),
        crate::json::pretty(&header).into_bytes(),
    ));
    outcome.files.push((BIN_FILE.into(), bin));
    if run.live {
        outcome.diagnostics.push(Diagnostic::info(
            "TG0574",
            format!(
                "embedding index: {} tools, model `{}`, {dimensions} dimensions ({} embedded, {reused} reused from the previous run)",
                docs.len(),
                config.model,
                docs.len() - reused,
            ),
        ));
    }
    outcome
}

/// L2-normalized `f32` copy of `row` (a zero vector stays zero).
fn normalize(row: &[f64]) -> Vec<f32> {
    let norm = row.iter().map(|x| x * x).sum::<f64>().sqrt();
    row.iter()
        .map(|x| if norm > 0.0 { (x / norm) as f32 } else { 0.0 })
        .collect()
}

#[derive(Default)]
struct Cache {
    vectors: HashMap<String, Vec<f32>>,
    dimensions: Option<usize>,
}

/// The vectors the previous run wrote, by content hash. Empty when the
/// files are missing, unreadable, for another model or dimensions, or fail
/// their checksum.
fn load_cache(out_dir: &Path, config: &Config) -> Cache {
    let read = || -> Option<Cache> {
        let header: Value =
            serde_json::from_slice(&std::fs::read(out_dir.join(HEADER_FILE)).ok()?).ok()?;
        let bin = std::fs::read(out_dir.join(BIN_FILE)).ok()?;
        if header.get("format")?.as_u64()? != u64::from(FORMAT)
            || header.get("model")?.as_str()? != config.model
        {
            return None;
        }
        let dimensions = usize::try_from(header.get("dimensions")?.as_u64()?).ok()?;
        if dimensions == 0
            || config.dimensions.is_some_and(|d| d != dimensions)
            || header.get("checksum")?.as_str()? != Digest::of(&bin).0
        {
            return None;
        }
        let docs = header.get("docs")?.as_array()?;
        if bin.len() != docs.len().checked_mul(dimensions)?.checked_mul(4)? {
            return None;
        }
        let mut vectors = HashMap::new();
        for (doc, row) in docs.iter().zip(bin.chunks_exact(dimensions * 4)) {
            let vector: Vec<f32> = row
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            vectors.insert(doc.get("hash")?.as_str()?.to_string(), vector);
        }
        Some(Cache {
            vectors,
            dimensions: Some(dimensions),
        })
    };
    read().unwrap_or_default()
}
