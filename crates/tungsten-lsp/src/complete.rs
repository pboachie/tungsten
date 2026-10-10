// SPDX-License-Identifier: AGPL-3.0-only
//! Completion: keys from the published schemas, values from the schemas and
//! from the compiled project (operation ids, gates, clusters).

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionTextEdit, Documentation, MarkupContent,
    MarkupKind, TextEdit,
};

use crate::analysis::Index;
use crate::docs::{self, Kind};
use crate::position::LineIndex;
use crate::scan::{self, Context, Seg, Slot, shape};
use crate::schema::Schema;

/// Everything completion reads.
pub struct Inputs<'a> {
    pub kind: Kind,
    pub text: &'a str,
    pub lines: &'a LineIndex,
    pub index: Option<&'a Index>,
    pub manifest: &'a Schema,
    pub agent: &'a Schema,
}

impl Inputs<'_> {
    fn schema(&self) -> Option<&Schema> {
        match self.kind {
            Kind::Manifest => Some(self.manifest),
            Kind::Agent => Some(self.agent),
            Kind::Overlay => None,
        }
    }
}

pub fn complete(inputs: &Inputs<'_>, offset: usize) -> Vec<CompletionItem> {
    let Some(cx) = scan::context(inputs.text, inputs.lines, offset) else {
        return vec![];
    };
    let item_is_mapping = cx.item && has_keys(inputs, &cx.path);
    match cx.slot {
        Slot::Key if !cx.item || item_is_mapping => keys(inputs, &cx),
        _ => values(inputs, &cx),
    }
}

fn has_keys(inputs: &Inputs<'_>, path: &[Seg]) -> bool {
    match inputs.schema() {
        Some(schema) => schema.has_properties(path),
        None => !overlay_keys(path).is_empty(),
    }
}

fn edit(inputs: &Inputs<'_>, cx: &Context, new_text: String) -> CompletionTextEdit {
    let range = inputs.lines.range(cx.word.0, cx.word.1, inputs.text);
    CompletionTextEdit::Edit(TextEdit { range, new_text })
}

fn markdown(text: String) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value: text,
    })
}

fn item(
    inputs: &Inputs<'_>,
    cx: &Context,
    label: &str,
    kind: CompletionItemKind,
    insert: String,
) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        filter_text: Some(label.to_string()),
        text_edit: Some(edit(inputs, cx, insert)),
        ..CompletionItem::default()
    }
}

/// Keys that may start at the cursor.
fn keys(inputs: &Inputs<'_>, cx: &Context) -> Vec<CompletionItem> {
    let scan = scan::scan(inputs.text, inputs.lines);
    let present = scan.children(&cx.path);
    let after = &inputs.text[cx.word.1..];
    let colon_follows = after.starts_with(':');
    let insert = |name: &str| {
        if colon_follows {
            name.to_string()
        } else {
            format!("{name}: ")
        }
    };
    let mut out = vec![];
    let mut names: Vec<(String, Option<String>)> = vec![];
    match inputs.schema() {
        Some(schema) => {
            for p in schema.properties(&cx.path) {
                let doc = docs::field(inputs.kind, &join(&cx.path, &p.name))
                    .map(str::to_string)
                    .or(p.description);
                names.push((p.name, doc));
            }
        }
        None => {
            for name in overlay_keys(&cx.path) {
                let doc = docs::field(Kind::Overlay, &join(&cx.path, name)).map(str::to_string);
                names.push(((*name).to_string(), doc));
            }
        }
    }
    for (name, doc) in names {
        if present.contains(&name.as_str()) {
            continue;
        }
        let mut it = item(
            inputs,
            cx,
            &name,
            CompletionItemKind::PROPERTY,
            insert(&name),
        );
        it.documentation = doc.map(markdown);
        out.push(it);
    }
    out.extend(dynamic_keys(inputs, cx, &present, &insert));
    out
}

