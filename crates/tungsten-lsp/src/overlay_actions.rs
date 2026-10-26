// SPDX-License-Identifier: AGPL-3.0-only
//! Quick fixes that write an overlay: the actions `tungsten overlay suggest`
//! derives for TG0403 (an operation without `operationId`) and TG0401 (a
//! repeated operation id).
//!
//! The fix is not in the buffer the diagnostic is shown in: the spec stays
//! as it is and an overlay sets the id the compiler already uses. The edit
//! therefore spans files. The action is added to the input's suggested
//! overlay, `<namespace>.suggested-overlay.yaml` next to `tungsten.yml`,
//! when that file is already listed under the input's `overlays`; otherwise
//! the file is created and listed in `tungsten.yml` in the same edit.
//!
//! Every file of the edit is named in the action's title and, when the
//! client supports change annotations, the edit asks for confirmation, so
//! the editor previews all of it before anything changes. Open buffers are
//! edited at the version the fix was computed for, a new file is created
//! only when it does not exist, and no fix is offered when the manifest or
//! overlay text does not have the shape the edit expects. The server never
//! writes a file itself.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use lsp_types::{
    AnnotatedTextEdit, ChangeAnnotation, CodeAction, CodeActionKind, CodeActionOrCommand,
    CreateFile, CreateFileOptions, Diagnostic, DocumentChangeOperation, DocumentChanges,
    NumberOrString, OneOf, OptionalVersionedTextDocumentIdentifier, Position, Range, ResourceOp,
    TextDocumentEdit, TextEdit, WorkspaceEdit,
};
use tungsten_build::suggest::{InputSuggestions, render_actions, render_overlay};

use crate::analysis::OverlayFix;
use crate::position::LineIndex;
use crate::uri::path_to_uri;

/// Most diagnostics of one request that get an overlay fix.
const MAX_DIAGNOSTICS: usize = 50;
/// Names tried for a new suggested overlay of one input.
const MAX_NAMES: usize = 9;
const ANNOTATION: &str = "tungsten-overlay";

/// What the client can apply.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditSupport {
    /// `documentChanges` in workspace edits; required for any overlay fix.
    pub document_changes: bool,
    /// The `create` resource operation; required for a new overlay file.
    pub create_files: bool,
    /// Change annotations, used to ask for confirmation.
    pub annotations: bool,
}

/// The text of a file as the editor sees it: the open buffer and its
/// version, else the file on disk.
pub type Reader<'a> = dyn Fn(&Path) -> Option<(String, Option<i32>)> + 'a;

/// The overlay fixes for the `diagnostics` (as the client sent them back)
/// of the file at `path`.
pub fn actions(
    path: &Path,
    diagnostics: &[Diagnostic],
    fixes: &[OverlayFix],
    manifest: &Path,
    support: EditSupport,
    read: &Reader<'_>,
) -> Vec<CodeActionOrCommand> {
    if !support.document_changes || fixes.is_empty() {
        return vec![];
    }
    let mut out = vec![];
    let mut whole_inputs = vec![];
    for d in diagnostics
        .iter()
        .filter(|d| d.source.as_deref() == Some("tungsten"))
        .take(MAX_DIAGNOSTICS)
    {
        let Some(NumberOrString::String(code)) = &d.code else {
            continue;
        };
        let Some(fix) = fixes.iter().find(|f| {
            f.fixes
                .iter()
                .any(|(p, r, c)| p == path && *r == d.range && c == code)
        }) else {
            continue;
        };
        if let Some(action) = overlay_action(&[fix], d, manifest, support, read) {
            out.push(CodeActionOrCommand::CodeAction(action));
        }
        let same_input: Vec<&OverlayFix> = fixes.iter().filter(|f| f.input == fix.input).collect();
        if same_input.len() > 1 && !whole_inputs.contains(&fix.input) {
            whole_inputs.push(fix.input);
            if let Some(action) = overlay_action(&same_input, d, manifest, support, read) {
                out.push(CodeActionOrCommand::CodeAction(action));
            }
        }
    }
    out
}

