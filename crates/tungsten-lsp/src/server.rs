// SPDX-License-Identifier: AGPL-3.0-only
//! The message loop: documents, debounced compilation and requests.
//!
//! The server serves every project of its workspace folders (bounded
//! discovery, see `tungsten_config::workspace`), or the one `--config`
//! names. Each project has its own deadline, generation and last analysis;
//! an edit moves the deadline of the projects that read the edited file
//! only, so editing one project never dirties another while a shared input
//! dirties every project that reads it. One compilation runs at a time for
//! the whole workspace, on its own thread, over the text of the open
//! buffers; when it ends, the project whose deadline passed first starts
//! next. A result that predates a later edit of its project is not
//! published: the superseding compilation starts right after it. The
//! diagnostics of all projects are merged per file and only files whose
//! diagnostics changed are published. Requests never wait for a
//! compilation: they are answered from the buffer text and the last index
//! of the project that owns the document.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, never, select};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    CodeActionKind, CodeActionOptions, CodeActionParams, CodeActionProviderCapability,
    CompletionOptions, CompletionParams, CompletionResponse, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidChangeWatchedFilesRegistrationOptions,
    DidChangeWorkspaceFoldersParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse, FileSystemWatcher,
    GlobPattern, GotoDefinitionParams, GotoDefinitionResponse, HoverParams,
    HoverProviderCapability, InitializeParams, OneOf, PublishDiagnosticsParams, Registration,
    RegistrationParams, SaveOptions, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, TextDocumentSyncSaveOptions, Uri,
    WorkspaceFoldersServerCapabilities, WorkspaceServerCapabilities,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tungsten_openapi::CancellationToken;

use crate::actions::{self, Buffer};
use crate::analysis::{Analysis, Index, analyze};
use crate::complete::{Inputs, complete};
use crate::docs::Kind;
use crate::hover;
use crate::overlay_actions::{self, EditSupport};
use crate::position::LineIndex;
use crate::schema::Schema;
use crate::symbols::symbols;
use crate::uri::{absolute, normalize, path_to_uri, uri_to_path};

/// How the server is started.
#[derive(Debug, Clone)]
pub struct Options {
    /// The manifest to serve (`--config`): a file, or a directory holding
    /// `tungsten.yml`. Default: the manifest of the workspace folder.
    pub config: Option<PathBuf>,
    /// Quiet time after an edit before the project is compiled again.
    pub debounce: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            config: None,
            debounce: Duration::from_millis(250),
        }
    }
}

/// An open buffer.
struct Doc {
    version: i32,
    text: String,
    lines: LineIndex,
}

impl Doc {
    fn new(text: String, version: i32) -> Self {
        let lines = LineIndex::new(&text);
        Self {
            text,
            lines,
            version,
        }
    }
}

/// The result of one check: a new analysis (`None` when it failed), or
/// the previous one still holds because no input changed.
enum Checked {
    Fresh(Option<Box<Analysis>>),
    Unchanged,
}

/// A finished check: the manifest of its project, the project generation
/// it was started for, and its result.
type Done = (PathBuf, u64, Checked);

/// One served project, keyed by its normalized manifest path.
#[derive(Default)]
struct Project {
    /// The last published analysis.
    analysis: Option<Analysis>,
    /// The generation `analysis` was computed for.
    analysis_generation: u64,
    /// The index of the last compilation that built the IR.
    index: Option<Index>,
    /// Bumped by every change that can alter this project's result.
    generation: u64,
    deadline: Option<Instant>,
}

impl Project {
    /// Whether the project's last analysis read `path` or depends on its
    /// absence.
    fn reads(&self, manifest: &Path, path: &Path) -> bool {
        if manifest == path {
            return true;
        }
        let Some(a) = &self.analysis else {
            return false;
        };
        a.agent_file == path
            || a.inputs.contains(path)
            || a.overlays.iter().any(|o| o == path)
            || a.profiles.iter().any(|p| p == path)
            || a.inputs_key
                .as_ref()
                .is_some_and(|k| k.paths().any(|p| p == path))
    }

    /// Whether the project's next check may depend on files it has not
    /// read yet: it was never checked, or its last check had errors (a
    /// missing input appearing must be noticed).
    fn open_ended(&self) -> bool {
        self.analysis
            .as_ref()
            .is_none_or(|a| a.inputs_key.is_none())
    }
}

struct Server<'c> {
    conn: &'c Connection,
    opts: Options,
    manifest_schema: Schema,
    agent_schema: Schema,
    profile_schema: Schema,
    /// The editor's workspace folders, in the order the client gave them.
    folders: Vec<PathBuf>,
    /// Manifests served because they (or an agent manifest next to them)
    /// were opened, outside what discovery found.
    adopted: BTreeSet<PathBuf>,
    /// The served projects by manifest; iteration order is the order work
    /// is scheduled in on ties.
    projects: BTreeMap<PathBuf, Project>,
    /// Discovery findings (TG0640-TG0643) by file.
    discovery: BTreeMap<PathBuf, Vec<lsp_types::Diagnostic>>,
    /// How the client accepts watcher registrations, when it does.
    watch: Option<WatchSupport>,
    /// Directories of profiles outside the workspace folders that a
    /// watcher is registered for.
    watched_profiles: BTreeSet<PathBuf>,
    /// Bumped by every registration request, for unique request ids.
    registrations: u64,
    docs: BTreeMap<PathBuf, Doc>,
    /// The diagnostics last sent per file.
    published: BTreeMap<PathBuf, Vec<lsp_types::Diagnostic>>,
    /// The project being checked and how to cancel it.
    running: Option<(PathBuf, CancellationToken)>,
    done_tx: Sender<Done>,
    shutdown: bool,
    /// What workspace edits the client can apply.
    edits: EditSupport,
    /// Whether the client shows disabled code actions with their reason.
    disabled_actions: bool,
}

