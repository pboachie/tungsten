// SPDX-License-Identifier: AGPL-3.0-only
//! `src/table.rs` of the CLI crate: the `CliSpec` of the API as Rust source.
//!
//! The file is regular on purpose (one helper call per flag, one struct per
//! command) so it reads as a table; it is marked `rustfmt::skip` by
//! `main.rs`.

use tungsten_ir::Safety;

use crate::cli::kinds::Kind;
use crate::cli::model::{Flag, MacroCommand, Model, OpCommand};

/// A Rust string literal.
pub(crate) fn rs_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A raw string literal holding `s` exactly.
fn raw_str(s: &str) -> String {
    let mut hashes = 1;
    while s.contains(&format!("\"{}", "#".repeat(hashes))) {
        hashes += 1;
    }
    let h = "#".repeat(hashes);
    format!("r{h}\"{s}\"{h}")
}

fn safety(s: Safety) -> &'static str {
    match s {
        Safety::ReadOnly => "Safety::ReadOnly",
        Safety::Mutating => "Safety::Mutating",
        Safety::Destructive => "Safety::Destructive",
        Safety::Irreversible => "Safety::Irreversible",
    }
}

fn kind(k: &Kind) -> String {
    match k {
        Kind::String => "FlagKind::String".into(),
        Kind::Integer => "FlagKind::Integer".into(),
        Kind::Number => "FlagKind::Number".into(),
        Kind::Boolean => "FlagKind::Boolean".into(),
        Kind::Json => "FlagKind::Json".into(),
        Kind::File => "FlagKind::File".into(),
        Kind::Enum(values) => {
            let list: Vec<String> = values.iter().map(|v| rs_str(v)).collect();
            format!("FlagKind::Enum(strings(&[{}]))", list.join(", "))
        }
        Kind::Array(inner) => format!("FlagKind::Array(Box::new({}))", kind(inner)),
    }
}

fn uses_enum(k: &Kind) -> bool {
    match k {
        Kind::Enum(_) => true,
        Kind::Array(inner) => uses_enum(inner),
        _ => false,
    }
}

/// `name(arg, arg, ...)` on one line, or one argument per line when it
/// would pass 100 columns.
fn call(indent: usize, name: &str, args: &[String]) -> String {
    let pad = " ".repeat(indent);
    let one = format!("{pad}{name}({}),", args.join(", "));
    if one.chars().count() <= 100 {
        return one;
    }
    let inner = " ".repeat(indent + 4);
    let mut out = format!("{pad}{name}(\n");
    for a in args {
        out.push_str(&format!("{inner}{a},\n"));
    }
    out.push_str(&format!("{pad}),"));
    out
}

fn flag_call(f: &Flag, indent: usize) -> String {
    call(
        indent,
        if f.sensitive { "secret" } else { "flag" },
        &[
            rs_str(&f.flag),
            rs_str(&f.arg),
            kind(&f.kind),
            f.required.to_string(),
            rs_str(&f.help),
        ],
    )
}

fn flags(list: &[Flag], indent: usize) -> String {
    let pad = " ".repeat(indent);
    if list.is_empty() {
        return format!("{pad}flags: vec![],\n");
    }
    let mut out = format!("{pad}flags: vec![\n");
    for f in list {
        out.push_str(&flag_call(f, indent + 4));
        out.push('\n');
    }
    out.push_str(&format!("{pad}],\n"));
    out
}

fn path(p: &[String]) -> String {
    let list: Vec<String> = p.iter().map(|s| rs_str(s)).collect();
    format!("path(&[{}])", list.join(", "))
}

/// One `name: value,` line of a struct literal at 16 spaces.
fn field(name: &str, value: &str) -> String {
    format!("                {name}: {value},\n")
}

fn op(o: &OpCommand) -> String {
    let mut s = String::from("            CliOp {\n");
    s.push_str(&field("id", &format!("{}.into()", rs_str(&o.id))));
    s.push_str(&field("path", &path(&o.path)));
    s.push_str(&field("about", &format!("{}.into()", rs_str(&o.about))));
    s.push_str(&field("safety", safety(o.safety)));
    s.push_str(&flags(&o.flags, 16));
    let body = match &o.body_arg {
        Some(b) => format!("Some({}.into())", rs_str(b)),
        None => "None".to_string(),
    };
    s.push_str(&field("body_arg", &body));
    s.push_str(&field("paginated", &o.paginated.to_string()));
    s.push_str(&field("schema", &schema(&o.schema)));
    s.push_str("            },\n");
    s
}

fn mac(m: &MacroCommand) -> String {
    let mut s = String::from("            CliMacro {\n");
    s.push_str(&field("name", &format!("{}.into()", rs_str(&m.name))));
    s.push_str(&field("path", &path(&m.path)));
    s.push_str(&field("about", &format!("{}.into()", rs_str(&m.about))));
    s.push_str(&field("safety", safety(m.safety)));
    s.push_str(&flags(&m.flags, 16));
    s.push_str(&field("schema", &schema(&m.schema)));
    s.push_str("            },\n");
    s
}