/// One code action adding the actions of `fixes` (all of one input) to its
/// suggested overlay.
fn overlay_action(
    fixes: &[&OverlayFix],
    diagnostic: &Diagnostic,
    manifest: &Path,
    support: EditSupport,
    read: &Reader<'_>,
) -> Option<CodeAction> {
    let first = fixes.first()?;
    let dir = manifest.parent()?;
    for n in 1..=MAX_NAMES {
        let name = if n == 1 {
            format!("{}.suggested-overlay.yaml", first.namespace)
        } else {
            format!("{}.suggested-overlay-{n}.yaml", first.namespace)
        };
        let path = dir.join(&name);
        let listed = first.overlays.iter().any(|o| o == &name);
        let existing = read(&path);
        let plan = match (listed, existing) {
            (true, Some((text, version))) => append(fixes, &path, &text, version, support)?,
            (true, None) | (false, Some(_)) => continue,
            (false, None) => create(fixes, &name, &path, manifest, support, read)?,
        };
        return Some(code_action(fixes, diagnostic, plan, support));
    }
    None
}

/// The changes of one fix and the files they touch, for the title.
struct Plan {
    changes: Vec<DocumentChangeOperation>,
    files: Vec<String>,
}

/// Append the actions to an overlay that already holds suggested actions.
fn append(
    fixes: &[&OverlayFix],
    path: &Path,
    text: &str,
    version: Option<i32>,
    support: EditSupport,
) -> Option<Plan> {
    let fresh: Vec<_> = fixes
        .iter()
        .map(|f| f.action.clone())
        .filter(|a| !text.contains(&quoted(&a.target)))
        .collect();
    if fresh.is_empty() {
        return None;
    }
    let newline = newline_of(text);
    let items = render_actions(&fresh).replace('\n', newline);
    let lines = LineIndex::new(text);
    let (start, end, new_text) = actions_tail(text, &items, newline)?;
    let edit = TextEdit {
        range: lines.range(start, end, text),
        new_text,
    };
    Some(Plan {
        changes: vec![DocumentChangeOperation::Edit(text_edit(
            path, version, edit, support,
        )?)],
        files: vec![display(path)],
    })
}

/// Create the overlay with the actions and list it under the input's
/// `overlays` in the manifest.
fn create(
    fixes: &[&OverlayFix],
    name: &str,
    path: &Path,
    manifest: &Path,
    support: EditSupport,
    read: &Reader<'_>,
) -> Option<Plan> {
    if !support.create_files {
        return None;
    }
    let first = fixes.first()?;
    let (manifest_text, manifest_version) = read(manifest)?;
    let newline = newline_of(&manifest_text);
    let (start, end, insert) = list_overlay(&manifest_text, first.input, &first.spec, name)?;
    let lines = LineIndex::new(&manifest_text);
    let manifest_edit = TextEdit {
        range: lines.range(start, end, &manifest_text),
        new_text: insert.replace('\n', newline),
    };
    let input = InputSuggestions {
        namespace: first.namespace.clone(),
        spec: first.spec.clone(),
        actions: fixes.iter().map(|f| f.action.clone()).collect(),
    };
    let rendered = render_overlay(&input);
    let body = rendered
        .split_once("overlay: 1.0.0\n")
        .map_or(rendered.as_str(), |(_, rest)| rest);
    let content = format!(
        "# Suggested by `tungsten lsp` for namespace {} (spec {}).\n\
         # Listed under `overlays` of that input in tungsten.yml.\n\
         overlay: 1.0.0\n{body}",
        quoted(&first.namespace),
        quoted(&first.spec),
    );
    let uri = path_to_uri(path)?;
    let annotation = support.annotations.then(|| ANNOTATION.to_string());
    let create = DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
        uri,
        options: Some(CreateFileOptions {
            overwrite: Some(false),
            ignore_if_exists: Some(false),
        }),
        annotation_id: annotation,
    }));
    let fill = TextEdit {
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        new_text: content,
    };
    Some(Plan {
        changes: vec![
            create,
            DocumentChangeOperation::Edit(text_edit(path, None, fill, support)?),
            DocumentChangeOperation::Edit(text_edit(
                manifest,
                manifest_version,
                manifest_edit,
                support,
            )?),
        ],
        files: vec![format!("{} (new)", display(path)), display(manifest)],
    })
}