impl Drop for Server<'_> {
    fn drop(&mut self) {
        if let Some((_, cancellation)) = &self.running {
            cancellation.cancel();
        }
    }
}

/// Serve the language server protocol on the process's stdin and stdout.
/// Returns the process exit code: 0 after `shutdown` and `exit`, 1 when the
/// client left without them, 3 when the protocol broke.
pub fn run_stdio(options: Options) -> i32 {
    let (connection, threads) = Connection::stdio();
    let code = match serve(&connection, options) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("tungsten lsp: {message}");
            3
        }
    };
    drop(connection);
    let _ = threads.join();
    code
}

/// Serve one connection until `exit` or until the client disconnects.
pub fn serve(connection: &Connection, options: Options) -> Result<i32, String> {
    let (id, params) = connection
        .initialize_start()
        .map_err(|e| format!("initialize failed: {e}"))?;
    let params: InitializeParams = serde_json::from_value(params).unwrap_or_default();
    let result = serde_json::json!({
        "capabilities": capabilities(),
        "serverInfo": {"name": "tungsten", "version": env!("CARGO_PKG_VERSION")},
    });
    connection
        .initialize_finish(id, result)
        .map_err(|e| format!("initialize failed: {e}"))?;
    let (done_tx, done_rx) = crossbeam_channel::unbounded();
    let mut server = Server {
        conn: connection,
        opts: options,
        manifest_schema: Schema::new(tungsten_config::TungstenConfig::json_schema()),
        agent_schema: Schema::new(tungsten_agent::json_schema()),
        profile_schema: Schema::embedding(tungsten_config::profiles::json_schema()),
        folders: folders(&params),
        adopted: BTreeSet::new(),
        projects: BTreeMap::new(),
        discovery: BTreeMap::new(),
        watch: watch_support(&params),
        watched_profiles: BTreeSet::new(),
        registrations: 0,
        docs: BTreeMap::new(),
        published: BTreeMap::new(),
        running: None,
        done_tx,
        shutdown: false,
        edits: edit_support(&params),
        disabled_actions: params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|t| t.code_action.as_ref())
            .and_then(|c| c.disabled_support)
            .unwrap_or(false),
    };
    server.watch_files()?;
    server.refresh_projects()?;
    server.run(&done_rx)
}

/// The workspace folders of the session: every folder, else the root.
#[allow(deprecated)]
fn folders(params: &InitializeParams) -> Vec<PathBuf> {
    let listed: Vec<PathBuf> = params
        .workspace_folders
        .iter()
        .flatten()
        .filter_map(|f| uri_to_path(&f.uri))
        .collect();
    if !listed.is_empty() {
        return listed;
    }
    params
        .root_uri
        .as_ref()
        .and_then(uri_to_path)
        .or_else(|| params.root_path.as_ref().map(|p| normalize(Path::new(p))))
        .into_iter()
        .collect()
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                    include_text: Some(false),
                })),
                ..TextDocumentSyncOptions::default()
            },
        )),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![" ".to_string()]),
            ..CompletionOptions::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
            ..CodeActionOptions::default()
        })),
        workspace: Some(WorkspaceServerCapabilities {
            workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                supported: Some(true),
                change_notifications: Some(OneOf::Left(true)),
            }),
            file_operations: None,
        }),
        ..ServerCapabilities::default()
    }
}

/// Largest file read from disk to compute an edit of it.
const MAX_EDITED_FILE: u64 = 4 * 1024 * 1024;

fn edit_support(params: &InitializeParams) -> EditSupport {
    let edit = params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|w| w.workspace_edit.as_ref());
    let Some(edit) = edit else {
        return EditSupport::default();
    };
    EditSupport {
        document_changes: edit.document_changes.unwrap_or(false),
        create_files: edit
            .resource_operations
            .as_ref()
            .is_some_and(|ops| ops.contains(&lsp_types::ResourceOperationKind::Create)),
        annotations: edit.change_annotation_support.is_some(),
    }
}

/// Watcher registration the client supports.
#[derive(Debug, Clone, Copy)]
struct WatchSupport {
    /// Patterns relative to a base folder (LSP 3.17), so directories
    /// outside the workspace folder can be watched.
    relative: bool,
}

fn watch_support(params: &InitializeParams) -> Option<WatchSupport> {
    let caps = params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|w| w.did_change_watched_files.as_ref())?;
    caps.dynamic_registration
        .unwrap_or(false)
        .then_some(WatchSupport {
            relative: caps.relative_pattern_support.unwrap_or(false),
        })
}

const PROFILE_WATCHERS: &str = "tungsten-profile-watchers";

