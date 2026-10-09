// SPDX-License-Identifier: Apache-2.0
//! Consistency of the command table, checked before the command tree is
//! built: clap rejects duplicate or clashing names by panicking in debug
//! builds, and a table that reaches the kit is data from another crate.

use std::collections::BTreeSet;

use crate::spec::{CliFlag, CliSpec, FlagKind};

/// Flags the kit adds to every command; a table flag cannot take them.
pub const RESERVED_FLAGS: &[&str] = &[
    "all",
    "base-url",
    "body",
    "config",
    "dry-run",
    "help",
    "i-understand",
    "idempotency-key",
    "json",
    "no-color",
    "profile",
    "timeout",
    "verify",
    "version",
    "yes",
];

/// First path segments of the kit's own commands; a resource cannot take
/// them.
pub const RESERVED_COMMANDS: &[&str] = &["auth", "explain-error", "help", "operations", "schema"];

fn name_ok(s: &str, extra: &[char]) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || extra.contains(&c))
}

pub(crate) fn validate(spec: &CliSpec) -> Result<(), String> {
    if !name_ok(&spec.bin, &['_', '.']) {
        return Err(format!("binary name {:?} is not a command name", spec.bin));
    }
    if spec.env_prefix.is_empty()
        || !spec
            .env_prefix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(format!(
            "environment prefix {:?} must be upper-case letters, digits and `_`",
            spec.env_prefix
        ));
    }
    let dir = &spec.config_dir;
    if dir.is_empty() || dir == ".." || dir == "." || dir.contains(['/', '\\']) {
        return Err(format!(
            "config directory {dir:?} must be a single path segment"
        ));
    }
    let mut paths: Vec<(&[String], String)> = Vec::new();
    for op in &spec.ops {
        let what = format!("operation {}", op.id);
        flags(&op.flags, op.body_arg.as_deref(), &what)?;
        paths.push((&op.path, what));
    }
    for m in &spec.macros {
        let what = format!("macro {}", m.name);
        flags(&m.flags, None, &what)?;
        paths.push((&m.path, what));
    }
    for (path, what) in &paths {
        if path.is_empty() {
            return Err(format!("{what} has an empty command path"));
        }
        if let Some(bad) = path.iter().find(|s| !name_ok(s, &['_'])) {
            return Err(format!("{what} has an invalid command name {bad:?}"));
        }
        if RESERVED_COMMANDS.contains(&path[0].as_str()) {
            return Err(format!(
                "{what} starts its command path with `{}`, which is a command of the kit",
                path[0]
            ));
        }
    }
    let mut seen: BTreeSet<&[String]> = BTreeSet::new();
    for (path, what) in &paths {
        if !seen.insert(path) {
            return Err(format!(
                "{what} repeats the command path `{}`",
                path.join(" ")
            ));
        }
    }
    for (a, what) in &paths {
        for (b, other) in &paths {
            if a.len() < b.len() && b[..a.len()] == **a {
                return Err(format!(
                    "{what} is the command `{}`, which {other} uses as a group",
                    a.join(" ")
                ));
            }
        }
    }
    Ok(())
}

fn flags(list: &[CliFlag], body_arg: Option<&str>, what: &str) -> Result<(), String> {
    let mut names = BTreeSet::new();
    let mut args = BTreeSet::new();
    if let Some(b) = body_arg {
        args.insert(b);
    }
    for f in list {
        if !name_ok(&f.flag, &[]) || f.flag.chars().any(|c| c.is_ascii_uppercase()) {
            return Err(format!("{what} has an invalid flag name {:?}", f.flag));
        }
        if RESERVED_FLAGS.contains(&f.flag.as_str()) {
            return Err(format!(
                "{what} has a flag `--{}`, which is a flag of the kit",
                f.flag
            ));
        }
        if !names.insert(f.flag.as_str()) {
            return Err(format!("{what} repeats the flag `--{}`", f.flag));
        }
        if f.arg.is_empty() || !args.insert(f.arg.as_str()) {
            return Err(format!(
                "{what} gives the argument {:?} to two flags",
                f.arg
            ));
        }
        kind(&f.kind, false).map_err(|e| format!("{what}, flag `--{}`: {e}", f.flag))?;
    }
    Ok(())
}

fn kind(k: &FlagKind, in_array: bool) -> Result<(), String> {
    match k {
        FlagKind::Enum(values) if values.is_empty() => Err("an enum without values".into()),
        FlagKind::Array(_) if in_array => Err("an array of arrays".into()),
        FlagKind::Array(inner) => kind(inner, true),
        _ => Ok(()),
    }
}