fn schema(v: &serde_json::Value) -> String {
    format!("schema({})", raw_str(&v.to_string()))
}

/// What the table's source needs besides the spec itself.
pub(crate) struct Meta<'a> {
    pub header: &'a str,
    pub bin: &'a str,
    pub about: &'a str,
    pub version: &'a str,
    pub env_prefix: &'a str,
    pub config_dir: &'a str,
    /// `(scheme, variable)` of the schemes whose profile names a variable.
    pub credential_env: &'a [(String, String)],
}

pub(crate) fn source(model: &Model, meta: &Meta<'_>) -> String {
    let all_flags: Vec<&Flag> = model
        .ops
        .iter()
        .flat_map(|o| &o.flags)
        .chain(model.macros.iter().flat_map(|m| &m.flags))
        .collect();
    let has_flags = !all_flags.is_empty();
    let has_secret = all_flags.iter().any(|f| f.sensitive);
    let has_enum = all_flags.iter().any(|f| uses_enum(&f.kind));
    let has_commands = !model.ops.is_empty() || !model.macros.is_empty();

    let mut kit: Vec<&str> = vec![];
    if has_flags {
        kit.push("CliFlag");
    }
    if !model.macros.is_empty() {
        kit.push("CliMacro");
    }
    if !model.ops.is_empty() {
        kit.push("CliOp");
    }
    kit.push("CliSpec");
    if has_flags {
        kit.push("FlagKind");
    }

    let mut s = String::new();
    s.push_str(meta.header);
    s.push_str("\n//! The command table of the CLI: one command per operation and per macro of\n");
    s.push_str(
        "//! the API. The kit builds the command tree, parses the flags and runs the SDK.\n\n",
    );
    if has_commands {
        s.push_str("use serde_json::Value;\n");
    }
    if let [one] = kit.as_slice() {
        s.push_str(&format!("use tungsten_cli_kit::{one};\n"));
    } else {
        s.push_str(&format!("use tungsten_cli_kit::{{{}}};\n", kit.join(", ")));
    }
    if has_commands {
        s.push_str("use tungsten_runtime::Safety;\n");
        s.push_str("\nfn schema(text: &str) -> Value {\n    serde_json::from_str(text).unwrap_or(Value::Null)\n}\n");
        s.push_str("\nfn path(segments: &[&str]) -> Vec<String> {\n    segments.iter().map(|s| (*s).to_string()).collect()\n}\n");
    }
    if has_enum {
        s.push_str("\nfn strings(values: &[&str]) -> Vec<String> {\n    values.iter().map(|s| (*s).to_string()).collect()\n}\n");
    }
    if has_flags {
        s.push_str(
            "\nfn flag(name: &str, arg: &str, kind: FlagKind, required: bool, help: &str) -> CliFlag {\n    CliFlag {\n        flag: name.to_string(),\n        arg: arg.to_string(),\n        kind,\n        required,\n        help: help.to_string(),\n        sensitive: false,\n    }\n}\n",
        );
    }
    if has_secret {
        s.push_str(
            "\n/// A flag whose value is never given on the command line.\nfn secret(name: &str, arg: &str, kind: FlagKind, required: bool, help: &str) -> CliFlag {\n    CliFlag {\n        sensitive: true,\n        ..flag(name, arg, kind, required, help)\n    }\n}\n",
        );
    }
    s.push_str("\npub(crate) fn spec() -> CliSpec {\n    CliSpec {\n");
    s.push_str(&format!("        bin: {}.into(),\n", rs_str(meta.bin)));
    s.push_str(&format!("        about: {}.into(),\n", rs_str(meta.about)));
    s.push_str(&format!(
        "        version: {}.into(),\n",
        rs_str(meta.version)
    ));
    s.push_str(&format!(
        "        env_prefix: {}.into(),\n",
        rs_str(meta.env_prefix)
    ));
    s.push_str(&format!(
        "        config_dir: {}.into(),\n",
        rs_str(meta.config_dir)
    ));
    if meta.credential_env.is_empty() {
        s.push_str("        credential_env: vec![],\n");
    } else {
        s.push_str("        credential_env: vec![\n");
        for (scheme, variable) in meta.credential_env {
            s.push_str(&format!(
                "            ({}.into(), {}.into()),\n",
                rs_str(scheme),
                rs_str(variable)
            ));
        }
        s.push_str("        ],\n");
    }
    if model.ops.is_empty() {
        s.push_str("        ops: vec![],\n");
    } else {
        s.push_str("        ops: vec![\n");
        for o in &model.ops {
            s.push_str(&op(o));
        }
        s.push_str("        ],\n");
    }
    if model.macros.is_empty() {
        s.push_str("        macros: vec![],\n");
    } else {
        s.push_str("        macros: vec![\n");
        for m in &model.macros {
            s.push_str(&mac(m));
        }
        s.push_str("        ],\n");
    }
    s.push_str("    }\n}\n");
    s
}
