// SPDX-License-Identifier: AGPL-3.0-only
//! The static HTML form of `tungsten report` (planning/07 "Generation
//! report").
//!
//! One self-contained file: inline CSS, no scripts, no external assets or
//! links (only in-page anchors), light and dark through
//! `prefers-color-scheme`, tables that scroll inside their own box on a
//! phone. Every string from the specs, manifests or diagnostics is escaped.
//! The output depends only on the report data.

use std::fmt::Write as _;

use tungsten_core::Severity;

use crate::commands::diff::{change_name, describe, describe_semver, level_name};
use crate::output::{
    CellOrigin, CoverageStatus, DiagnosticGroup, JsonDiagnostic, ReportResult, SafetyCell,
    SafetyRow, TargetDiff, ToolCost, ToolKind,
};

/// Tools listed as the most expensive.
const TOP_TOOLS: usize = 10;

const STYLE: &str = r#"
:root {
  --bg: #ffffff; --fg: #1d2026; --muted: #5d6470; --line: #d9dde3; --panel: #f5f7fa;
  --accent: #2456c7; --ok: #1d7a46; --warn: #9a6200; --err: #b3261e; --bar: #7d9be0;
  --ro: #e3f1e8; --mut: #e6edfb; --des: #fdf0dc; --irr: #fbe3e1;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #14161a; --fg: #e6e8eb; --muted: #9aa2ae; --line: #30353d; --panel: #1c1f25;
    --accent: #8fb0ff; --ok: #6fcf97; --warn: #f2c14e; --err: #ff8a80; --bar: #4f6fb8;
    --ro: #1d3326; --mut: #1f2a40; --des: #3a2e16; --irr: #3d1f1d;
  }
}
* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; }
body { margin: 0; background: var(--bg); color: var(--fg);
  font: 15px/1.5 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }
header, main, footer { max-width: 1180px; margin: 0 auto; padding: 0 16px; }
header { padding-top: 24px; }
h1 { font-size: 1.5rem; margin: 0 0 4px; }
h2 { font-size: 1.2rem; margin: 32px 0 8px; padding-top: 8px; border-top: 1px solid var(--line); }
h3 { font-size: 1rem; margin: 20px 0 6px; }
p { margin: 6px 0; }
.meta, .note, footer { color: var(--muted); }
nav { display: flex; flex-wrap: wrap; gap: 6px 14px; margin: 12px 0 0; }
a { color: var(--accent); }
code, .mono { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 0.86em; }
.scroll { overflow-x: auto; -webkit-overflow-scrolling: touch; border: 1px solid var(--line); border-radius: 6px; margin: 8px 0; }
table { border-collapse: collapse; width: 100%; font-size: 0.9rem; }
th, td { text-align: left; padding: 6px 10px; border-bottom: 1px solid var(--line); vertical-align: top; }
th { background: var(--panel); font-weight: 600; white-space: nowrap; }
tr:last-child td { border-bottom: 0; }
td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
.src { display: block; color: var(--muted); font-size: 0.78rem; overflow-wrap: anywhere; }
.badge { display: inline-block; padding: 0 6px; border-radius: 4px; font-size: 0.8rem; white-space: nowrap; }
.read_only { background: var(--ro); } .mutating { background: var(--mut); }
.destructive { background: var(--des); } .irreversible { background: var(--irr); }
.error { color: var(--err); } .warning { color: var(--warn); } .info { color: var(--muted); }
.ok { color: var(--ok); }
.barcell { min-width: 120px; }
.bar { display: block; height: 8px; border-radius: 4px; background: var(--bar); }
.cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 8px; margin: 8px 0; }
.card { background: var(--panel); border: 1px solid var(--line); border-radius: 6px; padding: 8px 10px; }
.card b { display: block; font-size: 1.25rem; font-variant-numeric: tabular-nums; }
details { border: 1px solid var(--line); border-radius: 6px; margin: 6px 0; background: var(--panel); }
summary { cursor: pointer; padding: 6px 10px; }
details ul { margin: 0; padding: 6px 10px 10px 28px; background: var(--bg); border-radius: 0 0 6px 6px; }
details li { margin: 4px 0; overflow-wrap: anywhere; }
footer { padding: 32px 16px 24px; font-size: 0.85rem; }
"#;

