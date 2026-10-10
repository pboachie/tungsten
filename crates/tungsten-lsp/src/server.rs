// SPDX-License-Identifier: AGPL-3.0-only
//! The message loop: documents, debounced compilation and requests.
//!
//! One compilation runs at a time, on its own thread, over the text of the
//! open buffers. Edits only move a deadline; when it passes and no
//! compilation is running a new one starts, so a burst of keystrokes costs
//! one compilation. A result that predates a later edit is not published
//! (its index is still used for completion): the superseding compilation
//! starts right after it. Requests never wait for a compilation: they are
//! answered from the buffer text and the last index.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, never, select};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    CompletionOptions, CompletionParams, CompletionResponse, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DocumentSymbolParams,
    DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse, HoverParams,
    HoverProviderCapability, InitializeParams, OneOf, PublishDiagnosticsParams, SaveOptions,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions, Uri,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::analysis::{Analysis, Index, analyze};
use crate::complete::{Inputs, complete};
use crate::docs::Kind;
use crate::hover;
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
    text: String,
    lines: LineIndex,
}

impl Doc {
    fn new(text: String) -> Self {
        let lines = LineIndex::new(&text);
        Self { text, lines }
    }
}

type Done = (u64, Option<Analysis>);

struct Server<'c> {
    conn: &'c Connection,
    opts: Options,
    manifest_schema: Schema,
    agent_schema: Schema,
    manifest: Option<PathBuf>,
    docs: BTreeMap<PathBuf, Doc>,
    analysis: Option<Analysis>,
    /// The index of the last compilation that built the IR.
    index: Option<Index>,
    published: BTreeSet<PathBuf>,
    /// Bumped by every change that can alter the result.
    generation: u64,
    deadline: Option<Instant>,
    running: bool,
    done_tx: Sender<Done>,
    shutdown: bool,
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
        manifest: None,
        opts: options,
        manifest_schema: Schema::new(tungsten_config::TungstenConfig::json_schema()),
        agent_schema: Schema::new(tungsten_agent::json_schema()),
        docs: BTreeMap::new(),
        analysis: None,
        index: None,
        published: BTreeSet::new(),
        generation: 0,
        deadline: None,
        running: false,
        done_tx,
        shutdown: false,
    };
    server.manifest = server.discover(&params);
    if server.manifest.is_some() {
        server.touch(Duration::ZERO);
    }
    server.run(&done_rx)
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
        ..ServerCapabilities::default()
    }
}

const MANIFEST_NAMES: [&str; 2] = ["tungsten.yml", "tungsten.yaml"];

