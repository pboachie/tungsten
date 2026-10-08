// SPDX-License-Identifier: Apache-2.0
//! The kit's own commands: `schema`, `operations` and `auth status`.

use std::collections::BTreeMap;

use clap::ArgMatches;
use serde_json::{Value, json};

use crate::config::{Found, Settings, Source};
use crate::ctx::{Ctx, Fail};
use crate::exit;
use crate::render::{json_doc, safety_name};
use crate::tree::Leaf;

fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// `schema <command...>` or `schema <operation id>`.
pub(crate) fn schema(
    ctx: &mut Ctx<'_>,
    leaves: &BTreeMap<Vec<String>, Leaf>,
    m: &ArgMatches,
) -> Result<u8, Fail> {
    let path: Vec<String> = m
        .get_many::<String>("path")
        .map(|v| v.cloned().collect())
        .unwrap_or_default();
    let spec = ctx.spec;
    let by_id = || {
        let [id] = path.as_slice() else { return None };
        spec.ops
            .iter()
            .find(|o| o.id == *id)
            .map(|o| &o.schema)
            .or_else(|| {
                spec.macros
                    .iter()
                    .find(|x| x.name == *id)
                    .map(|x| &x.schema)
            })
    };
    let found = match leaves.get(&path) {
        Some(Leaf::Op(i)) => Some(&spec.ops[*i].schema),
        Some(Leaf::Macro(i)) => Some(&spec.macros[*i].schema),
        None => by_id(),
    };
    let Some(schema) = found else {
        let wanted = path.join(" ");
        let near: Vec<String> = leaves
            .keys()
            .filter(|k| k.first() == path.first())
            .take(8)
            .map(|k| format!("  {} {}", spec.bin, k.join(" ")))
            .collect();
        let hint = if near.is_empty() {
            format!("run `{} operations` to list the commands", spec.bin)
        } else {
            format!("commands that start alike:\n{}", near.join("\n"))
        };
        return Err(Fail::Usage(format!(
            "there is no command `{wanted}`; {hint}"
        )));
    };
    let pretty = m.get_flag("pretty");
    let text = json_doc(schema, pretty);
    ctx.out(&text);
    Ok(exit::OK)
}

/// `operations`: every command with its tier and summary.
pub(crate) fn operations(ctx: &mut Ctx<'_>) -> u8 {
    let spec = ctx.spec;
    let mut rows: Vec<(String, String, &'static str, String, bool)> = Vec::new();
    for o in &spec.ops {
        rows.push((
            "operation".into(),
            o.id.clone(),
            safety_name(o.safety),
            first_line(&o.about).to_string(),
            o.paginated,
        ));
    }
    let ops_len = rows.len();
    for x in &spec.macros {
        rows.push((
            "macro".into(),
            x.name.clone(),
            safety_name(x.safety),
            first_line(&x.about).to_string(),
            false,
        ));
    }
    let paths: Vec<String> = spec
        .ops
        .iter()
        .map(|o| o.path.join(" "))
        .chain(spec.macros.iter().map(|x| x.path.join(" ")))
        .collect();
    if ctx.json {
        let list: Vec<Value> = rows
            .iter()
            .zip(&paths)
            .map(|((kind, id, safety, summary, paginated), path)| {
                json!({
                    "kind": kind,
                    "id": id,
                    "command": path,
                    "safety": safety,
                    "summary": summary,
                    "paginated": paginated,
                })
            })
            .collect();
        let text = json_doc(&Value::Array(list), ctx.pretty);
        ctx.out(&text);
        return exit::OK;
    }
    let width = paths.iter().map(String::len).max().unwrap_or(0);
    let tier = rows.iter().map(|r| r.2.len()).max().unwrap_or(0);
    for (i, ((_, _, safety, summary, _), path)) in rows.iter().zip(&paths).enumerate() {
        let tag = if i < ops_len { "" } else { " (macro)" };
        ctx.out(&format!("{path:<width$}  {safety:<tier$}  {summary}{tag}"));
    }
    exit::OK
}

/// `auth status`: the credentials that are present, never their values.
pub(crate) fn auth_status(
    ctx: &mut Ctx<'_>,
    settings: &Settings,
    found: &[Found],
    default_url: Option<&str>,
    unknown: &[String],
) -> u8 {
    let url = match (&settings.base_url, default_url) {
        (Some((v, s)), _) => (Some(v.as_str()), *s),
        (None, d) => (d, Source::Default),
    };
    if ctx.json {
        let creds: Vec<Value> = found
            .iter()
            .map(|f| {
                json!({
                    "scheme": f.slot.scheme,
                    "part": f.slot.part,
                    "env": f.slot.env,
                    "profile_key": f.slot.profile_key,
                    "present": f.source.is_some(),
                    "source": f.source.map(Source::name),
                })
            })
            .collect();
        let doc = json!({
            "profile": settings.profile,
            "config_file": settings.config_path.as_ref().map(|p| p.display().to_string()),
            "base_url": { "value": url.0, "source": url.1.name() },
            "credentials": creds,
            "unrecognized_profile_keys": unknown,
        });
        let text = json_doc(&doc, ctx.pretty);
        ctx.out(&text);
        return exit::OK;
    }
    let config = settings
        .config_path
        .as_ref()
        .map_or("none".to_string(), |p| p.display().to_string());
    ctx.out(&format!("profile    {}", settings.profile));
    ctx.out(&format!("config     {config}"));
    ctx.out(&format!(
        "base url   {} ({})",
        url.0.unwrap_or("not set"),
        url.1.name()
    ));
    for f in found {
        let state = match f.source {
            Some(s) => format!("present ({})", s.name()),
            None => "missing".to_string(),
        };
        let part = f
            .slot
            .part
            .as_deref()
            .map_or(String::new(), |p| format!(" [{p}]"));
        ctx.out(&format!(
            "{}{part}: {state}; env {}, profile key {}",
            f.slot.scheme, f.slot.env, f.slot.profile_key
        ));
    }
    for k in unknown {
        ctx.out(&format!("unrecognized key in the profile: {k}"));
    }
    exit::OK
}