fn join(path: &[Seg], name: &str) -> Vec<Seg> {
    let mut p = path.to_vec();
    p.push(Seg::Key(name.to_string()));
    p
}

/// Keys of maps whose names come from the project.
fn dynamic_keys(
    inputs: &Inputs<'_>,
    cx: &Context,
    present: &[&str],
    insert: &dyn Fn(&str) -> String,
) -> Vec<CompletionItem> {
    let mut out = vec![];
    let mut push =
        |name: &str, kind: CompletionItemKind, detail: Option<String>, doc: Option<String>| {
            if present.contains(&name) {
                return;
            }
            let mut it = item(inputs, cx, name, kind, insert(name));
            it.detail = detail;
            it.documentation = doc.map(markdown);
            out.push(it);
        };
    let here = shape(&cx.path);
    let index = inputs.index;
    match (inputs.kind, here.as_str()) {
        (Kind::Agent, "gates") => {
            for g in index.map(|i| i.gates.as_slice()).unwrap_or_default() {
                push(g, CompletionItemKind::CONSTANT, Some("gate".into()), None);
            }
        }
        (Kind::Manifest, "naming.operations" | "pagination") => {
            for op in index.map(|i| i.ops.as_slice()).unwrap_or_default() {
                push(
                    &op.id,
                    CompletionItemKind::METHOD,
                    Some(op_detail(op)),
                    op.summary.clone(),
                );
            }
        }
        (Kind::Manifest, "resources") => {
            for ns in index.map(|i| i.namespaces.as_slice()).unwrap_or_default() {
                push(
                    ns,
                    CompletionItemKind::MODULE,
                    Some("namespace".into()),
                    None,
                );
            }
        }
        (Kind::Manifest, "targets") => {
            for t in tungsten_config::KNOWN_TARGETS {
                push(t, CompletionItemKind::MODULE, Some("target".into()), None);
            }
        }
        (Kind::Manifest, s) if s.starts_with("targets.") && cx.path.len() == 2 => {
            if let Some(Seg::Key(target)) = cx.path.get(1) {
                for key in target_keys(target) {
                    let path = [
                        Seg::Key("targets".into()),
                        Seg::Key(target.clone()),
                        Seg::Key((*key).to_string()),
                    ];
                    let doc = docs::field(Kind::Manifest, &path).map(str::to_string);
                    push(key, CompletionItemKind::PROPERTY, None, doc);
                }
            }
        }
        _ => {}
    }
    out
}

fn target_keys(target: &str) -> &'static [&'static str] {
    match target {
        "typescript" => &["package", "runtime", "out"],
        "python" => &["package", "runtime", "models", "out"],
        "rust" => &["crate", "runtime", "cli", "out"],
        "mcp" => &["package", "runtime", "out"],
        _ => &["out"],
    }
}

fn overlay_keys(path: &[Seg]) -> &'static [&'static str] {
    match shape(path).as_str() {
        "" => &["overlay", "info", "extends", "actions"],
        "info" => &["title", "version"],
        "actions[]" => &["target", "description", "update", "remove"],
        _ => &[],
    }
}

fn op_detail(op: &crate::analysis::OpInfo) -> String {
    format!("{} {}", op.method, op.path)
}