fn manifest_in(dir: &Path) -> Option<PathBuf> {
    MANIFEST_NAMES
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

fn is_yaml(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "yml" || e == "yaml")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl Server<'_> {
    /// The manifest of the workspace: `--config`, else the nearest
    /// `tungsten.yml` at or above the workspace folder.
    fn discover(&self, params: &InitializeParams) -> Option<PathBuf> {
        if let Some(config) = &self.opts.config {
            let path = absolute(config);
            return if path.is_dir() {
                manifest_in(&path).or(Some(path.join(MANIFEST_NAMES[0])))
            } else {
                Some(path)
            };
        }
        #[allow(deprecated)]
        let root = params
            .workspace_folders
            .as_ref()
            .and_then(|f| f.first())
            .and_then(|f| uri_to_path(&f.uri))
            .or_else(|| params.root_uri.as_ref().and_then(uri_to_path))
            .or_else(|| params.root_path.as_ref().map(|p| normalize(Path::new(p))))?;
        root.ancestors().find_map(manifest_in)
    }

    fn run(&mut self, done_rx: &Receiver<Done>) -> Result<i32, String> {
        loop {
            let timer = match self.deadline {
                Some(at) if !self.running => crossbeam_channel::at(at),
                _ => never(),
            };
            select! {
                recv(self.conn.receiver) -> msg => {
                    let Ok(msg) = msg else {
                        return Ok(if self.shutdown { 0 } else { 1 });
                    };
                    if let Some(code) = self.message(msg)? {
                        return Ok(code);
                    }
                }
                recv(done_rx) -> done => {
                    if let Ok((generation, analysis)) = done {
                        self.finished(generation, analysis)?;
                    }
                }
                recv(timer) -> _ => self.start(),
            }
        }
    }

    /// Ask for a compilation after `delay` of quiet; edits keep pushing it.
    fn touch(&mut self, delay: Duration) {
        self.generation += 1;
        self.deadline = Some(Instant::now() + delay);
    }

    fn start(&mut self) {
        self.deadline = None;
        let Some(manifest) = self.manifest.clone() else {
            return;
        };
        let overrides: BTreeMap<PathBuf, String> = self
            .docs
            .iter()
            .filter(|(p, _)| is_yaml(p))
            .map(|(p, d)| (p.clone(), d.text.clone()))
            .collect();
        let generation = self.generation;
        let tx = self.done_tx.clone();
        self.running = true;
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(|| analyze(&manifest, overrides)).ok();
            let _ = tx.send((generation, result));
        });
    }

    fn finished(&mut self, generation: u64, analysis: Option<Analysis>) -> Result<(), String> {
        self.running = false;
        let Some(analysis) = analysis else {
            self.log("the project check failed unexpectedly; see the server's stderr");
            return Ok(());
        };
        if let Some(index) = &analysis.index {
            self.index = Some(index.clone());
        }
        let current = generation == self.generation;
        self.analysis = Some(analysis);
        if current {
            self.publish()?;
            if let Some(a) = &self.analysis {
                let total: usize = a.diagnostics.values().map(Vec::len).sum();
                self.log(&format!(
                    "checked {}: {total} diagnostics in {} files",
                    file_name(&a.manifest),
                    a.diagnostics.len()
                ));
            }
        }
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

    /// Send the diagnostics of the latest analysis, and clear the files
    /// that had some and no longer do.
    fn publish(&mut self) -> Result<(), String> {
        let Some(analysis) = &self.analysis else {
            return Ok(());
        };
        let mut now = BTreeSet::new();
        let mut sends: Vec<(Uri, Vec<lsp_types::Diagnostic>)> = vec![];
        for (path, list) in &analysis.diagnostics {
            if let Some(uri) = path_to_uri(path) {
                now.insert(path.clone());
                sends.push((uri, list.clone()));
            }
        }
        for path in self.published.difference(&now) {
            if let Some(uri) = path_to_uri(path) {
                sends.push((uri, vec![]));
            }
        }
        self.published = now;
        for (uri, diagnostics) in sends {
            self.notify(
                "textDocument/publishDiagnostics",
                PublishDiagnosticsParams {
                    uri,
                    diagnostics,
                    version: None,
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
            Message::Notification(note) => Ok(self.notification(note)),
            Message::Response(_) => Ok(None),
        }
    }

    fn notification(&mut self, note: Notification) -> Option<i32> {
        match note.method.as_str() {
            "exit" => return Some(if self.shutdown { 0 } else { 1 }),
            "textDocument/didOpen" => {
                if let Some(p) = parse::<DidOpenTextDocumentParams>(note.params)
                    && let Some(path) = uri_to_path(&p.text_document.uri)
                {
                    self.adopt(&path);
                    self.docs.insert(path, Doc::new(p.text_document.text));
                    self.touch(Duration::ZERO);
                }
            }
            "textDocument/didChange" => {
                if let Some(p) = parse::<DidChangeTextDocumentParams>(note.params)
                    && let Some(path) = uri_to_path(&p.text_document.uri)
                {
                    self.change(&path, p.content_changes);
                    self.touch(self.opts.debounce);
                }
            }
            "textDocument/didClose" => {
                if let Some(p) = parse::<DidCloseTextDocumentParams>(note.params)
                    && let Some(path) = uri_to_path(&p.text_document.uri)
                    && self.docs.remove(&path).is_some()
                {
                    self.touch(self.opts.debounce);
                }
            }
            "textDocument/didSave" | "workspace/didChangeWatchedFiles" => {
                self.touch(Duration::ZERO);
            }
            _ => {}
        }
        None
    }

    /// Without a configured manifest, the first manifest (or the manifest
    /// next to the first agent manifest) that is opened becomes the
    /// workspace.
    fn adopt(&mut self, path: &Path) {
        if self.manifest.is_some() || self.opts.config.is_some() {
            return;
        }
        let name = file_name(path);
        if MANIFEST_NAMES.contains(&name.as_str()) {
            self.manifest = Some(path.to_path_buf());
        } else if name.starts_with("agent") && is_yaml(path) {
            self.manifest = path.parent().and_then(manifest_in);
        }
    }

    fn change(&mut self, path: &Path, changes: Vec<lsp_types::TextDocumentContentChangeEvent>) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        for change in changes {
            match change.range {
                None => *doc = Doc::new(change.text),
                Some(range) => {
                    let start = doc.lines.offset(range.start, &doc.text);
                    let end = doc.lines.offset(range.end, &doc.text).max(start);
                    doc.text.replace_range(start..end, &change.text);
                    doc.lines = LineIndex::new(&doc.text);
                }
            }
        }
    }

    fn kind_of(&self, path: &Path) -> Option<Kind> {
        let analysis = self.analysis.as_ref();
        if self.manifest.as_deref() == Some(path) {
            return Some(Kind::Manifest);
        }
        if let Some(a) = analysis {
            if a.agent_file == path {
                return Some(Kind::Agent);
            }
            if a.overlays.iter().any(|o| o == path) {
                return Some(Kind::Overlay);
            }
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
            _ => self.fail(id, ErrorCode::MethodNotFound, "method not supported"),
        }
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
            index: self.index.as_ref(),
            manifest: &self.manifest_schema,
            agent: &self.agent_schema,
        };
        let offset = doc.lines.offset(position, &doc.text);
        Some(f(&inputs, offset))
    }
}

fn parse<T: DeserializeOwned>(params: serde_json::Value) -> Option<T> {
    serde_json::from_value(params).ok()
}
