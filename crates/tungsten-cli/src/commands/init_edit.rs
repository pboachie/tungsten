// SPDX-License-Identifier: AGPL-3.0-only
//! Additive project edits: a targeted edit of `tungsten.yml` that keeps the
//! rest of the file (comments, the schema header, formatting), an exclusive
//! backup and an atomic replacement.
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::args::InitArgs;
use serde_json::{Map, Value, json};

/// The outcome of [`add_outputs`].
pub(super) struct Added {
    /// The exclusive copy of the previous manifest.
    pub backup: PathBuf,
    /// Set when comments inside a rewritten part of the manifest were dropped.
    pub warning: Option<String>,
}

/// Backups are `tungsten.yml.init-backup`, then `.init-backup.1`, `.2`, ...
const MAX_BACKUPS: u32 = 1000;

pub(super) fn add_outputs(args: &InitArgs) -> Result<Added, String> {
    let manifest = args.dir.join("tungsten.yml");
    let original =
        std::fs::read_to_string(&manifest).map_err(|_| "cannot read project manifest")?;
    let (parsed, source) = tungsten_config::parse_yaml("tungsten.yml", &original);
    let mut value = parsed.map_err(|_| "cannot parse existing project manifest")?;
    let before = value
        .get("targets")
        .and_then(Value::as_object)
        .cloned()
        .ok_or("existing project has no target mapping")?;
    let targets = value
        .get_mut("targets")
        .and_then(Value::as_object_mut)
        .ok_or("existing project has no target mapping")?;
    let mut selected = args.add_targets.clone();
    if selected
        .iter()
        .any(|s| matches!(s.as_str(), "mcp" | "webmcp"))
        && !targets.contains_key("typescript")
        && !selected.iter().any(|s| s == "typescript")
    {
        selected.insert(0, "typescript".into());
    }
    for target in &selected {
        if target != "cli" && !tungsten_config::KNOWN_TARGETS.contains(&target.as_str()) {
            return Err(
                "unknown output; choose typescript, python, rust, cli, mcp, webmcp or docs".into(),
            );
        }
        let name = if target == "cli" {
            "rust"
        } else {
            target.as_str()
        };
        if let Some(existing) = targets.get_mut(name) {
            if target == "cli" {
                let object = existing
                    .as_object_mut()
                    .ok_or("existing Rust target must be a mapping")?;
                if object
                    .get("cli")
                    .is_none_or(|cli| cli == &Value::Bool(false))
                {
                    object.insert("cli".into(), Value::Bool(true));
                }
            }
            continue;
        }
        let mut options = json!({"out": format!("generated/{name}")});
        if target == "cli" {
            options["cli"] = json!(true);
        }
        targets.insert(name.to_string(), options);
    }
    link_adapters(&args.dir, targets, &before)?;
    if tungsten_config::parse_str(
        "tungsten.yml",
        &serde_json::to_string(&value).unwrap_or_default(),
    )
    .0
    .is_none()
    {
        return Err(
            "updated manifest does not validate; existing configuration is unchanged".into(),
        );
    }
    let targets = value
        .get("targets")
        .and_then(Value::as_object)
        .ok_or("existing project has no target mapping")?;
    let (edited, warning) = edit_targets(&original, &source, &before, targets)?;
    // The edit must mean exactly the computed manifest, or nothing is written.
    if tungsten_config::parse_yaml("tungsten.yml", &edited)
        .0
        .ok()
        .as_ref()
        != Some(&value)
    {
        return Err("cannot edit tungsten.yml in place; existing configuration is unchanged. Add the outputs under `targets:` by hand".into());
    }
    let backup = replace(&args.dir, &manifest, &edited)?;
    Ok(Added { backup, warning })
}