/// `&`, `<`, `>`, `"` and `'` escaped, for text and attribute values.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The report as one HTML document.
pub(crate) fn render(r: &ReportResult) -> String {
    let api = r
        .summary
        .stats
        .as_ref()
        .map(|s| s.api.clone())
        .unwrap_or_default();
    let title = r.summary.title.clone().unwrap_or_else(|| api.clone());
    let mut h = String::new();
    let _ = write!(
        h,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"color-scheme\" content=\"light dark\">\n\
         <title>{} · tungsten report</title>\n<style>{STYLE}</style>\n</head>\n<body>\n",
        escape(&title)
    );
    header(&mut h, r, &title);
    h.push_str("<main>\n");
    coverage(&mut h, r);
    safety(&mut h, r);
    budgets(&mut h, r);
    diagnostics(&mut h, r);
    changes(&mut h, r);
    h.push_str("</main>\n");
    let _ = writeln!(
        h,
        "<footer>Generated by <code>tungsten report</code> {}. The report depends only on its inputs: no timestamps, paths as given.</footer>",
        escape(tungsten_build::TUNGSTEN_VERSION)
    );
    h.push_str("</body>\n</html>\n");
    h
}

fn header(h: &mut String, r: &ReportResult, title: &str) {
    let s = &r.summary;
    let _ = writeln!(
        h,
        "<header>\n<h1>{} · generation report</h1>",
        escape(title)
    );
    let mut meta = vec![format!("tungsten {}", tungsten_build::TUNGSTEN_VERSION)];
    if let Some(stats) = &s.stats {
        meta.push(format!("API <code>{}</code>", escape(&stats.api)));
    }
    if let Some(v) = &s.api_version {
        meta.push(format!("version {}", escape(v)));
    }
    meta.push(plural(s.documents, "document", "documents"));
    meta.push(format!(
        "{} · {} · {}",
        plural(s.counts.errors, "error", "errors"),
        plural(s.counts.warnings, "warning", "warnings"),
        plural(s.counts.infos, "note", "notes")
    ));
    let _ = writeln!(h, "<p class=\"meta\">{}</p>", meta.join(" · "));
    h.push_str(
        "<nav><a href=\"#coverage\">Coverage</a><a href=\"#safety\">Safety matrix</a>\
         <a href=\"#budgets\">Token budgets</a><a href=\"#diagnostics\">Diagnostics</a>\
         <a href=\"#changes\">Changes</a></nav>\n</header>\n",
    );
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn card(h: &mut String, value: impl std::fmt::Display, label: &str) {
    let _ = write!(
        h,
        "<div class=\"card\"><b>{}</b>{}</div>",
        escape(&value.to_string()),
        escape(label)
    );
}

/// A table inside a scrolling box. `head` cells ending in `#` are numeric.
fn table(h: &mut String, head: &[&str], rows: &[Vec<String>]) {
    h.push_str("<div class=\"scroll\"><table>\n<thead><tr>");
    for cell in head {
        match cell.strip_suffix('#') {
            Some(name) => {
                let _ = write!(h, "<th class=\"num\">{}</th>", escape(name));
            }
            None => {
                let _ = write!(h, "<th>{}</th>", escape(cell));
            }
        }
    }
    h.push_str("</tr></thead>\n<tbody>\n");
    for row in rows {
        h.push_str("<tr>");
        for (i, cell) in row.iter().enumerate() {
            let numeric = head.get(i).is_some_and(|c| c.ends_with('#'));
            if numeric {
                let _ = write!(h, "<td class=\"num\">{cell}</td>");
            } else {
                let _ = write!(h, "<td>{cell}</td>");
            }
        }
        h.push_str("</tr>\n");
    }
    h.push_str("</tbody></table></div>\n");
}

fn coverage(h: &mut String, r: &ReportResult) {
    h.push_str("<section id=\"coverage\">\n<h2>Coverage</h2>\n");
    if let Some(stats) = &r.summary.stats {
        let o = &stats.operations;
        h.push_str("<div class=\"cards\">");
        card(h, o.total, "operations");
        card(h, o.implemented, "implemented");
        card(h, o.planned, "planned (not callable)");
        card(h, o.gated, "gated");
        card(h, stats.types, "named types");
        card(h, stats.resources, "resources");
        h.push_str("</div>\n");
    }
    h.push_str("<h3>Operations by namespace</h3>\n");
    let mut rows: Vec<Vec<String>> = r
        .coverage
        .namespaces
        .iter()
        .map(|ns| {
            vec![
                format!("<code>{}</code>", escape(&ns.namespace)),
                ns.implemented.to_string(),
                ns.planned.to_string(),
                ns.gated.to_string(),
                ns.total.to_string(),
            ]
        })
        .collect();
    if rows.is_empty() {
        h.push_str("<p class=\"note\">No IR was built: see the diagnostics.</p>\n");
    } else {
        let sum = |f: fn(&crate::output::NamespaceCoverage) -> usize| {
            r.coverage
                .namespaces
                .iter()
                .map(f)
                .sum::<usize>()
                .to_string()
        };
        rows.push(vec![
            "<b>all</b>".into(),
            sum(|n| n.implemented),
            sum(|n| n.planned),
            sum(|n| n.gated),
            sum(|n| n.total),
        ]);
        table(
            h,
            &["namespace", "implemented#", "planned#", "gated#", "total#"],
            &rows,
        );
    }
    h.push_str("<h3>By emitter</h3>\n");
    let rows: Vec<Vec<String>> = r
        .coverage
        .targets
        .iter()
        .map(|t| {
            let status = match t.status {
                CoverageStatus::Generated => "<span class=\"ok\">generated</span>",
                CoverageStatus::NotGenerated => "not generated",
                CoverageStatus::NoEmitter => "<span class=\"info\">no emitter yet</span>",
                CoverageStatus::Failed => "<span class=\"error\">failed</span>",
            };
            vec![
                format!("<code>{}</code>", escape(&t.target)),
                if t.configured { "yes" } else { "no" }.into(),
                status.into(),
                t.operations.to_string(),
                t.macros.to_string(),
                t.files.to_string(),
                t.warnings.to_string(),
                t.errors.to_string(),
            ]
        })
        .collect();
    table(
        h,
        &[
            "target",
            "configured",
            "status",
            "operations#",
            "macros#",
            "files#",
            "warnings#",
            "errors#",
        ],
        &rows,
    );
    h.push_str("<p class=\"note\">Emitters run in memory; targets that are not configured use their default options.</p>\n</section>\n");
}

fn origin_name(o: CellOrigin) -> &'static str {
    match o {
        CellOrigin::Tool => "tools entry",
        CellOrigin::Extension => "x-agent extension",
        CellOrigin::Defaults => "manifest defaults",
        CellOrigin::Inferred => "inferred",
        CellOrigin::BuiltIn => "built-in default",
    }
}

