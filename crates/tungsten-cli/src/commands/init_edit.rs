// SPDX-License-Identifier: AGPL-3.0-only
//! Additive project edits with an exclusive backup and recoverable replacement.
use crate::args::InitArgs;
use serde_json::{Value, json};
use std::io::Write;

pub(super) fn add_outputs(args: &InitArgs) -> Result<(), String> {
    let manifest = args.dir.join("tungsten.yml");
    let original =
        std::fs::read_to_string(&manifest).map_err(|_| "cannot read project manifest")?;
    let mut value = tungsten_config::parse_yaml("tungsten.yml", &original)
        .0
        .map_err(|_| "cannot parse existing project manifest")?;
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
    let sdk = targets
        .get("typescript")
        .and_then(|v| v.get("out"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(sdk) = sdk {
        let sdk = std::path::absolute(args.dir.join(sdk))
            .map_err(|_| "cannot resolve TypeScript output")?;
        for target in ["mcp", "webmcp"] {
            // Link only new targets; never overwrite a reviewed runtime/SDK choice.
            if !selected.iter().any(|s| s == target)
                || tungsten_config::parse_yaml("original", &original)
                    .0
                    .ok()
                    .and_then(|v| v.get("targets").and_then(|t| t.get(target)).cloned())
                    .is_some()
            {
                continue;
            }
            let options = targets.get_mut(target).ok_or("missing transport target")?;
            let out = options
                .get("out")
                .and_then(Value::as_str)
                .ok_or("missing output directory")?;
            let out = std::path::absolute(args.dir.join(out))
                .map_err(|_| "cannot resolve adapter output")?;
            let relative = super::init::relative_path(&sdk, &out)
                .ok_or("SDK and adapter outputs must share a filesystem root")?;
            options["sdk_path"] = json!(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    let encoded =
        serde_json::to_string_pretty(&value).map_err(|_| "cannot encode updated manifest")?;
    if tungsten_config::parse_str("tungsten.yml", &encoded)
        .0
        .is_none()
    {
        return Err(
            "updated manifest does not validate; existing configuration is unchanged".into(),
        );
    }
    let backup = args.dir.join("tungsten.yml.init-backup");
    let pending = args.dir.join("tungsten.yml.init-pending");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&pending)
        .map_err(|_| "pending manifest already exists or cannot be written")?;
    file.write_all(encoded.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|_| "cannot write pending manifest; original is unchanged")?;
    drop(file);
    // Hard-link creation never replaces an existing backup and retains exact bytes.
    std::fs::hard_link(&manifest, &backup)
        .map_err(|_| "cannot create exclusive manifest backup; original is unchanged")?;
    std::fs::remove_file(&manifest)
        .map_err(|_| "cannot replace manifest; original and backup are preserved")?;
    std::fs::rename(&pending, &manifest).map_err(
        |_| "cannot install pending manifest; restore tungsten.yml.init-backup before retrying",
    )?;
    Ok(())
}