const MANIFEST_NAMES: [&str; 2] = tungsten_config::workspace::MANIFEST_NAMES;

fn manifest_in(dir: &Path) -> Option<PathBuf> {
    MANIFEST_NAMES
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

fn is_manifest(path: &Path) -> bool {
    MANIFEST_NAMES.contains(&file_name(path).as_str())
}

/// Most projects served at once, across all workspace folders.
const MAX_PROJECTS: usize = tungsten_config::workspace::DEFAULT_LIMITS.max_projects;

fn is_yaml(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "yml" || e == "yaml")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl Server<'_> {
    /// The manifests to serve: `--config`; else every project discovery
    /// finds in the workspace folders (bounded), with the nearest
    /// `tungsten.yml` above a folder that holds none; plus the manifests
    /// adopted by opening them. Discovery findings are kept for
    /// publishing.
    fn wanted(&mut self) -> BTreeSet<PathBuf> {
        self.discovery.clear();
        if let Some(config) = &self.opts.config {
            let path = absolute(config);
            let manifest = if path.is_dir() {
                manifest_in(&path).unwrap_or_else(|| path.join(MANIFEST_NAMES[0]))
            } else {
                path
            };
            return BTreeSet::from([normalize(&manifest)]);
        }
        let mut wanted = BTreeSet::new();
        for folder in self.folders.clone() {
            let catalog = tungsten_config::workspace::discover(
                &folder,
                tungsten_config::workspace::DEFAULT_LIMITS,
            );
            for diagnostic in catalog.diagnostics.iter() {
                self.discovered(&catalog.root, diagnostic);
            }
            if catalog.projects.is_empty() {
                // A folder inside a project: the manifest above it.
                wanted.extend(
                    folder
                        .ancestors()
                        .skip(1)
                        .find_map(manifest_in)
                        .map(|m| normalize(&m)),
                );
            }
            wanted.extend(catalog.projects.iter().map(|p| normalize(&p.manifest_path)));
        }
        self.adopted
            .retain(|m| m.is_file() || self.docs.contains_key(m));
        wanted.extend(self.adopted.iter().cloned());
        if wanted.len() > MAX_PROJECTS {
            self.log(&format!(
                "serving the first {MAX_PROJECTS} of {} projects; open fewer workspace folders to serve the rest",
                wanted.len()
            ));
            wanted = wanted.into_iter().take(MAX_PROJECTS).collect();
        }
        wanted
    }

    /// Keep a discovery finding as an editor diagnostic at the start of
    /// the file it names.
    fn discovered(&mut self, root: &Path, diagnostic: &tungsten_core::Diagnostic) {
        let Some(label) = diagnostic.labels.first() else {
            return;
        };
        let file = normalize(&root.join(&label.file));
        if !file.is_file() {
            // A limit reached in a directory: no file to show it on.
            self.log(&format!("{}: {}", diagnostic.code, diagnostic.message));
            return;
        }
        let severity = match diagnostic.severity {
            tungsten_core::Severity::Error => lsp_types::DiagnosticSeverity::ERROR,
            tungsten_core::Severity::Warning => lsp_types::DiagnosticSeverity::WARNING,
            _ => lsp_types::DiagnosticSeverity::INFORMATION,
        };
        let message = match &diagnostic.help {
            Some(help) => format!("{}\nhelp: {help}", diagnostic.message),
            None => diagnostic.message.clone(),
        };
        self.discovery
            .entry(file)
            .or_default()
            .push(lsp_types::Diagnostic {
                range: lsp_types::Range::default(),
                severity: Some(severity),
                code: Some(lsp_types::NumberOrString::String(diagnostic.code.clone())),
                source: Some("tungsten".into()),
                message,
                ..lsp_types::Diagnostic::default()
            });
    }

    /// Bring the served projects in line with [`Server::wanted`]: a
    /// project no longer wanted has its running check cancelled, its
    /// results dropped and its diagnostics cleared; a new one is checked.
    fn refresh_projects(&mut self) -> Result<(), String> {
        let wanted = self.wanted();
        let gone: Vec<PathBuf> = self
            .projects
            .keys()
            .filter(|m| !wanted.contains(*m))
            .cloned()
            .collect();
        for manifest in &gone {
            self.projects.remove(manifest);
            if let Some((running, cancellation)) = &self.running
                && running == manifest
            {
                cancellation.cancel();
            }
            self.log(&format!("stopped serving {}", manifest.display()));
        }
        for manifest in wanted {
            if !self.projects.contains_key(&manifest) {
                self.projects.insert(manifest.clone(), Project::default());
                self.touch(&manifest, Duration::ZERO);
            }
        }
        self.publish()?;
        self.watch_profiles()
    }

    fn run(&mut self, done_rx: &Receiver<Done>) -> Result<i32, String> {
        loop {
            let next = self.projects.values().filter_map(|p| p.deadline).min();
            let timer = match next {
                Some(at) if self.running.is_none() => crossbeam_channel::at(at),
                _ => never(),
            };
            select! {
                recv(self.conn.receiver) -> msg => {
                    let Ok(msg) = msg else {
                        return Ok(if self.shutdown { 0 } else { 1 });
                    };
                    if let Some(code) = self.guarded(msg)? {
                        return Ok(code);
                    }
                }
                recv(done_rx) -> done => {
                    if let Ok((manifest, generation, checked)) = done {
                        self.finished(&manifest, generation, checked)?;
                    }
                }
                recv(timer) -> _ => self.start(),
            }
        }
    }

    /// Ask the client to tell us when files change on disk (specs and
    /// overlays edited elsewhere), when it can.
    fn watch_files(&mut self) -> Result<(), String> {
        if self.watch.is_none() {
            return Ok(());
        }
        let watchers = vec![FileSystemWatcher {
            glob_pattern: GlobPattern::String("**/*.{yml,yaml,json}".to_string()),
            kind: None,
        }];
        self.register_watchers("tungsten-watched-files", watchers)
    }

    fn register_watchers(
        &mut self,
        id: &str,
        watchers: Vec<FileSystemWatcher>,
    ) -> Result<(), String> {
        self.registrations += 1;
        let options = DidChangeWatchedFilesRegistrationOptions { watchers };
        let registration = Registration {
            id: id.to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: serde_json::to_value(options).ok(),
        };
        let request = Request::new(
            RequestId::from(format!("{id}-register-{}", self.registrations)),
            "client/registerCapability".to_string(),
            RegistrationParams {
                registrations: vec![registration],
            },
        );
        self.conn
            .sender
            .send(Message::Request(request))
            .map_err(|e| e.to_string())
    }

    /// Profiles may live outside the workspace folders (a shared directory
    /// next to several projects), where the workspace watcher does not
    /// reach. Watch their directories too, re-registering only when the set
    /// of directories changes.
    fn watch_profiles(&mut self) -> Result<(), String> {
        let Some(support) = self.watch else {
            return Ok(());
        };
        let roots: Vec<PathBuf> = self
            .folders
            .iter()
            .cloned()
            .chain(
                self.projects
                    .keys()
                    .filter_map(|m| m.parent().map(Path::to_path_buf)),
            )
            .collect();
        let dirs: BTreeSet<PathBuf> = self
            .projects
            .values()
            .filter_map(|p| p.analysis.as_ref())
            .flat_map(|a| a.profiles.iter())
            .filter(|p| !roots.iter().any(|r| p.starts_with(r)))
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .collect();
        if dirs == self.watched_profiles {
            return Ok(());
        }
        if !self.watched_profiles.is_empty() {
            self.registrations += 1;
            let request = Request::new(
                RequestId::from(format!(
                    "{PROFILE_WATCHERS}-unregister-{}",
                    self.registrations
                )),
                "client/unregisterCapability".to_string(),
                lsp_types::UnregistrationParams {
                    unregisterations: vec![lsp_types::Unregistration {
                        id: PROFILE_WATCHERS.to_string(),
                        method: "workspace/didChangeWatchedFiles".to_string(),
                    }],
                },
            );
            self.conn
                .sender
                .send(Message::Request(request))
                .map_err(|e| e.to_string())?;
        }
        let watchers: Vec<FileSystemWatcher> = dirs
            .iter()
            .filter_map(|dir| profile_watcher(dir, support))
            .collect();
        self.watched_profiles = dirs;
        if watchers.is_empty() {
            return Ok(());
        }
        self.register_watchers(PROFILE_WATCHERS, watchers)
    }

    /// [`Server::message`], with a panic in a handler turned into a log line
    /// and, for a request, an error response: one bad document must not end
    /// the editor's session.
    fn guarded(&mut self, msg: Message) -> Result<Option<i32>, String> {
        let id = match &msg {
            Message::Request(req) => Some(req.id.clone()),
            _ => None,
        };
        match catch_unwind(AssertUnwindSafe(|| self.message(msg))) {
            Ok(result) => result,
            Err(_) => {
                self.log("internal error while handling a message");
                if let Some(id) = id {
                    self.fail(id, ErrorCode::InternalError, "internal error")?;
                }
                Ok(None)
            }
        }
    }

    /// Ask for a check of the project of `manifest` after `delay` of
    /// quiet; edits keep pushing it. A running check of it is cancelled.
    fn touch(&mut self, manifest: &Path, delay: Duration) {
        if let Some((running, cancellation)) = &self.running
            && running == manifest
        {
            cancellation.cancel();
        }
        if let Some(project) = self.projects.get_mut(manifest) {
            project.generation += 1;
            project.deadline = Some(Instant::now() + delay);
        }
    }

    /// Touch every project a change of `path` can affect: the projects
    /// that read it, and the projects whose next check may read files they
    /// have not read yet when no project reads it or when it changed on
    /// disk (`disk`: opened, closed, saved or watched, as opposed to
    /// typed).
    fn changed(&mut self, path: &Path, delay: Duration, disk: bool) -> Result<(), String> {
        let readers: BTreeSet<PathBuf> = self
            .projects
            .iter()
            .filter(|(m, p)| p.reads(m, path))
            .map(|(m, _)| m.clone())
            .collect();
        let mut affected = readers.clone();
        if readers.is_empty() || disk {
            affected.extend(
                self.projects
                    .iter()
                    .filter(|(_, p)| p.open_ended())
                    .map(|(m, _)| m.clone()),
            );
        }
        for manifest in affected {
            self.touch(&manifest, delay);
        }
        Ok(())
    }

    /// Start the check of the project whose deadline passed first.
    fn start(&mut self) {
        let now = Instant::now();
        let due = self
            .projects
            .iter()
            .filter_map(|(m, p)| p.deadline.filter(|d| *d <= now).map(|d| (d, m.clone())))
            .min();
        let Some((_, manifest)) = due else {
            return;
        };
        let Some(project) = self.projects.get_mut(&manifest) else {
            return;
        };
        project.deadline = None;
        let generation = project.generation;
        let key = project.analysis.as_ref().and_then(|a| a.inputs_key.clone());
        let overrides: BTreeMap<PathBuf, String> = self
            .docs
            .iter()
            .map(|(p, d)| (p.clone(), d.text.clone()))
            .collect();
        let tx = self.done_tx.clone();
        let cancellation = CancellationToken::default();
        self.running = Some((manifest.clone(), cancellation.clone()));
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(|| {
                if key.is_some_and(|k| k.unchanged(&manifest, &overrides, &cancellation)) {
                    return Checked::Unchanged;
                }
                Checked::Fresh(analyze(&manifest, overrides, cancellation).map(Box::new))
            })
            .unwrap_or(Checked::Fresh(None));
            let _ = tx.send((manifest, generation, result));
        });
    }

    fn finished(
        &mut self,
        manifest: &Path,
        generation: u64,
        checked: Checked,
    ) -> Result<(), String> {
        self.running = None;
        if self.shutdown {
            return Ok(());
        }
        let Some(project) = self.projects.get_mut(manifest) else {
            // The project was removed while it was checked.
            return Ok(());
        };
        if generation != project.generation {
            return Ok(());
        }
        let analysis = match checked {
            Checked::Fresh(analysis) => analysis.map(|a| *a),
            Checked::Unchanged => {
                // The published diagnostics and the index still describe
                // these inputs; the analysis is current again.
                project.analysis_generation = generation;
                self.log(&format!(
                    "checked {}: inputs unchanged, kept the previous result",
                    shown(manifest)
                ));
                return Ok(());
            }
        };
        let Some(analysis) = analysis else {
            self.log(&format!(
                "the check of {} failed unexpectedly; see the server's stderr",
                shown(manifest)
            ));
            return Ok(());
        };
        if let Some(index) = &analysis.index {
            project.index = Some(index.clone());
        }
        let total: usize = analysis.diagnostics.values().map(Vec::len).sum();
        let files = analysis.diagnostics.len();
        project.analysis = Some(analysis);
        project.analysis_generation = generation;
        self.publish()?;
        self.watch_profiles()?;
        self.log(&format!(
            "checked {}: {total} diagnostics in {files} files",
            shown(manifest)
        ));
        Ok(())
    }

    fn log(&self, message: &str) {
        let params = lsp_types::LogMessageParams {
            typ: lsp_types::MessageType::LOG,
            message: message.to_string(),
        };
        let _ = self.notify("window/logMessage", params);
    }

    fn notify<P: Serialize>(&self, method: &str, params: P) -> Result<(), String> {
        let note = Notification::new(method.to_string(), params);
        self.conn
            .sender
            .send(Message::Notification(note))
            .map_err(|e| e.to_string())
    }

    /// Send the diagnostics of every project's latest analysis merged per
    /// file (a shared file gets each project's findings once), for the
    /// files whose diagnostics changed, and clear the files that had some
    /// and no longer do.
    fn publish(&mut self) -> Result<(), String> {
        let mut now: BTreeMap<PathBuf, Vec<lsp_types::Diagnostic>> = self.discovery.clone();
        for project in self.projects.values() {
            let Some(analysis) = &project.analysis else {
                continue;
            };
            for (path, list) in &analysis.diagnostics {
                now.entry(path.clone())
                    .or_default()
                    .extend(list.iter().cloned());
            }
        }
        for list in now.values_mut() {
            // The same finding reported by two projects reading one file
            // ends up adjacent and is kept once.
            list.sort_by(|a, b| {
                (a.range.start, a.range.end, &a.message).cmp(&(
                    b.range.start,
                    b.range.end,
                    &b.message,
                ))
            });
            list.dedup();
        }
        let mut sends: Vec<(PathBuf, Vec<lsp_types::Diagnostic>)> = vec![];
        for (path, list) in &now {
            if self.published.get(path) != Some(list) {
                sends.push((path.clone(), list.clone()));
            }
        }
        for path in self.published.keys() {
            if !now.contains_key(path) {
                sends.push((path.clone(), vec![]));
            }
        }
        self.published = now;
        for (path, diagnostics) in sends {
            let Some(uri) = path_to_uri(&path) else {
                continue;
            };
            let version = self.docs.get(&path).map(|doc| doc.version);
            self.notify(
                "textDocument/publishDiagnostics",
                PublishDiagnosticsParams {
                    uri,
                    diagnostics,
                    version,
                },
            )?;
        }
        Ok(())
    }

    /// Handle one message. `Some(code)` ends the loop.
    fn message(&mut self, msg: Message) -> Result<Option<i32>, String> {
        match msg {
            Message::Request(req) => {
                self.request(req)?;
                Ok(None)
            }
            Message::Notification(note) => self.notification(note),
            Message::Response(_) => Ok(None),
        }
    }

    fn notification(&mut self, note: Notification) -> Result<Option<i32>, String> {
        match note.method.as_str() {
            "exit" => return Ok(Some(if self.shutdown { 0 } else { 1 })),
            "textDocument/didOpen" => {
                if let Some(p) = parse::<DidOpenTextDocumentParams>(note.params)
                    && let Some(path) = uri_to_path(&p.text_document.uri)
                {
                    self.docs.insert(
                        path.clone(),
                        Doc::new(p.text_document.text, p.text_document.version),
                    );
                    self.adopt(&path)?;
                    self.changed(&path, Duration::ZERO, true)?;
                }
            }
            "textDocument/didChange" => {
                if let Some(p) = parse::<DidChangeTextDocumentParams>(note.params)
                    && let Some(path) = uri_to_path(&p.text_document.uri)
                    && self.change(&path, p.text_document.version, p.content_changes)
                {
                    self.changed(&path, self.opts.debounce, false)?;
                }
            }
            "textDocument/didClose" => {
                if let Some(p) = parse::<DidCloseTextDocumentParams>(note.params)
                    && let Some(path) = uri_to_path(&p.text_document.uri)
                    && self.docs.remove(&path).is_some()
                {
                    self.changed(&path, self.opts.debounce, true)?;
                }
            }
            "textDocument/didSave" => {
                if let Some(p) = parse::<DidSaveTextDocumentParams>(note.params)
                    && let Some(path) = uri_to_path(&p.text_document.uri)
                {
                    self.changed(&path, Duration::ZERO, true)?;
                }
            }
            "workspace/didChangeWatchedFiles" => {
                if let Some(p) = parse::<DidChangeWatchedFilesParams>(note.params) {
                    let paths: BTreeSet<PathBuf> = p
                        .changes
                        .iter()
                        .filter_map(|c| uri_to_path(&c.uri))
                        .collect();
                    let manifests = paths.iter().any(|p| is_manifest(p));
                    for path in &paths {
                        self.changed(path, Duration::ZERO, true)?;
                    }
                    if manifests {
                        // A manifest created or deleted on disk.
                        self.refresh_projects()?;
                    }
                }
            }
            "workspace/didChangeWorkspaceFolders" => {
                if let Some(p) = parse::<DidChangeWorkspaceFoldersParams>(note.params) {
                    let removed: Vec<PathBuf> = p
                        .event
                        .removed
                        .iter()
                        .filter_map(|f| uri_to_path(&f.uri))
                        .collect();
                    self.folders.retain(|f| !removed.contains(f));
                    for folder in p.event.added.iter().filter_map(|f| uri_to_path(&f.uri)) {
                        if !self.folders.contains(&folder) {
                            self.folders.push(folder);
                        }
                    }
                    // Adopted manifests go with their folder.
                    self.adopted
                        .retain(|m| !removed.iter().any(|r| m.starts_with(r)));
                    self.refresh_projects()?;
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// Without `--config`, a manifest that is opened (or the manifest next
    /// to an opened agent manifest) is served even when discovery did not
    /// find it.
    fn adopt(&mut self, path: &Path) -> Result<(), String> {
        if self.opts.config.is_some() {
            return Ok(());
        }
        let name = file_name(path);
        let manifest = if MANIFEST_NAMES.contains(&name.as_str()) {
            Some(path.to_path_buf())
        } else if name.starts_with("agent") && is_yaml(path) {
            path.parent().and_then(manifest_in).map(|m| normalize(&m))
        } else {
            None
        };
        let Some(manifest) = manifest else {
            return Ok(());
        };
        if self.projects.contains_key(&manifest) || self.adopted.contains(&manifest) {
            return Ok(());
        }
        self.adopted.insert(manifest);
        self.refresh_projects()
    }

    fn change(
        &mut self,
        path: &Path,
        version: i32,
        changes: Vec<lsp_types::TextDocumentContentChangeEvent>,
    ) -> bool {
        let Some(doc) = self.docs.get_mut(path) else {
            return false;
        };
        if version <= doc.version {
            return false;
        }
        for change in changes {
            match change.range {
                None => *doc = Doc::new(change.text, version),
                Some(range) => {
                    let start = doc.lines.offset(range.start, &doc.text);
                    let end = doc.lines.offset(range.end, &doc.text).max(start);
                    doc.text.replace_range(start..end, &change.text);
                    doc.lines = LineIndex::new(&doc.text);
                }
            }
        }
        doc.version = version;
        true
    }

    /// The project that answers requests about `path`: the project whose
    /// manifest it is, else the first project (in manifest order) that
    /// read it, else the innermost project whose directory holds it.
    fn owner(&self, path: &Path) -> Option<(&PathBuf, &Project)> {
        if let Some(found) = self.projects.get_key_value(path) {
            return Some(found);
        }
        self.projects
            .iter()
            .find(|(m, p)| p.reads(m, path))
            .or_else(|| {
                self.projects
                    .iter()
                    .filter(|(m, _)| m.parent().is_some_and(|d| path.starts_with(d)))
                    .max_by_key(|(m, _)| m.components().count())
            })
    }

    fn kind_of(&self, path: &Path) -> Option<Kind> {
        if self.projects.contains_key(path) {
            return Some(Kind::Manifest);
        }
        for a in self.projects.values().filter_map(|p| p.analysis.as_ref()) {
            if a.agent_file == path {
                return Some(Kind::Agent);
            }
            if a.overlays.iter().any(|o| o == path) {
                return Some(Kind::Overlay);
            }
            if a.profiles.iter().any(|p| p == path) {
                return Some(Kind::Profile);
            }
        }
        // A profile not yet selected by the manifest, or not yet compiled.
        if is_yaml(path)
            && self
                .docs
                .get(path)
                .is_some_and(|doc| declares_profile(&doc.text))
        {
            return Some(Kind::Profile);
        }
        let name = file_name(path);
        if !is_yaml(path) {
            None
        } else if name.starts_with("tungsten") {
            Some(Kind::Manifest)
        } else if name.starts_with("agent") {
            Some(Kind::Agent)
        } else if name.contains("overlay") {
            Some(Kind::Overlay)
        } else {
            None
        }
    }

    fn reply<R: Serialize>(&self, id: RequestId, result: R) -> Result<(), String> {
        self.send(Response::new_ok(id, result))
    }

    fn fail(&self, id: RequestId, code: ErrorCode, message: &str) -> Result<(), String> {
        self.send(Response::new_err(id, code as i32, message.to_string()))
    }

    fn send(&self, response: Response) -> Result<(), String> {
        self.conn
            .sender
            .send(Message::Response(response))
            .map_err(|e| e.to_string())
    }

    fn request(&mut self, req: Request) -> Result<(), String> {
        if self.shutdown {
            return self.fail(
                req.id,
                ErrorCode::InvalidRequest,
                "the server is shutting down",
            );
        }
        let id = req.id.clone();
        match req.method.as_str() {
            "shutdown" => {
                self.shutdown = true;
                if let Some((_, cancellation)) = &self.running {
                    cancellation.cancel();
                }
                self.reply(id, serde_json::Value::Null)
            }
            "textDocument/completion" => match parse::<CompletionParams>(req.params) {
                Some(p) => {
                    let pos = p.text_document_position;
                    let items =
                        self.with_doc(&pos.text_document.uri, pos.position, |inputs, offset| {
                            complete(inputs, offset)
                        });
                    self.reply(id, items.map(CompletionResponse::Array))
                }
                None => self.fail(
                    id,
                    ErrorCode::InvalidParams,
                    "invalid completion parameters",
                ),
            },
            "textDocument/hover" => match parse::<HoverParams>(req.params) {
                Some(p) => {
                    let pos = p.text_document_position_params;
                    let found = self
                        .with_doc(&pos.text_document.uri, pos.position, |i, o| {
                            hover::hover(i, o)
                        })
                        .flatten();
                    self.reply(id, found)
                }
                None => self.fail(id, ErrorCode::InvalidParams, "invalid hover parameters"),
            },
            "textDocument/definition" => match parse::<GotoDefinitionParams>(req.params) {
                Some(p) => {
                    let pos = p.text_document_position_params;
                    let found = self
                        .with_doc(&pos.text_document.uri, pos.position, |i, o| {
                            hover::definition(i, i.index?, o)
                        })
                        .flatten();
                    self.reply(id, found.map(GotoDefinitionResponse::Scalar))
                }
                None => self.fail(
                    id,
                    ErrorCode::InvalidParams,
                    "invalid definition parameters",
                ),
            },
            "textDocument/documentSymbol" => match parse::<DocumentSymbolParams>(req.params) {
                Some(p) => {
                    let doc =
                        uri_to_path(&p.text_document.uri).and_then(|path| self.docs.get(&path));
                    let list =
                        doc.map(|d| DocumentSymbolResponse::Nested(symbols(&d.text, &d.lines)));
                    self.reply(id, list)
                }
                None => self.fail(id, ErrorCode::InvalidParams, "invalid symbol parameters"),
            },
            "textDocument/codeAction" => match parse::<CodeActionParams>(req.params) {
                Some(p) => {
                    let wants_fixes = p.context.only.as_ref().is_none_or(|only| {
                        only.iter()
                            .any(|k| CodeActionKind::QUICKFIX.as_str().starts_with(k.as_str()))
                    });
                    let uri = &p.text_document.uri;
                    let path = uri_to_path(uri);
                    let doc = path.as_ref().and_then(|path| self.docs.get(path));
                    let index = path
                        .as_deref()
                        .and_then(|path| self.owner(path))
                        .and_then(|(_, p)| p.index.as_ref());
                    let mut list = match doc {
                        Some(d) if wants_fixes => {
                            let buffer = Buffer {
                                uri,
                                version: d.version,
                                document_changes: self.edits.document_changes,
                                text: &d.text,
                                lines: &d.lines,
                            };
                            actions::actions(&buffer, &p.context.diagnostics, index)
                        }
                        _ => vec![],
                    };
                    if wants_fixes {
                        let overlays = self.overlay_fixes(uri, &p.context.diagnostics);
                        if self.disabled_actions
                            && let Some(why) = actions::unavailable_overlay(
                                &p.context.diagnostics,
                                &overlays,
                                || self.overlay_unavailable(uri),
                            )
                        {
                            list.push(why);
                        }
                        list.extend(overlays);
                        if self.disabled_actions {
                            list.extend(actions::unsupported_inputs(&p.context.diagnostics));
                        }
                    }
                    self.reply(id, list)
                }
                None => self.fail(
                    id,
                    ErrorCode::InvalidParams,
                    "invalid code action parameters",
                ),
            },
            _ => self.fail(id, ErrorCode::MethodNotFound, "method not supported"),
        }
    }

    /// The overlay quick fixes for diagnostics of the file at `uri`, from
    /// the latest analysis that is still current.
    fn overlay_fixes(
        &self,
        uri: &Uri,
        diagnostics: &[lsp_types::Diagnostic],
    ) -> Vec<lsp_types::CodeActionOrCommand> {
        let Some(path) = uri_to_path(uri) else {
            return vec![];
        };
        let Some((manifest, project)) = self.owner(&path) else {
            return vec![];
        };
        let Some(analysis) = &project.analysis else {
            return vec![];
        };
        // An edit since the analysis may have fixed or moved the
        // diagnostics; the next analysis offers the fixes again.
        if project.analysis_generation != project.generation {
            return vec![];
        }
        let read = |path: &Path| self.read(path);
        overlay_actions::actions(
            &path,
            diagnostics,
            &analysis.overlay_fixes,
            manifest,
            self.edits,
            &read,
        )
    }

    /// Why the overlay fixes of the file at `uri` were not offered.
    fn overlay_unavailable(&self, uri: &Uri) -> String {
        const COMMAND: &str = "run `tungsten overlay suggest --namespace <namespace> --write <file>` and list the file under `overlays` of the input in tungsten.yml";
        let path = uri_to_path(uri);
        let owner = path.as_deref().and_then(|p| self.owner(p));
        let stale = owner.is_some_and(|(m, p)| {
            p.analysis_generation != p.generation
                || self.running.as_ref().is_some_and(|(r, _)| r == m)
        });
        if stale {
            return "the project is being checked again; retry when its diagnostics update".into();
        }
        if !self.edits.document_changes {
            return format!("the editor does not take versioned workspace edits; {COMMAND}");
        }
        let derived = owner
            .and_then(|(_, p)| p.analysis.as_ref())
            .is_some_and(|a| {
                a.overlay_fixes
                    .iter()
                    .any(|f| f.fixes.iter().any(|(p, _, _)| Some(p) == path.as_ref()))
            });
        if !derived {
            return "no overlay action derives a fix for this diagnostic from the current check; `tungsten overlay suggest` reports why".into();
        }
        if !self.edits.create_files {
            return format!(
                "the fix may need a new overlay file and the editor cannot create files from a quick fix; {COMMAND}"
            );
        }
        format!(
            "tungsten.yml or the suggested overlay does not have the shape a quick fix edits (for example a flow-style input or an unreadable file); {COMMAND}"
        )
    }

    /// The text of `path` as the editor has it: the open buffer and its
    /// version, else the file on disk (bounded).
    fn read(&self, path: &Path) -> Option<(String, Option<i32>)> {
        if let Some(doc) = self.docs.get(path) {
            return Some((doc.text.clone(), Some(doc.version)));
        }
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() || meta.len() > MAX_EDITED_FILE {
            return None;
        }
        std::fs::read_to_string(path).ok().map(|t| (t, None))
    }

    /// Run `f` on the open document at `uri` with the cursor offset.
    fn with_doc<T>(
        &self,
        uri: &Uri,
        position: lsp_types::Position,
        f: impl FnOnce(&Inputs<'_>, usize) -> T,
    ) -> Option<T> {
        let path = uri_to_path(uri)?;
        let doc = self.docs.get(&path)?;
        let inputs = Inputs {
            kind: self.kind_of(&path)?,
            text: &doc.text,
            lines: &doc.lines,
            index: self.owner(&path).and_then(|(_, p)| p.index.as_ref()),
            manifest: &self.manifest_schema,
            agent: &self.agent_schema,
            profile: &self.profile_schema,
        };
        let offset = doc.lines.offset(position, &doc.text);
        Some(f(&inputs, offset))
    }
}

/// A watcher for the YAML files of one profile directory.
fn profile_watcher(dir: &Path, support: WatchSupport) -> Option<FileSystemWatcher> {
    let pattern = "*.{yml,yaml}".to_string();
    let glob_pattern = if support.relative {
        GlobPattern::Relative(lsp_types::RelativePattern {
            base_uri: OneOf::Right(path_to_uri(dir)?),
            pattern,
        })
    } else {
        let dir = dir.to_str()?.replace('\\', "/");
        GlobPattern::String(format!("{}/{pattern}", dir.trim_end_matches('/')))
    };
    Some(FileSystemWatcher {
        glob_pattern,
        kind: None,
    })
}

/// Whether `text` has a top-level `tungsten_profile` key, as every profile
/// must.
fn declares_profile(text: &str) -> bool {
    text.lines().any(|line| {
        line.strip_prefix("tungsten_profile")
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
    })
}

fn parse<T: DeserializeOwned>(params: serde_json::Value) -> Option<T> {
    serde_json::from_value(params).ok()
}

/// A manifest path for log lines: its directory and file name.
fn shown(manifest: &Path) -> String {
    let dir = manifest
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned());
    match dir {
        Some(dir) => format!("{dir}/{}", file_name(manifest)),
        None => file_name(manifest),
    }
}