fn cell(c: &SafetyCell) -> String {
    let mut source = origin_name(c.origin).to_string();
    if let Some(file) = &c.file {
        let _ = write!(source, " · {}", escape(file));
        if let Some(line) = c.line {
            let _ = write!(source, ":{line}");
        }
    }
    if let Some(pointer) = &c.pointer {
        let _ = write!(source, " <code>{}</code>", escape(pointer));
    }
    format!("{}<span class=\"src\">{source}</span>", escape(&c.value))
}

fn tier(row: &SafetyRow) -> String {
    let value = &row.tier.value;
    let class = match value.as_str() {
        "read_only" | "mutating" | "destructive" | "irreversible" => value.as_str(),
        _ => "",
    };
    let mut c = cell(&row.tier);
    if let Some(rest) = c.strip_prefix(&escape(value)) {
        c = format!(
            "<span class=\"badge {class}\">{}</span>{rest}",
            escape(value)
        );
    }
    c
}

fn safety(h: &mut String, r: &ReportResult) {
    h.push_str("<section id=\"safety\">\n<h2>Safety matrix</h2>\n");
    if r.safety.is_empty() {
        h.push_str("<p class=\"note\">No callable operations.</p>\n</section>\n");
        return;
    }
    let mut tiers = std::collections::BTreeMap::<&str, usize>::new();
    for row in &r.safety {
        *tiers.entry(row.tier.value.as_str()).or_default() += 1;
    }
    h.push_str("<div class=\"cards\">");
    for (t, n) in &tiers {
        card(h, n, t);
    }
    h.push_str("</div>\n<p class=\"note\">Each cell names where its value comes from: an agent manifest <code>tools</code> entry or its <code>defaults</code> (with line and JSON Pointer), an <code>x-agent-*</code> extension in the spec, inference, or the built-in method defaults.</p>\n");
    let rows: Vec<Vec<String>> = r
        .safety
        .iter()
        .map(|row| {
            let gate = if row.gated {
                " <span class=\"badge\">gated</span>"
            } else {
                ""
            };
            vec![
                format!(
                    "<code>{}</code>{gate}<span class=\"src\">{} {}</span>",
                    escape(&row.operation),
                    escape(&row.method),
                    escape(&row.path)
                ),
                tier(row),
                cell(&row.idempotency),
                cell(&row.preview),
                cell(&row.confirmation),
                cell(&row.verify),
            ]
        })
        .collect();
    table(
        h,
        &[
            "operation",
            "tier",
            "idempotency",
            "preview",
            "confirmation",
            "verify",
        ],
        &rows,
    );
    h.push_str("</section>\n");
}