/// Values for the path under the cursor.
fn values(inputs: &Inputs<'_>, cx: &Context) -> Vec<CompletionItem> {
    let mut out = vec![];
    let here = shape(&cx.path);
    let index = inputs.index;
    let ops = |out: &mut Vec<CompletionItem>, with_macros: bool, globs: bool| {
        let Some(index) = index else { return };
        for op in &index.ops {
            let mut it = item(
                inputs,
                cx,
                &op.id,
                CompletionItemKind::METHOD,
                op.id.clone(),
            );
            it.detail = Some(op_detail(op));
            it.documentation = Some(markdown(op_markdown(op)));
            out.push(it);
        }
        if with_macros {
            for m in &index.macros {
                let mut it = item(inputs, cx, m, CompletionItemKind::FUNCTION, m.clone());
                it.detail = Some("macro".into());
                out.push(it);
            }
        }
        if globs {
            for ns in &index.namespaces {
                let glob = format!("{ns}.*");
                let mut it = item(inputs, cx, &glob, CompletionItemKind::MODULE, glob.clone());
                it.detail = Some("every operation of the namespace".into());
                out.push(it);
            }
        }
    };
    let names = |out: &mut Vec<CompletionItem>, list: &[String], kind, detail: &str| {
        for name in list {
            let mut it = item(inputs, cx, name, kind, name.clone());
            it.detail = Some(detail.to_string());
            out.push(it);
        }
    };
    match (inputs.kind, here.as_str()) {
        (
            Kind::Agent,
            "tools[].operation"
            | "tools[].verify.operation"
            | "macros[].steps[].call"
            | "macros[].steps[].poll"
            | "macros[].steps[].paginate",
        ) => ops(&mut out, false, false),
        (Kind::Agent, "disclosure.clusters[].operations[]") => ops(&mut out, true, true),
        (Kind::Agent, "tools[].gate") => {
            names(
                &mut out,
                list(index, |i| &i.gates),
                CompletionItemKind::CONSTANT,
                "gate",
            );
        }
        (Kind::Agent, "tools[].cluster") => {
            names(
                &mut out,
                list(index, |i| &i.clusters),
                CompletionItemKind::ENUM_MEMBER,
                "cluster",
            );
        }
        (Kind::Manifest, "auth_profiles.*.satisfies[]") => {
            names(
                &mut out,
                list(index, |i| &i.auth_schemes),
                CompletionItemKind::CONSTANT,
                "security scheme",
            );
        }
        _ => {}
    }
    if suppress_list(&cx.path) {
        for (code, summary) in tungsten_core::diagnostic::codes::REGISTRY {
            let mut it = item(
                inputs,
                cx,
                code,
                CompletionItemKind::VALUE,
                (*code).to_string(),
            );
            it.detail = Some((*summary).to_string());
            out.push(it);
        }
    }
    if let Some(schema) = inputs.schema() {
        for choice in schema.choices(&cx.path) {
            let doc = docs::value(inputs.kind, &cx.path, &choice.value)
                .map(str::to_string)
                .or(choice.description);
            let mut it = item(
                inputs,
                cx,
                &choice.value,
                CompletionItemKind::ENUM_MEMBER,
                choice.value.clone(),
            );
            it.documentation = doc.map(markdown);
            out.push(it);
        }
        if schema.is_boolean(&cx.path) {
            for b in ["true", "false"] {
                out.push(item(
                    inputs,
                    cx,
                    b,
                    CompletionItemKind::VALUE,
                    b.to_string(),
                ));
            }
        }
    }
    out
}

/// Whether the path is the `suppress` list of a manifest, or an item of it.
fn suppress_list(path: &[Seg]) -> bool {
    let key = Seg::Key("suppress".to_string());
    match path {
        [.., last] if *last == key => true,
        [.., key_seg, Seg::Item(_)] => *key_seg == key,
        _ => false,
    }
}

fn list<'a>(index: Option<&'a Index>, pick: impl Fn(&'a Index) -> &'a Vec<String>) -> &'a [String] {
    index.map(|i| pick(i).as_slice()).unwrap_or_default()
}

/// Markdown for an operation: method, path, summary and status.
pub fn op_markdown(op: &crate::analysis::OpInfo) -> String {
    let mut out = format!("`{} {}`", op.method, op.path);
    if let Some(summary) = &op.summary {
        out.push_str("\n\n");
        out.push_str(summary);
    }
    out.push_str(&format!("\n\n{}", op.status));
    if op.deprecated {
        out.push_str(" · deprecated");
    }
    out
}