/// New `mcp` and `webmcp` targets use the TypeScript SDK's output and, like a
/// fresh init, its local runtime (`sdk_runtime_path`), so the adapter and the
/// SDK share one runtime copy. Targets that existed before are never changed.
fn link_adapters(
    dir: &Path,
    targets: &mut Map<String, Value>,
    before: &Map<String, Value>,
) -> Result<(), String> {
    let typescript = targets.get("typescript");
    let Some(sdk) = typescript
        .and_then(|v| v.get("out"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let sdk = std::path::absolute(dir.join(sdk)).map_err(|_| "cannot resolve TypeScript output")?;
    let runtime = typescript
        .and_then(|v| v.get("runtime_path"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    for target in ["mcp", "webmcp"] {
        if before.contains_key(target) {
            continue;
        }
        let Some(options) = targets.get_mut(target) else {
            continue;
        };
        let out = options
            .get("out")
            .and_then(Value::as_str)
            .ok_or("missing output directory")?;
        let out =
            std::path::absolute(dir.join(out)).map_err(|_| "cannot resolve adapter output")?;
        let relative = super::init::relative_path(&sdk, &out)
            .ok_or("SDK and adapter outputs must share a filesystem root")?;
        options["sdk_path"] = json!(slashes(&relative));
        if let Some(runtime) = &runtime {
            // `runtime_path` is relative to the TypeScript output; rebase it
            // onto the adapter's output unless it is absolute.
            let path = Path::new(runtime);
            let rebased = if path.is_absolute() {
                runtime.clone()
            } else {
                let resolved = normalize(&sdk.join(path));
                let relative = super::init::relative_path(&resolved, &normalize(&out))
                    .ok_or("SDK runtime and adapter outputs must share a filesystem root")?;
                slashes(&relative)
            };
            options["sdk_runtime_path"] = json!(rebased);
        }
    }
    Ok(())
}

fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// `path` with `.` and `name/..` removed lexically.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// One line of the manifest: its byte range (with the line break) and
/// whether it holds YAML content (not blank, not only a comment).
struct Line {
    start: usize,
    end: usize,
    indent: usize,
    content: bool,
}

fn lines(text: &str) -> Vec<Line> {
    let mut out = vec![];
    let mut start = 0;
    for piece in text.split_inclusive('\n') {
        let body = piece.trim_end_matches(['\n', '\r']);
        let trimmed = body.trim_start_matches(' ');
        out.push(Line {
            start,
            end: start + piece.len(),
            indent: body.len() - trimmed.len(),
            content: !trimmed.trim().is_empty() && !trimmed.starts_with('#'),
        });
        start += piece.len();
    }
    out
}

/// The manifest text with the new and changed entries of `targets`. Each
/// such entry is one flow-style line; every other line is kept. When the
/// `targets` mapping is not in block style, the section is rewritten and the
/// warning says whether comments inside it were dropped.
fn edit_targets(
    original: &str,
    source: &tungsten_config::ManifestSource,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) -> Result<(String, Option<String>), String> {
    let newline = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let entry = |indent: usize, name: &str, value: &Value| {
        format!(
            "{}{}: {}{newline}",
            " ".repeat(indent),
            super::init::yaml_scalar(name),
            value
        )
    };
    let all = lines(original);
    let at = |offset: usize| all.iter().position(|l| l.start <= offset && offset < l.end);
    let key = source
        .offset("/targets")
        .and_then(|o| at(o as usize))
        .ok_or("cannot locate `targets` in tungsten.yml")?;
    let base = all[key].indent;
    // The section: every line after the key up to the next line with content
    // at the key's indentation or less; it ends after its last content line.
    let mut last = key;
    for (i, line) in all.iter().enumerate().skip(key + 1) {
        if !line.content {
            continue;
        }
        if line.indent <= base {
            break;
        }
        last = i;
    }
    let section_end = all[last].end;
    let mut children: Vec<(usize, &str)> = vec![];
    for name in before.keys() {
        let pointer = tungsten_config::pointer::child("/targets", name);
        let line = source
            .offset(&pointer)
            .and_then(|o| at(o as usize))
            .ok_or("cannot locate a target in tungsten.yml")?;
        children.push((line, name));
    }
    children.sort_unstable();
    let block = !children.is_empty() && children.iter().all(|(line, _)| *line > key);
    let mut text = String::new();
    if !block {
        let removed = &original[all[key].start..section_end];
        text.push_str(&original[..all[key].start]);
        text.push_str(&" ".repeat(base));
        text.push_str("targets:");
        text.push_str(newline);
        for (name, value) in after {
            text.push_str(&entry(base + 2, name, value));
        }
        text.push_str(&original[section_end..]);
        let warning = removed.contains('#').then(|| {
            "tungsten.yml `targets` is not a block mapping, so it was rewritten; comments inside it were dropped (the rest of the file, its comments and header are unchanged)".to_owned()
        });
        return Ok((text, warning));
    }
    let indent = all[children[0].0].indent;
    let mut warning = None;
    let mut cursor = 0;
    for (index, (line, name)) in children.iter().enumerate() {
        let new = after.get(*name).ok_or("an existing target disappeared")?;
        if before.get(*name) == Some(new) {
            continue;
        }
        // Replace this entry's lines up to its last content line.
        let next = children.get(index + 1).map_or(last + 1, |(l, _)| *l);
        let end_line = (*line..next)
            .rev()
            .find(|&i| all[i].content)
            .unwrap_or(*line);
        let (start, end) = (all[*line].start, all[end_line].end);
        if original[start..end].contains('#') {
            warning = Some(format!(
                "comments inside `targets.{name}` of tungsten.yml were dropped when it was rewritten; the rest of the file is unchanged"
            ));
        }
        text.push_str(&original[cursor..start]);
        text.push_str(&entry(indent, name, new));
        cursor = end;
    }
    text.push_str(&original[cursor..section_end]);
    for (name, value) in after {
        if !before.contains_key(name) {
            if !text.ends_with('\n') {
                text.push_str(newline);
            }
            text.push_str(&entry(indent, name, value));
        }
    }
    text.push_str(&original[section_end..]);
    Ok((text, warning))
}

/// Back up `manifest` under a fresh name, write `text` to a pending file and
/// move it over the manifest. Returns the backup's path. On failure the
/// manifest is unchanged and the pending file is removed.
fn replace(dir: &Path, manifest: &Path, text: &str) -> Result<PathBuf, String> {
    // A hard link never replaces an existing backup and keeps the exact bytes.
    let mut backup = None;
    for n in 0..MAX_BACKUPS {
        let name = if n == 0 {
            "tungsten.yml.init-backup".to_owned()
        } else {
            format!("tungsten.yml.init-backup.{n}")
        };
        let path = dir.join(name);
        match std::fs::hard_link(manifest, &path) {
            Ok(()) => {
                backup = Some(path);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err("cannot create a manifest backup; tungsten.yml is unchanged".into());
            }
        }
    }
    let backup = backup.ok_or(
        "too many tungsten.yml.init-backup files; remove old backups and retry (tungsten.yml is unchanged)",
    )?;
    let pending = dir.join("tungsten.yml.init-pending");
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&pending);
    let Ok(mut file) = file else {
        let _ = std::fs::remove_file(&backup);
        return Err("tungsten.yml.init-pending already exists (another init may be running, or one was interrupted); remove it and retry. tungsten.yml is unchanged".into());
    };
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if written.is_err() {
        let _ = std::fs::remove_file(&pending);
        let _ = std::fs::remove_file(&backup);
        return Err("cannot write the updated manifest; tungsten.yml is unchanged".into());
    }
    // Rename replaces the manifest atomically; the backup keeps the old bytes.
    if std::fs::rename(&pending, manifest).is_err() {
        let _ = std::fs::remove_file(&pending);
        let _ = std::fs::remove_file(&backup);
        return Err("cannot replace tungsten.yml; it is unchanged".into());
    }
    Ok(backup)
}