fn bar(tokens: usize, scale: usize) -> String {
    let pct = (tokens.saturating_mul(100) / scale.max(1)).min(100);
    format!("<span class=\"bar\" style=\"width:{pct}%\"></span>")
}

fn kind_name(k: ToolKind) -> &'static str {
    match k {
        ToolKind::Operation => "operation",
        ToolKind::Macro => "macro",
    }
}

fn tool_rows(tools: &[ToolCost], budget: usize, scale: usize) -> Vec<Vec<String>> {
    tools
        .iter()
        .map(|t| {
            let tokens = if t.tokens > budget {
                format!("<span class=\"error\">{}</span>", t.tokens)
            } else {
                t.tokens.to_string()
            };
            vec![
                format!("<code>{}</code>", escape(&t.name)),
                format!("{} <code>{}</code>", kind_name(t.kind), escape(&t.target)),
                tokens,
                format!("<div class=\"barcell\">{}</div>", bar(t.tokens, scale)),
            ]
        })
        .collect()
}

/// The `TOP_TOOLS` most expensive tools, most expensive first.
fn top(tools: &[ToolCost]) -> Vec<ToolCost> {
    let mut sorted = tools.to_vec();
    sorted.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.name.cmp(&b.name)));
    sorted.truncate(TOP_TOOLS);
    sorted
}