fn code_action(
    fixes: &[&OverlayFix],
    diagnostic: &Diagnostic,
    plan: Plan,
    support: EditSupport,
) -> CodeAction {
    let files = plan.files.join(", ");
    let title = match fixes {
        [one] => {
            let id = one
                .action
                .update
                .get("operationId")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            format!("Set operationId `{id}` with an overlay ({files})")
        }
        many => format!(
            "Add all {} suggested overlay fixes for namespace `{}` ({files})",
            many.len(),
            many[0].namespace
        ),
    };
    let change_annotations = support.annotations.then(|| {
        HashMap::from([(
            ANNOTATION.to_string(),
            ChangeAnnotation {
                label: "Add a suggested overlay".to_string(),
                needs_confirmation: Some(true),
                description: Some(format!("Edits {files}")),
            },
        )])
    });
    CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(WorkspaceEdit {
            changes: None,
            document_changes: Some(DocumentChanges::Operations(plan.changes)),
            change_annotations,
        }),
        is_preferred: Some(false),
        ..CodeAction::default()
    }
}

fn text_edit(
    path: &Path,
    version: Option<i32>,
    edit: TextEdit,
    support: EditSupport,
) -> Option<TextDocumentEdit> {
    let edit = if support.annotations {
        OneOf::Right(AnnotatedTextEdit {
            text_edit: edit,
            annotation_id: ANNOTATION.to_string(),
        })
    } else {
        OneOf::Left(edit)
    };
    Some(TextDocumentEdit {
        text_document: OptionalVersionedTextDocumentIdentifier {
            uri: path_to_uri(path)?,
            version,
        },
        edits: vec![edit],
    })
}

fn display(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A JSON string literal, the way the rendered overlay quotes targets.
fn quoted(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

fn newline_of(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// One line of a YAML file: its byte range (with the line break), its
/// indentation and its content without the line break or a trailing
/// comment.
struct Line<'t> {
    start: usize,
    end: usize,
    indent: usize,
    content: &'t str,
}

impl Line<'_> {
    fn blank(&self) -> bool {
        self.content.is_empty()
    }
}

fn yaml_lines(text: &str) -> Vec<Line<'_>> {
    let mut out = vec![];
    let mut at = 0;
    for raw in text.split_inclusive('\n') {
        let start = at;
        at += raw.len();
        let body = raw.trim_end_matches(['\n', '\r']);
        let indent = body.len() - body.trim_start_matches(' ').len();
        let content = strip_comment(&body[indent..]).trim_end();
        out.push(Line {
            start,
            end: at,
            indent,
            content,
        });
    }
    out
}

/// `text` without a trailing `# comment`, outside quotes.
fn strip_comment(text: &str) -> &str {
    let mut quote = None;
    let mut previous = ' ';
    for (i, c) in text.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && previous.is_whitespace() => return &text[..i],
            None => {}
        }
        previous = c;
    }
    text
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        if value.starts_with('"')
            && let Ok(s) = serde_json::from_str::<String>(value)
        {
            return s;
        }
        return value[1..value.len() - 1].replace("''", "'");
    }
    value.to_string()
}

