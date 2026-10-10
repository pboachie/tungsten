// SPDX-License-Identifier: AGPL-3.0-only
//! The command table of an API: one command per callable operation and per
//! macro, decided once from the IR.
//!
//! Command paths mirror the resource tree in kebab-case (a multi-namespace
//! API starts every path with its namespace, as the SDK clients do; macros
//! live under `macros`). Flags come from the arguments object of
//! `tungsten_emit::args`: a parameter is a flag, a merged body field is a
//! flag, a whole body is `--body`; their argument names are the Rust field
//! names of the SDK's request structs (`naming::render(Target::Rust,
//! Role::Field)`, made unique over the parameters and the merged body
//! fields), and the flag is the kebab-case of the same words.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::args::{BodyArg, args_layout};
use tungsten_emit::compact::{callable_operation, macro_parameters, operation_parameters};
use tungsten_ir::naming::{self, Case, Role, Target};
use tungsten_ir::{
    BodyEncoding, Doc, Ident, Ir, Operation, OperationStatus, Presence, Remediation, Resource,
    Retryable, Safety,
};

use crate::cli::kinds::{self, Kind};

/// Flags the kit adds to every command (`tungsten_cli_kit::RESERVED_FLAGS`).
pub(crate) const RESERVED_FLAGS: &[&str] = &[
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

/// First path segments of the kit's own commands
/// (`tungsten_cli_kit::RESERVED_COMMANDS`).
pub(crate) const RESERVED_COMMANDS: &[&str] =
    &["auth", "explain-error", "help", "operations", "schema"];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Flag {
    pub flag: String,
    pub arg: String,
    pub kind: Kind,
    pub required: bool,
    pub help: String,
    pub sensitive: bool,
}

/// One remediation entry (`explain-error`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Remedy {
    pub code: String,
    pub category: Option<String>,
    pub text: Option<String>,
    pub retryable: Option<&'static str>,
    pub next_action: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct OpCommand {
    pub id: String,
    pub path: Vec<String>,
    pub about: String,
    pub safety: Safety,
    pub flags: Vec<Flag>,
    pub body_arg: Option<String>,
    pub paginated: bool,
    pub schema: Value,
    pub remediation: Vec<Remedy>,
    pub remediation_note: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct MacroCommand {
    pub name: String,
    pub path: Vec<String>,
    pub about: String,
    pub safety: Safety,
    pub flags: Vec<Flag>,
    pub schema: Value,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Model {
    pub ops: Vec<OpCommand>,
    pub macros: Vec<MacroCommand>,
    pub error_codes: Vec<Remedy>,
}

fn retryable_name(r: Retryable) -> &'static str {
    match r {
        Retryable::Never => "never",
        Retryable::AfterDelay => "after_delay",
        Retryable::SameKeyOnly => "same_key_only",
        Retryable::AfterRemediation => "after_remediation",
    }
}

/// A remediation table as rows, sorted by code.
fn remedies(table: &BTreeMap<String, Remediation>) -> Vec<Remedy> {
    table
        .iter()
        .map(|(code, r)| Remedy {
            code: code.clone(),
            category: r.category.clone(),
            text: r.text.clone(),
            retryable: r.retryable.map(retryable_name),
            next_action: r.next_action.clone(),
        })
        .collect()
}

fn kebab(words: &[String]) -> String {
    naming::to_case(words, Case::Kebab)
}

fn ident(words: &[String]) -> Ident {
    Ident {
        wire: words.join(" "),
        words: words.to_vec(),
    }
}

/// The word lists of `entries`, made unique within one scope.
fn unique_words(entries: Vec<Vec<String>>, role: Role) -> Vec<Vec<String>> {
    let mut idents: Vec<Ident> = entries.iter().map(|w| ident(w)).collect();
    naming::disambiguate(&mut idents, Target::Rust, role);
    idents.into_iter().map(|i| i.words).collect()
}

fn callable(op: &Operation) -> bool {
    !matches!(op.status, OperationStatus::Planned { .. })
}

/// The first sentence of a documentation text, on one line.
pub(crate) fn sentence(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = flat
        .char_indices()
        .find(|(i, c)| {
            matches!(c, '.' | '!' | '?')
                && flat[i + c.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
        .map_or(flat.len(), |(i, c)| i + c.len_utf8());
    let mut s = flat[..end].to_string();
    if s.chars().count() > 120 {
        s = s.chars().take(117).collect::<String>() + "...";
    }
    s
}

fn doc_line(doc: Option<&Doc>) -> String {
    doc.and_then(|d| d.summary.as_deref().or(d.description.as_deref()))
        .map(sentence)
        .unwrap_or_default()
}

/// Operations with their command paths, in IR order.
fn op_paths(ir: &Ir) -> Vec<(&Operation, Vec<String>)> {
    fn walk<'a>(
        resources: &'a [Resource],
        names: &[Vec<String>],
        prefix: &[String],
        out: &mut Vec<(&'a Operation, Vec<String>)>,
    ) {
        for (r, name) in resources.iter().zip(names) {
            let mut path = prefix.to_vec();
            path.push(kebab(name));
            let ops: Vec<&Operation> = r.operations.iter().filter(|o| callable(o)).collect();
            let mut words: Vec<Vec<String>> = ops.iter().map(|o| o.name.words.clone()).collect();
            words.extend(r.children.iter().map(|c| c.name.words.clone()));
            let unique = unique_words(words, Role::Module);
            for (op, w) in ops.iter().zip(&unique) {
                let mut p = path.clone();
                p.push(kebab(w));
                out.push((op, p));
            }
            walk(&r.children, &unique[ops.len()..], &path, out);
        }
    }
    let multi = ir.namespaces.len() > 1;
    let ns_names = unique_words(
        ir.namespaces.iter().map(|n| n.name.words.clone()).collect(),
        Role::Module,
    );
    let mut out = vec![];
    for (ns, ns_name) in ir.namespaces.iter().zip(&ns_names) {
        let names = unique_words(
            ns.resources.iter().map(|r| r.name.words.clone()).collect(),
            Role::Module,
        );
        let prefix = if multi { vec![kebab(ns_name)] } else { vec![] };
        walk(&ns.resources, &names, &prefix, &mut out);
    }
    out
}

fn add_flag(
    flags: &mut Vec<Flag>,
    used: &mut BTreeSet<String>,
    diags: &mut Diagnostics,
    at: &str,
    mut f: Flag,
) {
    if RESERVED_FLAGS.contains(&f.flag.as_str()) || used.contains(&f.flag) {
        let original = f.flag.clone();
        let mut candidate = format!("{original}-arg");
        let mut n = 2;
        while RESERVED_FLAGS.contains(&candidate.as_str()) || used.contains(&candidate) {
            candidate = format!("{original}-arg-{n}");
            n += 1;
        }
        diags.push(
            Diagnostic::info(
                "TG0752",
                format!(
                    "the flag --{original} of {at} is named --{candidate} in the CLI: --{original} is taken"
                ),
            )
            .with_help("The kit's own flags (--json, --body, --yes, --dry-run, ...) keep their names."),
        );
        f.flag = candidate;
    }
    used.insert(f.flag.clone());
    flags.push(f);
}

struct Built {
    flags: Vec<Flag>,
    body_arg: Option<String>,
    body_required: bool,
}

fn op_flags(ir: &Ir, op: &Operation, diags: &mut Diagnostics) -> Built {
    let layout = args_layout(ir, op);
    let mut entries: Vec<Vec<String>> = layout
        .params
        .iter()
        .map(|a| a.param.name.words.clone())
        .collect();
    let merged = match &layout.body {
        Some(BodyArg::Merged { fields, .. }) => fields.clone(),
        _ => vec![],
    };
    entries.extend(merged.iter().map(|f| f.name.words.clone()));
    let has_arg_body = matches!(layout.body, Some(BodyArg::Arg { .. }));
    if has_arg_body {
        entries.push(vec!["body".to_string()]);
    }
    let mut idents: Vec<Ident> = entries.iter().map(|w| ident(w)).collect();
    naming::disambiguate(&mut idents, Target::Rust, Role::Field);
    let name_of = |i: usize| {
        (
            naming::render(&idents[i], Target::Rust, Role::Field),
            kebab(&idents[i].words),
        )
    };

    let mut flags = Vec::new();
    let mut used = BTreeSet::new();
    let id = op.id.0.as_str();
    for (i, a) in layout.params.iter().enumerate() {
        let (arg, flag) = name_of(i);
        let kind = kinds::flag_kind(ir, &a.param.ty).unwrap_or_else(|| {
            diags.push(
                Diagnostic::info(
                    "TG0750",
                    format!(
                        "the {} parameter `{}` of {id} is {}; its flag --{flag} takes JSON text or its members as --{flag}.NAME VALUE",
                        a.location.as_str(),
                        a.param.wire_name,
                        kinds::describe(ir, &a.param.ty)
                    ),
                )
                .with_help("Pass the value as inline JSON, @file.json or - for standard input, or member by member (--flag.NAME VALUE)."),
            );
            Kind::Json
        });
        add_flag(
            &mut flags,
            &mut used,
            diags,
            id,
            Flag {
                flag,
                arg,
                kind,
                required: a.param.required,
                help: doc_line(a.param.doc.as_ref()),
                sensitive: kinds::is_password(ir, &a.param.ty),
            },
        );
    }
    for (j, f) in merged.iter().enumerate() {
        let (arg, flag) = name_of(layout.params.len() + j);
        add_flag(
            &mut flags,
            &mut used,
            diags,
            id,
            Flag {
                flag,
                arg,
                kind: kinds::flag_kind(ir, &f.ty).unwrap_or(Kind::Json),
                required: matches!(f.presence, Presence::Required | Presence::RequiredNullable),
                help: doc_line(f.doc.as_ref()),
                sensitive: f.sensitive || kinds::is_password(ir, &f.ty),
            },
        );
    }
    let body_arg = match &layout.body {
        Some(BodyArg::Arg { content, .. }) => {
            if content.encoding == BodyEncoding::Multipart {
                diags.push(
                    Diagnostic::info(
                        "TG0750",
                        format!(
                            "the multipart body of {id} is given with --body as a JSON object; a part that is a file cannot be passed from the command line"
                        ),
                    )
                    .with_help("Use the SDK for multipart uploads."),
                );
            }
            Some(name_of(idents.len() - 1).0)
        }
        _ => None,
    };
    Built {
        flags,
        body_arg,
        body_required: layout.body_required,
    }
}

fn op_about(op: &Operation) -> String {
    let summary = if op.agent.compact_doc.trim().is_empty() {
        let line = doc_line(op.doc.as_ref());
        if line.is_empty() {
            format!("{} {}", method(op), op.path.raw)
        } else {
            line
        }
    } else {
        sentence(&op.agent.compact_doc)
    };
    format!("{summary}\n\n{} {}", method(op), op.path.raw)
}

fn method(op: &Operation) -> &'static str {
    use tungsten_ir::HttpMethod::*;
    match op.method {
        Get => "GET",
        Put => "PUT",
        Post => "POST",
        Delete => "DELETE",
        Options => "OPTIONS",
        Head => "HEAD",
        Patch => "PATCH",
        Trace => "TRACE",
    }
}

fn macro_flags(
    ir: &Ir,
    m: &tungsten_ir::Macro,
    ops: &[OpCommand],
    diags: &mut Diagnostics,
    body_required: &dyn Fn(&str) -> bool,
) -> Vec<Flag> {
    let mut flags = Vec::new();
    let mut used = BTreeSet::new();
    let name = m.name.0.as_str();
    let base = m
        .input
        .get("extends")
        .and_then(Value::as_str)
        .and_then(|id| callable_operation(ir, id))
        .and_then(|op| ops.iter().find(|c| c.id == op.id.0));
    if let Some(op) = base {
        for f in &op.flags {
            add_flag(&mut flags, &mut used, diags, name, f.clone());
        }
        if let Some(arg) = &op.body_arg {
            add_flag(
                &mut flags,
                &mut used,
                diags,
                name,
                Flag {
                    flag: "body".into(),
                    arg: arg.clone(),
                    kind: Kind::Json,
                    required: body_required(&op.id),
                    help: "The request body as JSON.".into(),
                    sensitive: false,
                },
            );
        }
    }
    if let Some(Value::Object(add)) = m.input.get("add") {
        for (input, schema) in add {
            if flags.iter().any(|f| f.arg == *input) {
                continue;
            }
            let words = naming::split_words(input);
            add_flag(
                &mut flags,
                &mut used,
                diags,
                name,
                Flag {
                    flag: kebab(&words),
                    arg: input.clone(),
                    kind: kinds::schema_kind(schema),
                    required: schema.get("default").is_none(),
                    help: schema
                        .get("description")
                        .and_then(Value::as_str)
                        .map(sentence)
                        .unwrap_or_default(),
                    sensitive: false,
                },
            );
        }
    }
    flags
}

/// Make every command path unique and keep the kit's own commands free:
/// a first segment the kit uses gets `-resource`, a repeated path a number.
fn settle_paths(paths: &mut [&mut Vec<String>], diags: &mut Diagnostics) {
    let mut renamed = BTreeSet::new();
    for p in paths.iter_mut() {
        if let Some(first) = p.first().cloned()
            && RESERVED_COMMANDS.contains(&first.as_str())
        {
            if renamed.insert(first.clone()) {
                diags.push(
                    Diagnostic::warning(
                        "TG0751",
                        format!(
                            "`{first}` is a command of the CLI itself; the commands of the API's `{first}` are under `{first}-resource`"
                        ),
                    )
                    .with_help("Rename the resource under `resources` in tungsten.yml to choose another name."),
                );
            }
            p[0] = format!("{first}-resource");
        }
    }
    let mut seen: BTreeSet<Vec<String>> = BTreeSet::new();
    for p in paths.iter_mut() {
        if seen.contains(&**p) {
            let original = p.join(" ");
            let mut n = 2;
            loop {
                let mut candidate = (**p).clone();
                if let Some(last) = candidate.last_mut() {
                    *last = format!("{last}-{n}");
                }
                if !seen.contains(&candidate) {
                    **p = candidate;
                    break;
                }
                n += 1;
            }
            diags.push(Diagnostic::warning(
                "TG0753",
                format!(
                    "two commands have the path `{original}`; the later one is `{}`",
                    p.join(" ")
                ),
            ));
        }
        seen.insert((**p).clone());
    }
}

/// The table of `ir`, and the diagnostics of its gaps.
pub(crate) fn build(ir: &Ir) -> (Model, Diagnostics) {
    let mut diags = Diagnostics::new();
    let mut ops: Vec<OpCommand> = vec![];
    let mut body_required: Vec<(String, bool)> = vec![];
    for (op, path) in op_paths(ir) {
        let built = op_flags(ir, op, &mut diags);
        body_required.push((op.id.0.clone(), built.body_required));
        ops.push(OpCommand {
            id: op.id.0.clone(),
            path,
            about: op_about(op),
            safety: op.agent.safety,
            flags: built.flags,
            body_arg: built.body_arg,
            paginated: op.pagination.is_some(),
            schema: operation_parameters(ir, op),
            remediation: remedies(&op.agent.remediation),
            remediation_note: op.agent.remediation_note.clone(),
        });
    }
    let member_words: Vec<Vec<String>> = ir
        .agent
        .macros
        .iter()
        .map(|m| {
            let id = m.name.0.as_str();
            naming::split_words(id.split_once('.').map_or(id, |(_, rest)| rest))
        })
        .collect();
    let members = unique_words(member_words, Role::Module);
    let required = |id: &str| body_required.iter().any(|(i, r)| i == id && *r);
    let mut macros: Vec<MacroCommand> = vec![];
    for (m, member) in ir.agent.macros.iter().zip(&members) {
        macros.push(MacroCommand {
            name: m.name.0.clone(),
            path: vec!["macros".to_string(), kebab(member)],
            about: m.summary.clone(),
            safety: m.safety,
            flags: macro_flags(ir, m, &ops, &mut diags, &required),
            schema: macro_parameters(ir, m),
        });
    }
    let mut paths: Vec<&mut Vec<String>> = ops
        .iter_mut()
        .map(|o| &mut o.path)
        .chain(macros.iter_mut().map(|m| &mut m.path))
        .collect();
    settle_paths(&mut paths, &mut diags);
    (
        Model {
            ops,
            macros,
            error_codes: remedies(&ir.agent.error_codes),
        },
        diags,
    )
}
