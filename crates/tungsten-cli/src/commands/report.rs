// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten report`: the generation report (planning/07 "Generation
//! report"): coverage, the safety matrix with the source of every value,
//! token budgets, diagnostics grouped by code and the changes since the
//! last generation.
//!
//! Every emitter of this version runs in memory: the configured targets
//! with their options, the others with defaults so that budgets are known
//! before a target is enabled (only the configured targets' diagnostics
//! are reported). Nothing but the `--html` file is written. The data is
//! deterministic: no timestamps, paths as given on the command line.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};
use tungsten_agent::{AgentConfig, AgentManifest};
use tungsten_build::Compiled;
use tungsten_core::diagnostic::codes;
use tungsten_core::{Diagnostic, Severity, SourceMap};
use tungsten_emit::FileSet;
use tungsten_ir::{IdempotencyKind, Ir, Operation, OperationStatus, PreviewMode, Resource, Safety};
use tungsten_tokens::{Counter, count};

use crate::args::ReportArgs;
use crate::commands::check;
use crate::commands::diff::{self, DiffOptions};
use crate::commands::generate::{Emitted, Project, emit_target};
use crate::input::{self, Input};
use crate::output::{
    Budgets, CellOrigin, CliError, CommandName, CommandResult, Coverage, CoverageStatus,
    DiagnosticGroup, DocumentBudget, ErrorKind, HistogramBucket, McpBudgets, NamespaceCoverage,
    ReportResult, ReportSummary, SafetyCell, SafetyRow, TargetCoverage, ToolBudgets, ToolCost,
    ToolKind, json_diagnostic,
};
use crate::stats::{headline, ir_stats, plural};
use crate::{Report, exit, html, targets};

/// The counter of every budget in the report.
const COUNTER: Counter = Counter::Estimate;

/// Width of a histogram bucket, and where the open last bucket starts.
const BUCKET: usize = 100;
const LAST_BUCKET: usize = 600;

pub(crate) fn run(args: &ReportArgs) -> Report {
    let mut compiled = input::compile(&args.input.path);
    let mut report = Report::new(CommandName::Report);
    let sources = std::mem::take(&mut compiled.workspace.sources);
    let mut diagnostics = compiled.diagnostics.0.clone();
    let project = Project::of(&args.input.path);
    let mut data = collect(&compiled, &project, &args.input.path, &mut diagnostics);
    data.diagnostic_groups = groups(&diagnostics, &sources);
    data.summary.counts = check::count(&diagnostics);
    report.exit = if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        exit::FAILED
    } else {
        exit::OK
    };
    report.diagnostics = diagnostics;
    report.sources = sources;
    if let Some(path) = &args.html {
        data.html = Some(path.display().to_string());
        if let Err(e) = write_html(path, &html::render(&data)) {
            return report.failed(
                exit::INTERNAL,
                CliError::new(
                    ErrorKind::Io,
                    format!("cannot write {}: {e}", path.display()),
                ),
            );
        }
    }
    let path = args.input.path.display().to_string();
    report.human = human(&path, compiled.workspace.documents.len(), &data);
    report.result = Some(CommandResult::Report(Box::new(data)));
    report
}