/// The edit (byte range and text) that lists `name` last under `overlays`
/// of input number `input` of a block-style manifest, provided that input's
/// `spec` is `spec`. `None` when the manifest is not laid out that way.
fn list_overlay(
    text: &str,
    input: usize,
    spec: &str,
    name: &str,
) -> Option<(usize, usize, String)> {
    let lines = yaml_lines(text);
    let top = lines
        .iter()
        .position(|l| l.indent == 0 && l.content == "inputs:")?;
    let block_end = lines[top + 1..]
        .iter()
        .position(|l| !l.blank() && l.indent == 0 && !l.content.starts_with('-'))
        .map_or(lines.len(), |i| top + 1 + i);
    let block = &lines[top + 1..block_end];
    let dash = block.iter().find(|l| !l.blank())?;
    if !dash.content.starts_with('-') {
        return None;
    }
    let item_indent = dash.indent;
    let starts: Vec<usize> = block
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.blank() && l.indent == item_indent && l.content.starts_with('-'))
        .map(|(i, _)| i)
        .collect();
    let first = *starts.get(input)?;
    let last = starts.get(input + 1).copied().unwrap_or(block.len());
    let item = &block[first..last];
    let head = &item[0];
    let after_dash = head.content[1..].trim_start();
    if after_dash.is_empty() {
        return None;
    }
    let key_indent = head.indent + (head.content.len() - after_dash.len());
    // The keys of the item, with the line they start on.
    let mut keys: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
    for (i, line) in item.iter().enumerate() {
        let content = if i == 0 {
            after_dash
        } else if !line.blank() && line.indent == key_indent && !line.content.starts_with('-') {
            line.content
        } else {
            continue;
        };
        let (key, value) = content.split_once(':')?;
        keys.insert(key.trim(), (i, value.trim()));
    }
    let (_, spec_value) = keys.get("spec")?;
    if unquote(spec_value) != spec {
        return None;
    }
    let last_content = item.iter().rposition(|l| !l.blank())?;
    let pad = |n: usize| " ".repeat(n);
    let after = |line: &Line<'_>, insert: String| {
        let prefix = if text[..line.end].ends_with('\n') {
            ""
        } else {
            "\n"
        };
        Some((line.end, line.end, format!("{prefix}{insert}")))
    };
    let Some(&(at, value)) = keys.get("overlays") else {
        return after(
            &item[last_content],
            format!(
                "{}overlays:\n{}- {name}\n",
                pad(key_indent),
                pad(key_indent + 2)
            ),
        );
    };
    if !value.is_empty() {
        let inner = value.strip_prefix('[')?.strip_suffix(']')?;
        let line = &item[at];
        let line_text = &text[line.start..line.end];
        let close = line.start + line_text.rfind(']')?;
        return if inner.trim().is_empty() {
            let open = line.start + line_text.rfind('[')?;
            Some((open, close + 1, format!("[{name}]")))
        } else {
            Some((close, close, format!(", {name}")))
        };
    }
    let mut last_entry = None;
    for (i, line) in item.iter().enumerate().skip(at + 1) {
        if line.blank() {
            continue;
        }
        let entry = line.indent > key_indent
            || (line.indent == key_indent && line.content.starts_with('-'));
        if !entry {
            break;
        }
        if line.content.starts_with('-') {
            last_entry = Some((i, line.indent));
        } else if last_entry.is_none() {
            return None;
        }
    }
    match last_entry {
        Some((i, indent)) => after(&item[i], format!("{}- {name}\n", pad(indent))),
        None => after(&item[at], format!("{}- {name}\n", pad(key_indent + 2))),
    }
}

/// The edit appending `items` to the top-level `actions` list of an
/// overlay, when that list is last in the file and written as a block (or
/// as `[]`).
fn actions_tail(text: &str, items: &str, newline: &str) -> Option<(usize, usize, String)> {
    let lines = yaml_lines(text);
    let at = lines
        .iter()
        .position(|l| l.indent == 0 && l.content.starts_with("actions:"))?;
    let value = lines[at].content["actions:".len()..].trim();
    let rest_is_list = lines[at + 1..].iter().all(|l| l.blank() || l.indent > 0);
    if !rest_is_list {
        return None;
    }
    match value {
        "" => {
            let end = text.len();
            let prefix = if text.ends_with('\n') { "" } else { newline };
            Some((end, end, format!("{prefix}{items}")))
        }
        "[]" => {
            if lines[at + 1..].iter().any(|l| !l.blank()) {
                return None;
            }
            let line = &lines[at];
            let body_end = line.start
                + text[line.start..line.end]
                    .trim_end_matches(['\n', '\r'])
                    .len();
            Some((
                line.start,
                body_end,
                format!("actions:{newline}{}", items.trim_end_matches(['\n', '\r'])),
            ))
        }
        _ => None,
    }
}
