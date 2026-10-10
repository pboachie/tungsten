// SPDX-License-Identifier: Apache-2.0
//! From flag text to the JSON values of the arguments object.
//!
//! A sensitive flag never takes its value on the command line: it takes `-`
//! (one line of standard input) or `env:NAME` (an environment variable), and
//! falls back to `<PREFIX>_<ARGUMENT>` when it is absent.

use clap::ArgMatches;
use serde_json::{Map, Number, Value};
use tungsten_runtime::{Binary, BodyEncoding};

use crate::config::upper_words;
use crate::ctx::{Ctx, Fail};
use crate::spec::{CliFlag, FlagKind};

/// The clap argument id of a table flag.
pub(crate) fn id_of(arg: &str) -> String {
    format!("arg:{arg}")
}

pub(crate) fn number(text: &str) -> Result<Number, String> {
    text.trim()
        .parse::<Number>()
        .map_err(|_| format!("`{text}` is not a number"))
}

/// The checks of a flag's text that need no input: what clap validates
/// before the command runs.
pub(crate) fn check_text(kind: &FlagKind, text: &str) -> Result<(), String> {
    match kind {
        FlagKind::Integer => {
            let n = number(text)?;
            if n.is_i64() || n.is_u64() {
                Ok(())
            } else {
                Err(format!("`{text}` is not an integer"))
            }
        }
        FlagKind::Number => number(text).map(|_| ()),
        FlagKind::Boolean => match text {
            "true" | "false" => Ok(()),
            _ => Err(format!("`{text}` is not `true` or `false`")),
        },
        FlagKind::Enum(values) => {
            if values.iter().any(|v| v == text) {
                Ok(())
            } else {
                Err(format!("`{text}` is not one of: {}", values.join(", ")))
            }
        }
        FlagKind::File if !text.starts_with('@') => {
            Err("expected `@path` of a file (or `@-` for standard input)".into())
        }
        FlagKind::Array(inner) => check_text(inner, text),
        FlagKind::String | FlagKind::Json | FlagKind::File => Ok(()),
    }
}

/// What clap accepts for a sensitive flag: a reference to the secret.
pub(crate) fn check_secret_ref(text: &str) -> Result<(), String> {
    if text == "-" || text.strip_prefix("env:").is_some_and(|n| !n.is_empty()) {
        Ok(())
    } else {
        Err(
            "a secret is never given on the command line; pass `-` to read it from standard \
             input or `env:NAME` to read it from an environment variable"
                .into(),
        )
    }
}

fn read_ref(ctx: &mut Ctx<'_>, reference: &str) -> Result<Vec<u8>, Fail> {
    if reference == "-" {
        return ctx.stdin_all();
    }
    std::fs::read(reference).map_err(|e| Fail::Usage(format!("cannot read {reference}: {e}")))
}

fn binary(ctx: &mut Ctx<'_>, reference: &str) -> Result<Value, Fail> {
    let data = read_ref(ctx, reference)?;
    let mut b = Binary::new(data);
    if reference != "-"
        && let Some(name) = std::path::Path::new(reference).file_name()
    {
        b = b.with_filename(name.to_string_lossy());
    }
    serde_json::to_value(&b).map_err(|e| Fail::Internal(e.to_string()))
}

pub(crate) fn json_from(ctx: &mut Ctx<'_>, text: &str) -> Result<Value, Fail> {
    let bytes = if text == "-" {
        ctx.stdin_all()?
    } else if let Some(path) = text.strip_prefix('@') {
        read_ref(ctx, path)?
    } else {
        text.as_bytes().to_vec()
    };
    serde_json::from_slice(&bytes).map_err(|e| Fail::Usage(format!("invalid JSON: {e}")))
}

fn parse_kind(ctx: &mut Ctx<'_>, kind: &FlagKind, text: &str) -> Result<Value, Fail> {
    check_text(kind, text).map_err(Fail::Usage)?;
    Ok(match kind {
        FlagKind::String | FlagKind::Enum(_) => Value::String(text.to_string()),
        FlagKind::Integer | FlagKind::Number => Value::Number(number(text).map_err(Fail::Usage)?),
        FlagKind::Boolean => Value::Bool(text == "true"),
        FlagKind::Json => json_from(ctx, text)?,
        FlagKind::File => binary(ctx, &text[1..])?,
        FlagKind::Array(inner) => parse_kind(ctx, inner, text)?,
    })
}

fn secret(ctx: &mut Ctx<'_>, reference: &str) -> Result<String, Fail> {
    check_secret_ref(reference).map_err(Fail::Usage)?;
    match reference.strip_prefix("env:") {
        Some(name) => ctx
            .env
            .get(name)
            .cloned()
            .ok_or_else(|| Fail::Usage(format!("the environment variable {name} is not set"))),
        None => ctx.stdin_line(),
    }
}

/// The arguments object the flags describe. Absent optional flags are not
/// keys.
pub(crate) fn collect(
    ctx: &mut Ctx<'_>,
    flags: &[CliFlag],
    m: &ArgMatches,
) -> Result<Map<String, Value>, Fail> {
    let mut args = Map::new();
    for flag in flags {
        let mut texts: Vec<String> = m
            .get_many::<String>(&id_of(&flag.arg))
            .map(|v| v.cloned().collect())
            .unwrap_or_default();
        let from_env = texts.is_empty() && flag.sensitive;
        if from_env {
            let name = format!("{}_{}", ctx.spec.env_prefix, upper_words(&flag.arg));
            match ctx.env.get(&name).filter(|v| !v.is_empty()) {
                Some(v) => texts.push(v.clone()),
                None => continue,
            }
        } else if texts.is_empty() {
            continue;
        }
        let mut values = Vec::new();
        for text in texts {
            let text = if flag.sensitive && !from_env {
                secret(ctx, &text)?
            } else {
                text
            };
            let value =
                parse_kind(ctx, &flag.kind, &text).map_err(|f| prefix_flag(f, &flag.flag))?;
            values.push(value);
        }
        let value = match &flag.kind {
            FlagKind::Array(_) => Value::Array(values),
            _ => values.into_iter().next().unwrap_or(Value::Null),
        };
        args.insert(flag.arg.clone(), value);
    }
    Ok(args)
}

fn prefix_flag(f: Fail, flag: &str) -> Fail {
    match f {
        Fail::Usage(m) => Fail::Usage(format!("--{flag}: {m}")),
        other => other,
    }
}

/// The value of `--body` for a body of `encoding` (`None` when the SDK does
/// not describe the body: JSON).
pub(crate) fn body_value(
    ctx: &mut Ctx<'_>,
    raw: &str,
    encoding: Option<BodyEncoding>,
) -> Result<Value, Fail> {
    let result = match encoding {
        Some(BodyEncoding::Bytes) => match raw {
            "-" => binary(ctx, "-"),
            r if r.starts_with('@') => binary(ctx, &r[1..]),
            _ => Err(Fail::Usage(
                "this body is bytes: pass `@path` of a file or `-` for standard input".into(),
            )),
        },
        Some(BodyEncoding::Text) => {
            let bytes = if raw == "-" {
                ctx.stdin_all()?
            } else if let Some(path) = raw.strip_prefix('@') {
                read_ref(ctx, path)?
            } else {
                raw.as_bytes().to_vec()
            };
            String::from_utf8(bytes)
                .map(Value::String)
                .map_err(|_| Fail::Usage("the body is not valid UTF-8".into()))
        }
        _ => json_from(ctx, raw),
    };
    result.map_err(|f| prefix_flag(f, "body"))
}