fn write_html(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

/// Everything but the diagnostic groups, which need the final list.
fn collect(
    compiled: &Compiled,
    project: &Project,
    input: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> ReportResult {
    let ir = compiled
        .ir
        .as_ref()
        .filter(|_| !diagnostics.iter().any(|d| d.severity == Severity::Error));
    let configured: Vec<String> = compiled
        .config
        .as_ref()
        .map(|c| c.targets.keys().cloned().collect())
        .unwrap_or_default();
    let mut data = ReportResult {
        summary: ReportSummary {
            title: compiled.ir.as_ref().map(|ir| ir.api.title.clone()),
            api_version: compiled.ir.as_ref().map(|ir| ir.api.version.clone()),
            documents: compiled.workspace.documents.len(),
            stats: compiled.ir.as_ref().map(ir_stats),
            counts: Default::default(),
        },
        coverage: Coverage {
            namespaces: compiled.ir.as_ref().map(namespaces).unwrap_or_default(),
            targets: vec![],
        },
        safety: vec![],
        budgets: budgets_shell(compiled.ir.as_ref()),
        diagnostic_groups: vec![],
        changes: vec![],
        html: None,
    };
    let Some(ir) = ir else {
        data.coverage.targets = target_names(&configured)
            .into_iter()
            .map(|name| failed_coverage(&name, &configured))
            .collect();
        return data;
    };
    let agent = agent_manifest(compiled, input, project);
    data.safety = safety_rows(ir, agent.as_ref());
    let mut runs: BTreeMap<String, Emitted> = BTreeMap::new();
    for name in target_names(&configured) {
        let emitted = emit_target(compiled, ir, project, &name);
        if configured.contains(&name) {
            diagnostics.extend(emitted.diagnostics.0.iter().cloned());
        }
        runs.insert(name, emitted);
    }
    data.coverage.targets = runs
        .iter()
        .map(|(name, e)| coverage(ir, name, e, &configured))
        .collect();
    if let Some(files) = runs
        .get("docs")
        .filter(|e| !e.failed())
        .and_then(|e| e.files.as_ref())
    {
        data.budgets.documents = documents(files);
        data.budgets.tools = tool_budgets(files, data.budgets.schema_budget);
    }
    if let Some(files) = runs
        .get("mcp")
        .filter(|e| !e.failed())
        .and_then(|e| e.files.as_ref())
    {
        data.budgets.mcp = mcp_budgets(files, data.budgets.schema_budget);
    }
    let opts = DiffOptions {
        patches: false,
        semver: true,
    };
    // `runs` holds every configured target (`target_names` includes them).
    for (name, emitted) in configured.iter().filter_map(|n| Some((n, runs.get(n)?))) {
        data.changes.push(diff::target_diff(
            name,
            emitted,
            ir,
            project,
            opts,
            diagnostics,
        ));
    }
    data
}

/// Every known target, then configured names this version does not know.
fn target_names(configured: &[String]) -> Vec<String> {
    let mut names: Vec<String> = tungsten_config::KNOWN_TARGETS
        .iter()
        .map(|s| s.to_string())
        .collect();
    for name in configured {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

fn failed_coverage(name: &str, configured: &[String]) -> TargetCoverage {
    let emitter = targets::emitter(name).is_some();
    TargetCoverage {
        target: name.to_string(),
        configured: configured.iter().any(|c| c == name),
        emitter,
        status: if emitter {
            CoverageStatus::Failed
        } else {
            CoverageStatus::NoEmitter
        },
        operations: 0,
        macros: 0,
        files: 0,
        warnings: 0,
        errors: 0,
    }
}

fn namespaces(ir: &Ir) -> Vec<NamespaceCoverage> {
    fn walk(r: &Resource, c: &mut NamespaceCoverage) {
        for op in &r.operations {
            c.total += 1;
            match op.status {
                OperationStatus::Implemented => c.implemented += 1,
                OperationStatus::Planned { .. } => c.planned += 1,
                OperationStatus::Gated { .. } => c.gated += 1,
            }
        }
        r.children.iter().for_each(|child| walk(child, c));
    }
    ir.namespaces
        .iter()
        .map(|ns| {
            let mut c = NamespaceCoverage {
                namespace: ns.name.wire.clone(),
                implemented: 0,
                planned: ns.planned.len(),
                gated: 0,
                total: ns.planned.len(),
            };
            ns.resources.iter().for_each(|r| walk(r, &mut c));
            c
        })
        .collect()
}

fn coverage(ir: &Ir, name: &str, e: &Emitted, configured: &[String]) -> TargetCoverage {
    let mut c = failed_coverage(name, configured);
    let Some(files) = &e.files else {
        return c;
    };
    c.files = files.len();
    c.warnings = e
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();
    c.errors = e
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    c.status = if e.failed() {
        CoverageStatus::Failed
    } else if files.is_empty() {
        CoverageStatus::NotGenerated
    } else {
        CoverageStatus::Generated
    };
    if c.status != CoverageStatus::Generated {
        return c;
    }
    let tools = match name {
        "docs" => tools_json(files).map(|t| t.into_iter().map(|(c, _)| c.kind).collect::<Vec<_>>()),
        "mcp" => mcp_manifest(files).map(|m| mcp_tools(&m).into_iter().map(|t| t.kind).collect()),
        _ => None,
    };
    match tools {
        Some(kinds) => {
            c.operations = kinds.iter().filter(|k| **k == ToolKind::Operation).count();
            c.macros = kinds.iter().filter(|k| **k == ToolKind::Macro).count();
        }
        None => {
            c.operations = ir.operations().len();
            c.macros = ir.agent.macros.len();
        }
    }
    c
}

// ── safety matrix ──────────────────────────────────────────────────────────

/// The agent manifest the compilation used, loaded again for its node
/// positions: the `agent` key of tungsten.yml, else `agent.yml` next to a
/// project manifest.
fn agent_manifest(compiled: &Compiled, input: &Path, project: &Project) -> Option<Agent> {
    let name = match compiled.config.as_ref().and_then(|c| c.agent.clone()) {
        Some(name) => name,
        None if matches!(input::resolve(input), Ok(Input::Project(_))) => {
            tungsten_agent::DEFAULT_AGENT_MANIFEST.to_string()
        }
        None => return None,
    };
    let path = input::join(&project.base_dir, &name);
    if !path.is_file() {
        return None;
    }
    let parsed = tungsten_agent::load_as(&path, &name);
    parsed.manifest.map(|manifest| Agent { name, manifest })
}

struct Agent {
    name: String,
    manifest: AgentManifest,
}

impl Agent {
    fn config(&self) -> &AgentConfig {
        self.manifest.config()
    }

    fn cell(&self, value: String, origin: CellOrigin, pointer: String) -> SafetyCell {
        SafetyCell {
            value,
            origin,
            file: Some(self.name.clone()),
            line: self.manifest.source().line_col(&pointer).map(|(l, _)| l),
            pointer: Some(pointer),
        }
    }

    /// The `tools` entry index for each callable operation it names: by id
    /// or by `<namespace>.<resource path>.<method>`; the first entry wins.
    fn tool_index(&self, ir: &Ir) -> BTreeMap<String, usize> {
        let mut aliases: BTreeMap<String, Option<String>> = BTreeMap::new();
        fn walk(prefix: &str, r: &Resource, aliases: &mut BTreeMap<String, Option<String>>) {
            let path = format!("{prefix}.{}", r.name.wire);
            for op in &r.operations {
                aliases
                    .entry(format!("{path}.{}", op.name.wire))
                    .and_modify(|v| *v = None)
                    .or_insert_with(|| Some(op.id.0.clone()));
            }
            r.children.iter().for_each(|c| walk(&path, c, aliases));
        }
        for ns in &ir.namespaces {
            ns.resources
                .iter()
                .for_each(|r| walk(&ns.name.wire, r, &mut aliases));
        }
        let callable: Vec<&str> = ir.operations().iter().map(|op| op.id.0.as_str()).collect();
        let mut out = BTreeMap::new();
        for (i, tool) in self.config().tools.iter().enumerate() {
            let id = if callable.contains(&tool.operation.as_str()) {
                Some(tool.operation.clone())
            } else {
                aliases.get(&tool.operation).cloned().flatten()
            };
            if let Some(id) = id {
                out.entry(id).or_insert(i);
            }
        }
        out
    }
}

fn snake<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn plain(value: String, origin: CellOrigin) -> SafetyCell {
    SafetyCell {
        value,
        origin,
        file: None,
        pointer: None,
        line: None,
    }
}

fn needs_confirmation(safety: Safety) -> bool {
    matches!(safety, Safety::Destructive | Safety::Irreversible)
}

fn safety_rows(ir: &Ir, agent: Option<&Agent>) -> Vec<SafetyRow> {
    let tools = agent.map(|a| a.tool_index(ir)).unwrap_or_default();
    let namespace_of = |id: &str| id.split('.').next().unwrap_or_default().to_string();
    ir.operations()
        .into_iter()
        .map(|op| {
            let tool = agent.zip(tools.get(&op.id.0).copied());
            let cells = Cells { op, agent, tool };
            SafetyRow {
                operation: op.id.0.clone(),
                namespace: namespace_of(&op.id.0),
                method: snake(&op.method),
                path: op.path.raw.clone(),
                gated: matches!(op.status, OperationStatus::Gated { .. }),
                tier: cells.tier(),
                idempotency: cells.idempotency(),
                preview: cells.preview(),
                confirmation: cells.confirmation(),
                verify: cells.verify(),
            }
        })
        .collect()
}

/// Where each safety value of one operation comes from, in the order the
/// agent transform applies them: a `tools` entry, an `x-agent-*`
/// extension, inference, the manifest's `defaults`, the built-in defaults.
struct Cells<'a> {
    op: &'a Operation,
    agent: Option<&'a Agent>,
    /// The agent manifest and the index of the operation's `tools` entry.
    tool: Option<(&'a Agent, usize)>,
}

impl Cells<'_> {
    /// The explicit source of `key`: the `tools` entry's `key`, else the
    /// operation's `x-agent-<extension>`.
    fn explicit(&self, value: &str, key: &str, extension: &str) -> Option<SafetyCell> {
        if let Some((agent, i)) = self.tool {
            let entry = agent
                .manifest
                .as_json()
                .pointer(&format!("/tools/{i}/{key}"));
            if entry.is_some_and(|v| !v.is_null()) {
                return Some(agent.cell(
                    value.to_string(),
                    CellOrigin::Tool,
                    format!("/tools/{i}/{key}"),
                ));
            }
        }
        let ext = format!("x-agent-{extension}");
        self.op.extensions.contains_key(&ext).then(|| SafetyCell {
            value: value.to_string(),
            origin: CellOrigin::Extension,
            file: Some(self.op.source.file.clone()),
            pointer: Some(tungsten_config::pointer::child(
                &self.op.source.pointer,
                &ext,
            )),
            line: None,
        })
    }

    /// The manifest's `defaults` node at `pointer`, when it is set.
    fn defaults(&self, value: &str, pointer: &str) -> Option<SafetyCell> {
        let agent = self.agent?;
        let node = agent.manifest.as_json().pointer(pointer)?;
        (!node.is_null())
            .then(|| agent.cell(value.to_string(), CellOrigin::Defaults, pointer.to_string()))
    }

    fn read_only(&self) -> bool {
        self.op.agent.safety == Safety::ReadOnly
    }

    fn tier(&self) -> SafetyCell {
        let value = snake(&self.op.agent.safety);
        let method = snake(&self.op.method).to_ascii_lowercase();
        self.explicit(&value, "safety", "safety")
            .or_else(|| self.defaults(&value, &format!("/defaults/safety/{method}")))
            .unwrap_or_else(|| plain(value, CellOrigin::BuiltIn))
    }

    fn idempotency(&self) -> SafetyCell {
        let policy = &self.op.agent.idempotency;
        let mut value = snake(&policy.policy);
        let details: Vec<&str> = policy
            .header
            .as_deref()
            .into_iter()
            .chain(policy.format.as_deref())
            .chain(policy.persist_required.then_some("key required"))
            .collect();
        if !details.is_empty() {
            let _ = write!(value, " ({})", details.join(", "));
        }
        if let Some(cell) = self.explicit(&value, "idempotency", "idempotency") {
            return cell;
        }
        if self.read_only() {
            return plain(value, CellOrigin::BuiltIn);
        }
        let default_policy = self
            .agent
            .and_then(|a| a.config().defaults.idempotency.as_ref())
            .map(|d| d.policy().policy);
        if policy.policy == IdempotencyKind::CallerOwned
            && default_policy != Some(IdempotencyKind::CallerOwned)
        {
            return plain(value, CellOrigin::Inferred);
        }
        self.defaults(&value, "/defaults/idempotency")
            .unwrap_or_else(|| plain(value, CellOrigin::BuiltIn))
    }

    fn preview(&self) -> SafetyCell {
        let value = match &self.op.agent.preview {
            PreviewMode::Local => "local".to_string(),
            PreviewMode::Header { header, value } => format!("header {header}: {value}"),
            PreviewMode::Endpoint { operation } => format!("endpoint {}", operation.0),
            PreviewMode::None => "none".to_string(),
        };
        if let Some(cell) = self.explicit(&value, "preview", "preview") {
            return cell;
        }
        if self.read_only() {
            return plain(value, CellOrigin::BuiltIn);
        }
        self.defaults(&value, "/defaults/preview")
            .unwrap_or_else(|| plain(value, CellOrigin::BuiltIn))
    }

    fn confirmation(&self) -> SafetyCell {
        let needed = needs_confirmation(self.op.agent.safety);
        let mut value = if needed { "required" } else { "not required" }.to_string();
        if let Some(c) = &self.op.agent.confirmation
            && !c.summary_fields.is_empty()
        {
            let _ = write!(value, " (summary: {})", c.summary_fields.join(", "));
        }
        self.explicit(&value, "confirmation", "confirmation")
            .unwrap_or_else(|| plain(value, CellOrigin::Inferred))
    }

    fn verify(&self) -> SafetyCell {
        let value = match &self.op.agent.verify {
            Some(hook) if hook.poll_budget_ms.is_some() => format!("{} (polls)", hook.operation.0),
            Some(hook) => hook.operation.0.clone(),
            None => "none".to_string(),
        };
        self.explicit(&value, "verify", "verify")
            .unwrap_or_else(|| plain(value, CellOrigin::BuiltIn))
    }
}

// ── budgets ────────────────────────────────────────────────────────────────

fn budgets_shell(ir: Option<&Ir>) -> Budgets {
    let d = ir.map(|ir| ir.agent.disclosure.clone()).unwrap_or_default();
    Budgets {
        counter: COUNTER.id().to_string(),
        schema_budget: d.schema_budget_tokens,
        description_budget: d.description_budget_tokens,
        threshold: d.threshold,
        documents: vec![],
        tools: None,
        mcp: None,
    }
}

/// The docs files agents read, in a fixed order.
const AGENT_DOCUMENTS: [&str; 3] = ["llms.txt", "llms-full.txt", "tools.json"];

fn documents(files: &FileSet) -> Vec<DocumentBudget> {
    AGENT_DOCUMENTS
        .iter()
        .filter_map(|name| {
            let bytes = files.get(name)?;
            Some(DocumentBudget {
                file: name.to_string(),
                bytes: bytes.len(),
                tokens: count(&String::from_utf8_lossy(bytes), COUNTER),
            })
        })
        .collect()
}

fn parse_json(files: &FileSet, path: &str) -> Option<Value> {
    serde_json::from_slice(files.get(path)?).ok()
}

/// The tools of `tools.json`, each as compact JSON of name, description
/// and parameters, as a function-calling API receives it.
fn tools_json(files: &FileSet) -> Option<Vec<(ToolCost, Value)>> {
    let doc = parse_json(files, "tools.json")?;
    let tools = doc.get("tools")?.as_array()?;
    Some(
        tools
            .iter()
            .map(|t| {
                let meta = t.get("x-tungsten");
                let (kind, target) = match meta.and_then(|m| m.get("macro")).and_then(Value::as_str)
                {
                    Some(name) => (ToolKind::Macro, name),
                    None => (
                        ToolKind::Operation,
                        meta.and_then(|m| m.get("operation"))
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                };
                let sent = json!({
                    "name": t.get("name"),
                    "description": t.get("description"),
                    "parameters": t.get("parameters"),
                });
                let cost = ToolCost {
                    name: str_of(t, "name"),
                    target: target.to_string(),
                    kind,
                    tokens: count(&sent.to_string(), COUNTER),
                };
                (cost, sent)
            })
            .collect(),
    )
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn tool_budgets(files: &FileSet, budget: u32) -> Option<ToolBudgets> {
    let tools = tools_json(files)?;
    let list = Value::Array(tools.iter().map(|(_, sent)| sent.clone()).collect());
    let costs: Vec<ToolCost> = tools.into_iter().map(|(cost, _)| cost).collect();
    Some(ToolBudgets {
        list_tokens: count(&list.to_string(), COUNTER),
        largest: costs.iter().map(|t| t.tokens).max().unwrap_or(0),
        median: median(&costs),
        over_budget: costs.iter().filter(|t| t.tokens > budget as usize).count(),
        histogram: histogram(&costs),
        tools: costs,
    })
}

/// The lower median of the tool costs.
fn median(tools: &[ToolCost]) -> usize {
    let mut costs: Vec<usize> = tools.iter().map(|t| t.tokens).collect();
    costs.sort_unstable();
    costs
        .get(costs.len().saturating_sub(1) / 2)
        .copied()
        .unwrap_or(0)
}

fn histogram(tools: &[ToolCost]) -> Vec<HistogramBucket> {
    let mut buckets: Vec<HistogramBucket> = (0..LAST_BUCKET / BUCKET)
        .map(|i| HistogramBucket {
            min: i * BUCKET,
            max: Some((i + 1) * BUCKET - 1),
            tools: 0,
        })
        .collect();
    buckets.push(HistogramBucket {
        min: LAST_BUCKET,
        max: None,
        tools: 0,
    });
    for t in tools {
        let i = (t.tokens / BUCKET).min(buckets.len() - 1);
        buckets[i].tools += 1;
    }
    buckets
}

/// The MCP manifest (`McpManifest`, runtimes/mcp/src/types.ts) in the MCP
/// target's files: the shortest-path `manifest.json` outside `.tungsten/`
/// with `manifestVersion` 1 and a `tools` array.
fn mcp_manifest(files: &FileSet) -> Option<Value> {
    let mut candidates: Vec<&str> = files
        .iter()
        .map(|(path, _)| path)
        .filter(|p| {
            (*p == "manifest.json" || p.ends_with("/manifest.json")) && !p.starts_with(".tungsten/")
        })
        .collect();
    candidates.sort_by_key(|p| (p.len(), p.to_string()));
    candidates.into_iter().find_map(|path| {
        parse_json(files, path).filter(|m| {
            m.get("manifestVersion") == Some(&json!(1))
                && m.get("tools").is_some_and(Value::is_array)
        })
    })
}

fn mcp_tools(manifest: &Value) -> Vec<ToolCost> {
    manifest
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .map(|t| ToolCost {
                    name: str_of(t, "name"),
                    target: str_of(t, "target"),
                    kind: if t.get("kind").and_then(Value::as_str) == Some("macro") {
                        ToolKind::Macro
                    } else {
                        ToolKind::Operation
                    },
                    tokens: t
                        .get("schemaTokens")
                        .and_then(Value::as_u64)
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn mcp_budgets(files: &FileSet, budget: u32) -> Option<McpBudgets> {
    let manifest = mcp_manifest(files)?;
    let tools = manifest.get("tools").and_then(Value::as_array)?;
    let discrete: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.get("name"),
                "description": t.get("description"),
                "inputSchema": t.get("inputSchema"),
                "annotations": t.get("annotations"),
            })
        })
        .collect();
    let clusters: Vec<Value> = manifest
        .get("clusters")
        .and_then(Value::as_array)
        .map(|cs| {
            cs.iter()
                .map(|c| json!({ "name": c.get("name"), "summary": c.get("summary"), "tools": c.get("tools") }))
                .collect()
        })
        .unwrap_or_default();
    let index = json!({ "instructions": manifest.get("instructions"), "clusters": clusters });
    let costs = mcp_tools(&manifest);
    Some(McpBudgets {
        mode: str_of(&manifest, "mode"),
        threshold: manifest
            .get("threshold")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        manifest_counter: str_of(&manifest, "tokenCounter"),
        discrete_tokens: count(&Value::Array(discrete).to_string(), COUNTER),
        index_tokens: count(&index.to_string(), COUNTER),
        over_budget: costs.iter().filter(|t| t.tokens > budget as usize).count(),
        tools: costs,
    })
}

// ── diagnostics ────────────────────────────────────────────────────────────

fn groups(diagnostics: &[Diagnostic], sources: &SourceMap) -> Vec<DiagnosticGroup> {
    let mut by_code: BTreeMap<&str, Vec<&Diagnostic>> = BTreeMap::new();
    for d in diagnostics {
        by_code.entry(d.code.as_str()).or_default().push(d);
    }
    by_code
        .into_iter()
        .map(|(code, list)| DiagnosticGroup {
            code: code.to_string(),
            severity: list
                .iter()
                .map(|d| d.severity)
                .min()
                .unwrap_or(Severity::Info),
            summary: codes::describe(code).map(str::to_string),
            diagnostics: list.iter().map(|d| json_diagnostic(d, sources)).collect(),
        })
        .collect()
}

// ── human output ───────────────────────────────────────────────────────────

fn human(path: &str, documents: usize, r: &ReportResult) -> String {
    let mut out = headline(path, documents, r.summary.stats.as_ref());
    out.push('\n');
    let row = |out: &mut String, label: &str, text: String| {
        let _ = writeln!(out, "  {label:<12}{text}");
    };
    for ns in &r.coverage.namespaces {
        row(
            &mut out,
            "namespace",
            format!(
                "{} · {} ({} implemented, {} planned, {} gated)",
                ns.namespace,
                plural(ns.total, "operation", "operations"),
                ns.implemented,
                ns.planned,
                ns.gated
            ),
        );
    }
    let targets: Vec<String> = r
        .coverage
        .targets
        .iter()
        .map(|t| {
            let state = match t.status {
                CoverageStatus::Generated => format!("{} ops", t.operations),
                CoverageStatus::NotGenerated => "not generated".into(),
                CoverageStatus::NoEmitter => "no emitter".into(),
                CoverageStatus::Failed => "failed".into(),
            };
            format!("{} {state}", t.target)
        })
        .collect();
    row(&mut out, "targets", targets.join(" · "));
    let mut tiers: BTreeMap<String, usize> = BTreeMap::new();
    for s in &r.safety {
        *tiers.entry(s.tier.value.clone()).or_default() += 1;
    }
    let tiers: Vec<String> = tiers.iter().map(|(t, n)| format!("{n} {t}")).collect();
    row(&mut out, "safety", tiers.join(" · "));
    let b = &r.budgets;
    match &b.tools {
        Some(t) => row(
            &mut out,
            "tools.json",
            format!(
                "{} · largest {} · median {} · list {} tok · {} over {}",
                plural(t.tools.len(), "tool", "tools"),
                t.largest,
                t.median,
                t.list_tokens,
                t.over_budget,
                b.schema_budget
            ),
        ),
        None => row(&mut out, "tools.json", "not generated".into()),
    }
    match &b.mcp {
        Some(m) => row(
            &mut out,
            "mcp",
            format!(
                "{} · {} · index {} tok · discrete list {} tok · {} over {}",
                m.mode,
                plural(m.tools.len(), "tool", "tools"),
                m.index_tokens,
                m.discrete_tokens,
                m.over_budget,
                b.schema_budget
            ),
        ),
        None => row(&mut out, "mcp", "not generated".into()),
    }
    let codes: Vec<String> = r
        .diagnostic_groups
        .iter()
        .map(|g| format!("{} ×{}", g.code, g.diagnostics.len()))
        .collect();
    if !codes.is_empty() {
        row(&mut out, "diagnostics", codes.join(" · "));
    }
    for t in &r.changes {
        let mut line = diff::describe(t);
        if let Some(s) = &t.semver {
            let _ = write!(line, " · semver {}", diff::describe_semver(s));
        }
        row(&mut out, &t.target, line);
    }
    if let Some(html) = &r.html {
        row(&mut out, "wrote", html.clone());
    }
    out
}