fn budgets(h: &mut String, r: &ReportResult) {
    let b = &r.budgets;
    let budget = b.schema_budget as usize;
    h.push_str("<section id=\"budgets\">\n<h2>Token budgets</h2>\n");
    let _ = writeln!(
        h,
        "<p class=\"note\">Counted with <code>{}</code> (calibrated against <code>cl100k_base</code>, leaning high). Budgets from the agent manifest: {} tokens per tool schema, {} per description; progressive disclosure above {} tools.</p>",
        escape(&b.counter),
        b.schema_budget,
        b.description_budget,
        b.threshold
    );
    if !b.documents.is_empty() {
        h.push_str("<h3>Agent documents</h3>\n");
        let rows: Vec<Vec<String>> = b
            .documents
            .iter()
            .map(|d| {
                vec![
                    format!("<code>{}</code>", escape(&d.file)),
                    d.bytes.to_string(),
                    d.tokens.to_string(),
                ]
            })
            .collect();
        table(h, &["file", "bytes#", "tokens#"], &rows);
    }
    h.push_str("<h3>tools.json</h3>\n");
    match &b.tools {
        None => h.push_str("<p class=\"note\">not generated</p>\n"),
        Some(t) => {
            h.push_str("<div class=\"cards\">");
            card(h, t.tools.len(), "tools");
            card(h, t.largest, "largest tool");
            card(h, t.median, "median tool");
            card(h, t.list_tokens, "whole list");
            card(h, t.over_budget, "over budget");
            h.push_str("</div>\n");
            let scale = t.largest.max(budget);
            h.push_str("<h3>Histogram</h3>\n");
            let max_bucket = t.histogram.iter().map(|b| b.tools).max().unwrap_or(0);
            let rows: Vec<Vec<String>> = t
                .histogram
                .iter()
                .map(|bucket| {
                    let range = match bucket.max {
                        Some(max) => format!("{}–{max}", bucket.min),
                        None => format!("{}+", bucket.min),
                    };
                    vec![
                        range,
                        bucket.tools.to_string(),
                        format!(
                            "<div class=\"barcell\">{}</div>",
                            bar(bucket.tools, max_bucket)
                        ),
                    ]
                })
                .collect();
            table(h, &["tokens", "tools#", ""], &rows);
            let _ = writeln!(h, "<h3>The {TOP_TOOLS} most expensive tools</h3>");
            table(
                h,
                &["tool", "calls", "tokens#", ""],
                &tool_rows(&top(&t.tools), budget, scale),
            );
            let _ = writeln!(
                h,
                "<details><summary>Every tool ({})</summary>",
                t.tools.len()
            );
            table(
                h,
                &["tool", "calls", "tokens#", ""],
                &tool_rows(&t.tools, budget, scale),
            );
            h.push_str("</details>\n");
        }
    }
    h.push_str("<h3>MCP manifest</h3>\n");
    match &b.mcp {
        None => h.push_str(
            "<p class=\"note\">not generated: the MCP emitter produced no manifest.</p>\n",
        ),
        Some(m) => {
            h.push_str("<div class=\"cards\">");
            card(h, &m.mode, "mode");
            card(h, m.tools.len(), "tools");
            card(h, m.progressive_tokens, "progressive tools/list");
            card(h, m.discrete_tokens, "discrete tools/list");
            card(h, m.over_budget, "over budget");
            h.push_str("</div>\n");
            let _ = writeln!(
                h,
                "<p class=\"note\">Per-tool costs are the manifest's <code>schemaTokens</code> ({}); the list sizes are what the server sends before the first call (<code>tools/list</code> and the <code>initialize</code> instructions, the progressive ones carrying the cluster index), counted as the generated README counts them.</p>",
                escape(&m.manifest_counter)
            );
            let scale = m
                .tools
                .iter()
                .map(|t| t.tokens)
                .max()
                .unwrap_or(0)
                .max(budget);
            let _ = writeln!(h, "<h3>The {TOP_TOOLS} most expensive MCP tools</h3>");
            table(
                h,
                &["tool", "calls", "tokens#", ""],
                &tool_rows(&top(&m.tools), budget, scale),
            );
        }
    }
    h.push_str("</section>\n");
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

fn location(d: &JsonDiagnostic) -> String {
    let mut out = String::new();
    if let Some(file) = &d.file {
        out.push_str(&escape(file));
        if let Some(line) = d.line {
            let _ = write!(out, ":{line}");
            if let Some(col) = d.column {
                let _ = write!(out, ":{col}");
            }
        }
    }
    if let Some(pointer) = d.pointer.as_ref().filter(|p| !p.is_empty()) {
        let _ = write!(out, " <code>{}</code>", escape(pointer));
    }
    out
}

fn group(h: &mut String, g: &DiagnosticGroup) {
    let sev = severity_name(g.severity);
    let _ = write!(
        h,
        "<details><summary><span class=\"{sev}\">{sev}</span> <code>{}</code> ×{}",
        escape(&g.code),
        g.diagnostics.len()
    );
    if let Some(summary) = &g.summary {
        let _ = write!(h, " — {}", escape(summary));
    }
    h.push_str("</summary>\n<ul>\n");
    for d in &g.diagnostics {
        let _ = write!(h, "<li>{}", escape(&d.message));
        let loc = location(d);
        if !loc.is_empty() {
            let _ = write!(h, "<span class=\"src\">{loc}</span>");
        }
        if let Some(help) = &d.help {
            let _ = write!(h, "<span class=\"src\">help: {}</span>", escape(help));
        }
        h.push_str("</li>\n");
    }
    h.push_str("</ul></details>\n");
}

fn diagnostics(h: &mut String, r: &ReportResult) {
    h.push_str("<section id=\"diagnostics\">\n<h2>Diagnostics</h2>\n");
    if r.diagnostic_groups.is_empty() {
        h.push_str("<p class=\"ok\">No diagnostics.</p>\n");
    }
    for g in &r.diagnostic_groups {
        group(h, g);
    }
    h.push_str("<p class=\"note\"><code>tungsten explain &lt;code&gt;</code> describes each code and how to fix it.</p>\n</section>\n");
}

fn target_changes(h: &mut String, t: &TargetDiff) {
    let _ = writeln!(
        h,
        "<h3><code>{}</code></h3>\n<p>{}</p>",
        escape(&t.target),
        escape(&describe(t))
    );
    if let Some(s) = &t.semver {
        let _ = writeln!(h, "<p>API surface: {}</p>", escape(&describe_semver(s)));
        if !s.changes.is_empty() {
            let rows: Vec<Vec<String>> = s
                .changes
                .iter()
                .map(|c| {
                    vec![
                        escape(level_name(c.level)),
                        format!("<code>{}</code>", escape(&c.rule)),
                        format!("<code>{}</code>", escape(&c.subject)),
                        escape(&c.detail),
                    ]
                })
                .collect();
            table(h, &["level", "rule", "subject", "detail"], &rows);
        }
    }
    if !t.files.is_empty() {
        let _ = writeln!(
            h,
            "<details><summary>{}</summary>",
            plural(t.files.len(), "file", "files")
        );
        let rows: Vec<Vec<String>> = t
            .files
            .iter()
            .map(|f| {
                vec![
                    escape(change_name(f.change)),
                    format!("<code>{}</code>", escape(&f.path)),
                    format!("+{}", f.lines_added),
                    format!("−{}", f.lines_removed),
                ]
            })
            .collect();
        table(h, &["change", "file", "added#", "removed#"], &rows);
        h.push_str("</details>\n");
    }
}

fn changes(h: &mut String, r: &ReportResult) {
    h.push_str("<section id=\"changes\">\n<h2>Changes since the last generation</h2>\n");
    h.push_str("<p class=\"note\">Each configured target's directory (its <code>.tungsten/manifest.json</code> and files) compared with what <code>tungsten generate</code> would write now; <code>tungsten diff</code> shows the line changes.</p>\n");
    if r.changes.is_empty() {
        h.push_str("<p class=\"note\">No configured targets.</p>\n");
    }
    for t in &r.changes {
        target_changes(h, t);
    }
    h.push_str("</section>\n");
}
