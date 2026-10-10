//! The language server: message loop, document store and request dispatch.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tree_sitter::Parser;

use crate::analysis::{self, Analysis};
use crate::document::Document;
use crate::features::{self, Config, Snapshot};
use crate::lsp::markup::ClientFormats;
use crate::lsp::transport::{Message, read_message, write_message};
use crate::lsp::types::*;
use crate::syntax;
use crate::text::PositionEncoding;
use crate::uri;
use crate::util::push_unique;
use crate::workspace::{self, FileIndex, Workspace};

enum Event {
    Message(Message),
    Indexed(Vec<FileIndex>, Option<String>),
    Eof,
}

struct OpenDocument {
    document: Document,
    analysis: Analysis,
    /// The diagnostics last published, with the document version they are for.
    diagnostics: Option<(i32, Vec<Diagnostic>)>,
    /// The loose project folder last indexed for the document.
    loose_dir: Option<PathBuf>,
}

struct RequestError {
    code: i64,
    message: String,
}

impl RequestError {
    fn invalid_params(message: impl Into<String>) -> RequestError {
        RequestError {
            code: error_code::INVALID_PARAMS,
            message: message.into(),
        }
    }
}

type Response = Result<Value, RequestError>;

pub struct Server<W: Write> {
    writer: W,
    events: Sender<Event>,
    parser: Parser,
    documents: HashMap<String, OpenDocument>,
    workspace: Workspace,
    config: Config,
    encoding: PositionEncoding,
    snippet_support: bool,
    formats: ClientFormats,
    watch_registration: bool,
    /// The client can be asked to re-request semantic tokens / inlay hints.
    refresh_semantic_tokens: bool,
    refresh_inlay_hints: bool,
    initialized: bool,
    shutdown_requested: bool,
    next_request_id: u64,
    /// Open documents edited but not yet re-analyzed.
    changed: Vec<String>,
    /// Folders of files opened outside every workspace folder, already searched for a schema.
    loose_dirs: HashSet<PathBuf>,
}

/// Runs the server until the client exits. Returns the process exit code.
pub fn run(reader: impl BufRead + Send + 'static, writer: impl Write) -> i32 {
    let (sender, receiver) = mpsc::channel();
    let reader_sender = sender.clone();
    thread::spawn(move || {
        let mut reader = reader;
        loop {
            match read_message(&mut reader) {
                Ok(Some(message)) => {
                    if reader_sender
                        .send(Event::Message(message))
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(None) => break,
                Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                    eprintln!("gsql-lsp: ignoring malformed message: {err}");
                }
                Err(err) => {
                    eprintln!(
                        "gsql-lsp: failed to read from the client: {err}"
                    );
                    break;
                }
            }
        }
        let _ = reader_sender.send(Event::Eof);
    });
    let mut server = Server::new(writer, sender);
    let mut stopping = false;
    while !stopping {
        let Ok(first) = receiver.recv() else { break };
        // Everything already waiting is handled in one go, so that a burst
        // of edits is analyzed and published once, not once per keystroke.
        let mut batch = vec![first];
        batch.extend(receiver.try_iter());
        for event in batch {
            match event {
                Event::Message(message) => {
                    if let Some(code) = server.handle(message) {
                        return code;
                    }
                }
                Event::Indexed(files, warning) => {
                    server.flush_changes();
                    server.on_indexed(files, warning);
                }
                Event::Eof => {
                    stopping = true;
                    break;
                }
            }
        }
        server.flush_changes();
    }
    if server.shutdown_requested { 0 } else { 1 }
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, RequestError> {
    serde_json::from_value(params)
        .map_err(|err| RequestError::invalid_params(err.to_string()))
}

fn to_value<T: serde::Serialize>(value: T) -> Response {
    Ok(serde_json::to_value(value).expect("protocol types serialize"))
}

impl<W: Write> Server<W> {
    fn new(writer: W, events: Sender<Event>) -> Server<W> {
        Server {
            writer,
            events,
            parser: syntax::new_parser(),
            documents: HashMap::new(),
            workspace: Workspace::default(),
            config: Config::default(),
            encoding: PositionEncoding::Utf16,
            snippet_support: false,
            formats: ClientFormats::default(),
            watch_registration: false,
            refresh_semantic_tokens: false,
            refresh_inlay_hints: false,
            initialized: false,
            shutdown_requested: false,
            next_request_id: 1,
            changed: Vec::new(),
            loose_dirs: HashSet::new(),
        }
    }

    fn send(&mut self, message: Message) {
        if let Err(err) = write_message(&mut self.writer, &message) {
            eprintln!("gsql-lsp: failed to write to the client: {err}");
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(Message::Notification {
            method: method.to_string(),
            params,
        });
    }

    fn request(&mut self, method: &str, params: Value) {
        let id = json!(format!("gsql-lsp-{}", self.next_request_id));
        self.next_request_id += 1;
        self.send(Message::Request {
            id,
            method: method.to_string(),
            params,
        });
    }

    /// Re-analyzes and republishes the documents edited since the last time.
    fn flush_changes(&mut self) {
        for uri in std::mem::take(&mut self.changed) {
            self.reanalyze(&uri);
        }
    }

    /// Handles one message; returns an exit code when the server should stop.
    fn handle(&mut self, message: Message) -> Option<i32> {
        // Edits are analyzed lazily (see `run`), but anything else sees them.
        let is_edit = matches!(&message, Message::Notification { method, .. } if method == "textDocument/didChange");
        if !is_edit {
            self.flush_changes();
        }
        match message {
            Message::Request { id, method, params } => {
                let result = if method == "initialize" {
                    if self.initialized {
                        Err(RequestError {
                            code: error_code::INVALID_REQUEST,
                            message: "the server is already initialized"
                                .into(),
                        })
                    } else {
                        self.initialize(params)
                    }
                } else if !self.initialized {
                    Err(RequestError {
                        code: error_code::SERVER_NOT_INITIALIZED,
                        message: "server not initialized".into(),
                    })
                } else if self.shutdown_requested {
                    Err(RequestError {
                        code: error_code::INVALID_REQUEST,
                        message: "server is shutting down".into(),
                    })
                } else {
                    match catch_unwind(AssertUnwindSafe(|| {
                        self.dispatch(&method, params)
                    })) {
                        Ok(result) => result,
                        Err(panic) => {
                            let detail = crate::panic_message(panic);
                            eprintln!(
                                "gsql-lsp: internal error handling {method}: {detail}"
                            );
                            Err(RequestError {
                                code: error_code::INTERNAL_ERROR,
                                message: format!("internal error: {detail}"),
                            })
                        }
                    }
                };
                let message = match result {
                    Ok(value) => Message::Response {
                        id,
                        result: Some(value),
                        error: None,
                    },
                    Err(err) => Message::Response {
                        id,
                        result: None,
                        error: Some(
                            json!({ "code": err.code, "message": err.message }),
                        ),
                    },
                };
                self.send(message);
                None
            }
            Message::Invalid { id, code, message } => {
                self.send(Message::Invalid { id, code, message });
                None
            }
            Message::Notification { method, params } => {
                if method == "exit" {
                    return Some(if self.shutdown_requested { 0 } else { 1 });
                }
                if self.initialized && !self.shutdown_requested {
                    let outcome = catch_unwind(AssertUnwindSafe(|| {
                        self.notification(&method, params)
                    }));
                    if outcome.is_err() {
                        eprintln!(
                            "gsql-lsp: internal error handling {method}"
                        );
                    }
                }
                None
            }
            // Responses to our registration requests need no handling.
            Message::Response { .. } => None,
        }
    }

    fn initialize(&mut self, params: Value) -> Response {
        let params: InitializeParams = parse(params)?;
        let capabilities = &params.capabilities;
        let offered: Vec<String> = capabilities
            .pointer("/general/positionEncodings")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        self.encoding = if offered.iter().any(|e| e == "utf-8") {
            PositionEncoding::Utf8
        } else {
            offered
                .iter()
                .find_map(|e| PositionEncoding::from_name(e))
                .filter(|e| *e != PositionEncoding::Utf32)
                .unwrap_or(PositionEncoding::Utf16)
        };
        self.snippet_support = capabilities
            .pointer("/textDocument/completion/completionItem/snippetSupport")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.formats = ClientFormats::from_capabilities(capabilities);
        let flag = |pointer: &str| {
            capabilities
                .pointer(pointer)
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        self.watch_registration =
            flag("/workspace/didChangeWatchedFiles/dynamicRegistration");
        self.refresh_semantic_tokens =
            flag("/workspace/semanticTokens/refreshSupport");
        self.refresh_inlay_hints =
            flag("/workspace/inlayHint/refreshSupport");
        if let Some(options) = &params.initialization_options {
            self.config.update(options);
        }
        let mut roots: Vec<PathBuf> = params
            .workspace_folders
            .unwrap_or_default()
            .iter()
            .filter_map(|f| uri::to_path(&f.uri))
            .collect();
        if roots.is_empty() {
            if let Some(root) = params
                .root_uri
                .as_deref()
                .and_then(uri::to_path)
            {
                roots.push(root);
            } else if let Some(path) = params.root_path {
                roots.push(PathBuf::from(path));
            }
        }
        self.workspace.roots = roots;
        self.initialized = true;
        Ok(json!({
            "capabilities": {
                "positionEncoding": self.encoding.as_str(),
                "textDocumentSync": {
                    "openClose": true,
                    "change": 2,
                    "save": { "includeText": false },
                },
                "completionProvider": {
                    "triggerCharacters": [".", "@", "<", "(", ":", "-"],
                    "resolveProvider": false,
                },
                "hoverProvider": true,
                "signatureHelpProvider": {
                    "triggerCharacters": ["(", ","],
                    "retriggerCharacters": [","],
                },
                "definitionProvider": true,
                "typeDefinitionProvider": true,
                "referencesProvider": true,
                "documentHighlightProvider": true,
                "documentSymbolProvider": true,
                "workspaceSymbolProvider": true,
                "codeActionProvider": { "codeActionKinds": ["quickfix", "source.fixAll"] },
                "documentFormattingProvider": true,
                "documentRangeFormattingProvider": true,
                "renameProvider": { "prepareProvider": true },
                "foldingRangeProvider": true,
                "selectionRangeProvider": true,
                "inlayHintProvider": true,
                "callHierarchyProvider": true,
                "documentLinkProvider": { "resolveProvider": false },
                "semanticTokensProvider": {
                    "legend": {
                        "tokenTypes": features::semantic_tokens::TOKEN_TYPES,
                        "tokenModifiers": features::semantic_tokens::TOKEN_MODIFIERS,
                    },
                    "full": true,
                    "range": true,
                },
                "workspace": {
                    "workspaceFolders": { "supported": true, "changeNotifications": true },
                },
            },
            "serverInfo": { "name": "gsql-lsp", "version": env!("CARGO_PKG_VERSION") },
        }))
    }

    fn notification(&mut self, method: &str, params: Value) {
        match method {
            "initialized" => {
                self.start_indexing(self.workspace.roots.clone());
                if self.watch_registration {
                    let watchers: Vec<Value> = workspace::EXTENSIONS
                        .iter()
                        .map(|ext| json!({ "globPattern": format!("**/*.{ext}") }))
                        .collect();
                    self.request(
                        "client/registerCapability",
                        json!({ "registrations": [{
                            "id": "gsql-lsp-watch",
                            "method": "workspace/didChangeWatchedFiles",
                            "registerOptions": { "watchers": watchers },
                        }]}),
                    );
                }
            }
            "textDocument/didOpen" => {
                let Ok(params) = parse::<DidOpenTextDocumentParams>(params)
                else {
                    return;
                };
                let item = params.text_document;
                let document = Document::new(
                    item.uri.clone(),
                    item.version,
                    item.text,
                    &mut self.parser,
                );
                self.documents.insert(
                    item.uri.clone(),
                    OpenDocument {
                        document,
                        analysis: Analysis::default(),
                        diagnostics: None,
                        loose_dir: None,
                    },
                );
                self.index_neighbours(&item.uri);
                self.reanalyze(&item.uri);
            }
            "textDocument/didChange" => {
                let Ok(params) = parse::<DidChangeTextDocumentParams>(params)
                else {
                    return;
                };
                let uri = params.text_document.uri;
                let Some(open) = self.documents.get_mut(&uri) else {
                    return;
                };
                open.document.apply_changes(
                    &params.content_changes,
                    params.text_document.version,
                    self.encoding,
                    &mut self.parser,
                );
                push_unique(&mut self.changed, uri);
            }
            "textDocument/didClose" => {
                let Ok(params) = parse::<TextDocumentParams>(params) else {
                    return;
                };
                let uri = params.text_document.uri;
                let before = self.declarations(&uri);
                self.documents.remove(&uri);
                // Every loose project, not just this file's: after a `.gsqlroot` came
                // or went, one may be left that no open file holds.
                let dirs: Vec<PathBuf> =
                    self.loose_dirs.iter().cloned().collect();
                let released = self.release_loose_projects(dirs);
                // Fall back to the disk if a workspace folder or loose project holds it.
                let path =
                    uri::to_path(&uri).filter(|p| self.indexed_from_disk(p));
                match path
                    .and_then(|p| workspace::index_file(&p, self.encoding))
                {
                    Some(index) => self.workspace.update(index),
                    None => self.workspace.remove(&uri),
                }
                self.notify(
                    "textDocument/publishDiagnostics",
                    json!({ "uri": uri, "diagnostics": [] }),
                );
                if released || self.declarations(&uri) != before {
                    self.publish_all();
                }
            }
            "workspace/didChangeWatchedFiles" => {
                let Ok(params) = parse::<DidChangeWatchedFilesParams>(params)
                else {
                    return;
                };
                let mut changed = false;
                let open = self.open_keys();
                for change in params.changes {
                    if open.contains(&uri::key(&change.uri)) {
                        continue;
                    }
                    let Some(path) = uri::to_path(&change.uri) else {
                        continue;
                    };
                    if !workspace::is_gsql_file(&path) {
                        continue;
                    }
                    match change.kind {
                        file_change::DELETED => {
                            self.workspace.remove(&change.uri)
                        }
                        // Only files of the workspace or of a loose project are indexed.
                        _ if !self.indexed_from_disk(&path) => {
                            continue;
                        }
                        _ => {
                            if let Some(index) =
                                workspace::index_file(&path, self.encoding)
                            {
                                self.workspace.update(index);
                            }
                        }
                    }
                    changed = true;
                }
                if changed {
                    self.publish_all();
                }
            }
            "workspace/didChangeConfiguration" => {
                if let Some(settings) = params.get("settings") {
                    self.config.update(settings);
                }
                self.publish_all();
            }
            "workspace/didChangeWorkspaceFolders" => {
                let Ok(params) =
                    parse::<DidChangeWorkspaceFoldersParams>(params)
                else {
                    return;
                };
                let removed: Vec<PathBuf> = params
                    .event
                    .removed
                    .iter()
                    .filter_map(|f| uri::to_path(&f.uri))
                    .collect();
                self.workspace
                    .roots
                    .retain(|r| !removed.contains(r));
                self.drop_unheld(&removed);
                let added: Vec<PathBuf> = params
                    .event
                    .added
                    .iter()
                    .filter_map(|f| uri::to_path(&f.uri))
                    .collect();
                self.workspace
                    .roots
                    .extend(added.iter().cloned());
                // An open document now outside every folder is a loose file.
                let uris: Vec<String> =
                    self.documents.keys().cloned().collect();
                for uri in &uris {
                    self.index_neighbours(uri);
                }
                self.start_indexing(added);
                self.publish_all();
            }
            _ => {}
        }
    }

    fn start_indexing(&mut self, roots: Vec<PathBuf>) {
        if roots.is_empty() {
            return;
        }
        self.workspace.start_indexing();
        let sender = self.events.clone();
        let encoding = self.encoding;
        let spawned = thread::Builder::new()
            .name("gsql-lsp-indexer".into())
            .stack_size(crate::STACK_SIZE)
            .spawn(move || {
                let report = workspace::scan_report(&roots);
                let indexes: Vec<FileIndex> = report
                    .files
                    .iter()
                    .filter_map(|p| workspace::index_file(p, encoding))
                    .collect();
                let _ =
                    sender.send(Event::Indexed(indexes, report.warning()));
            });
        if let Err(err) = spawned {
            self.workspace.finish_indexing();
            eprintln!(
                "gsql-lsp: could not start the workspace indexer: {err}"
            );
        }
    }

    /// A file outside every workspace folder (or opened with no folder at all)
    /// has no project to search for its schema: the nearest folder above it
    /// with a `.gsqlroot` file is indexed, else its own folder (a schema file
    /// next to it is found; subfolders are not searched).
    fn index_neighbours(&mut self, uri: &str) {
        let Some(path) = uri::to_path(uri) else {
            return;
        };
        if self.workspace.contains(&path) {
            return;
        }
        let Some(dir) = workspace::loose_dir(&path) else {
            return;
        };
        // A project already indexed is not scanned again.
        let (dir, files) = if self.loose_dirs.contains(&dir) {
            (dir, Vec::new())
        } else {
            let Some(project) = workspace::loose_project(&path) else {
                return;
            };
            project
        };
        let Some(document) = self.documents.get_mut(uri) else {
            return;
        };
        let previous = document.loose_dir.replace(dir.clone());
        if self.loose_dirs.insert(dir.clone()) {
            let open = self.open_keys();
            for file in files {
                if open.contains(&uri::key(&uri::from_path(&file))) {
                    continue;
                }
                if let Some(index) =
                    workspace::index_file(&file, self.encoding)
                {
                    self.workspace.update(index);
                }
            }
        }
        // A `.gsqlroot` came or went since: the document left its old project.
        if let Some(previous) = previous
            && previous != dir
        {
            self.release_loose_projects(vec![previous]);
        }
    }

    /// Whether the file is in a workspace folder or an indexed loose project.
    fn indexed_from_disk(&self, path: &Path) -> bool {
        // (Both tests read the disk: each is skipped when it cannot hold.)
        (!self.workspace.roots.is_empty() && self.workspace.contains(path))
            || (!self.loose_dirs.is_empty()
                && workspace::is_gsql_file(path)
                && self.in_loose_project(path))
    }

    /// Whether an indexed loose project has the file: it is in the project's
    /// folder, or anywhere below it when that folder has a `.gsqlroot`.
    fn in_loose_project(&self, path: &Path) -> bool {
        let path = uri::resolve_dots(path);
        path.parent().is_some_and(|d| self.loose_dirs.contains(d))
            // Each marked folder above the file, nearest first.
            || std::iter::successors(workspace::marked_root(&path), |d| {
                workspace::marked_root(d)
            })
            .any(|d| self.loose_dirs.contains(&d))
    }

    /// Drops the closed files under `dirs` that no workspace folder or loose project
    /// holds. Returns whether any was dropped.
    fn drop_unheld(&mut self, dirs: &[PathBuf]) -> bool {
        if dirs.is_empty() {
            return false;
        }
        let open = self.open_keys();
        let unheld: Vec<String> = self
            .workspace
            .files()
            .filter(|f| {
                // The cheap test first: the index may hold thousands of other files.
                let Some(path) = uri::to_path(&f.uri)
                    .filter(|p| dirs.iter().any(|d| p.starts_with(d)))
                else {
                    return false;
                };
                let is_open =
                    !open.is_empty() && open.contains(&uri::key(&f.uri));
                !is_open && !self.indexed_from_disk(&path)
            })
            .map(|f| f.uri.clone())
            .collect();
        for uri in &unheld {
            self.workspace.remove(uri);
        }
        !unheld.is_empty()
    }

    /// Drops the loose projects among `dirs` that no open loose file belongs to,
    /// by the folder indexed for it or by the one it is in now. Returns whether
    /// files left the index.
    fn release_loose_projects(&mut self, mut dirs: Vec<PathBuf>) -> bool {
        dirs.retain(|d| self.loose_dirs.contains(d));
        if dirs.is_empty() {
            return false;
        }
        let mut held = HashSet::new();
        for (u, open) in &self.documents {
            let Some(path) = uri::to_path(u) else {
                continue;
            };
            if !self.workspace.contains(&path) {
                held.extend(open.loose_dir.clone());
                held.extend(workspace::loose_dir(&path));
            }
        }
        dirs.retain(|d| !held.contains(d));
        for dir in &dirs {
            self.loose_dirs.remove(dir);
        }
        self.drop_unheld(&dirs)
    }

    /// Workspace keys of the open documents (their in-memory text wins over the disk).
    fn open_keys(&self) -> HashSet<String> {
        self.documents
            .keys()
            .map(|u| uri::key(u))
            .collect()
    }

    fn on_indexed(&mut self, files: Vec<FileIndex>, warning: Option<String>) {
        self.workspace.finish_indexing();
        if let Some(message) = warning {
            self.notify(
                "window/logMessage",
                json!({ "type": 2, "message": message }),
            );
        }
        // (A log message: editors list standard error output as errors.)
        self.notify(
            "window/logMessage",
            json!({ "type": 3, "message": format!("gsql-lsp: indexed {} GSQL files", files.len()) }),
        );
        let open = self.open_keys();
        for index in files {
            if !open.contains(&uri::key(&index.uri)) {
                self.workspace.update(index);
            }
        }
        self.publish_all();
    }

    /// The workspace-level declarations of a file, as indexed: when they
    /// change, other documents may need new diagnostics.
    fn declarations(&self, uri: &str) -> Vec<String> {
        self.workspace
            .file(uri)
            .map(|i| {
                // (An edge's ends decide the types of pattern aliases in other files.)
                i.symbols
                    .iter()
                    .map(|s| {
                        format!(
                            "{:?}|{}|{:?}|{:?}|{:?}|{:?}|{:?}",
                            s.kind,
                            s.name,
                            s.owner,
                            s.params,
                            s.ends,
                            s.vector,
                            s.returns
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Re-analyzes an open document, updates its workspace index entry and
    /// republishes diagnostics: for every open document when the file's
    /// workspace-level declarations changed.
    fn reanalyze(&mut self, uri: &str) {
        let before = self.declarations(uri);
        let Some(open) = self.documents.get_mut(uri) else {
            return;
        };
        let document = &open.document;
        open.analysis = analysis::Analysis::from_tree(
            &document.tree,
            document.text(),
            Some(&self.workspace),
        );
        let index = FileIndex::of_document(
            &document.uri,
            &document.tree,
            &open.analysis,
            &document.source,
            self.encoding,
            Some(&self.workspace),
        );
        self.workspace.update(index);
        if self.declarations(uri) != before {
            self.publish_all();
        } else {
            self.publish(uri);
        }
    }

    /// Republishes diagnostics for every open document and asks the client to
    /// refresh workspace-dependent results (the schema may have changed).
    fn publish_all(&mut self) {
        let uris: Vec<String> = self.documents.keys().cloned().collect();
        // Aliases take vertex types from the schema's edges: analyze again.
        for open in self.documents.values_mut() {
            let document = &open.document;
            open.analysis.schema_changed(
                &document.tree,
                document.text(),
                &self.workspace,
            );
        }
        for uri in uris {
            self.publish(&uri);
        }
        if self.documents.is_empty() {
            return;
        }
        if self.refresh_semantic_tokens {
            self.request("workspace/semanticTokens/refresh", Value::Null);
        }
        if self.refresh_inlay_hints {
            self.request("workspace/inlayHint/refresh", Value::Null);
        }
    }

    fn publish(&mut self, uri: &str) {
        let Ok(snapshot) = self.snapshot(uri) else {
            return;
        };
        let diagnostics = features::diagnostics::diagnostics(&snapshot);
        let Some(open) = self.documents.get_mut(uri) else {
            return;
        };
        let version = open.document.version;
        let params = json!({
            "uri": uri,
            "version": version,
            "diagnostics": diagnostics,
        });
        open.diagnostics = Some((version, diagnostics));
        self.notify("textDocument/publishDiagnostics", params);
    }

    fn snapshot<'a>(
        &'a self,
        uri: &'a str,
    ) -> Result<Snapshot<'a>, RequestError> {
        let open = self
            .documents
            .get(uri)
            .ok_or_else(|| RequestError {
                code: error_code::REQUEST_FAILED,
                message: format!("unknown document {uri}"),
            })?;
        Ok(Snapshot {
            uri,
            source: &open.document.source,
            tree: &open.document.tree,
            analysis: &open.analysis,
            workspace: &self.workspace,
            encoding: self.encoding,
            config: &self.config,
        })
    }

    fn dispatch(&mut self, method: &str, params: Value) -> Response {
        use features::*;
        match method {
            "shutdown" => {
                self.shutdown_requested = true;
                Ok(Value::Null)
            }
            "textDocument/hover" => {
                let p: TextDocumentPositionParams = parse(params)?;
                let mut hover = hover::hover(
                    &self.snapshot(&p.text_document.uri)?,
                    p.position,
                );
                if let Some(hover) = hover
                    .as_mut()
                    .filter(|_| !self.formats.hover_markdown)
                {
                    hover.contents = std::mem::replace(
                        &mut hover.contents,
                        MarkupContent::markdown(""),
                    )
                    .into_plain();
                }
                to_value(hover)
            }
            "textDocument/completion" => {
                let p: TextDocumentPositionParams = parse(params)?;
                let snapshot = self.snapshot(&p.text_document.uri)?;
                let mut list = completion::completion(
                    &snapshot,
                    p.position,
                    self.snippet_support,
                );
                if !self.formats.completion_markdown {
                    for item in &mut list.items {
                        item.documentation = item
                            .documentation
                            .take()
                            .map(MarkupContent::into_plain);
                    }
                }
                to_value(list)
            }
            "textDocument/signatureHelp" => {
                let p: TextDocumentPositionParams = parse(params)?;
                let mut help = signature_help::signature_help(
                    &self.snapshot(&p.text_document.uri)?,
                    p.position,
                );
                if !self.formats.signature_markdown {
                    for signature in help
                        .iter_mut()
                        .flat_map(|h| h.signatures.iter_mut())
                    {
                        signature.documentation = signature
                            .documentation
                            .take()
                            .map(MarkupContent::into_plain);
                        for parameter in &mut signature.parameters {
                            parameter.documentation = parameter
                                .documentation
                                .take()
                                .map(MarkupContent::into_plain);
                        }
                    }
                }
                to_value(help)
            }
            "textDocument/definition" => {
                let p: TextDocumentPositionParams = parse(params)?;
                to_value(navigation::definition(
                    &self.snapshot(&p.text_document.uri)?,
                    p.position,
                ))
            }
            "textDocument/typeDefinition" => {
                let p: TextDocumentPositionParams = parse(params)?;
                to_value(navigation::type_definition(
                    &self.snapshot(&p.text_document.uri)?,
                    p.position,
                ))
            }
            "textDocument/references" => {
                let p: ReferenceParams = parse(params)?;
                let snapshot = self.snapshot(&p.text_document.uri)?;
                to_value(navigation::references(
                    &snapshot,
                    p.position,
                    p.context.include_declaration,
                ))
            }
            "textDocument/documentHighlight" => {
                let p: TextDocumentPositionParams = parse(params)?;
                to_value(navigation::document_highlight(
                    &self.snapshot(&p.text_document.uri)?,
                    p.position,
                ))
            }
            "textDocument/prepareRename" => {
                let p: TextDocumentPositionParams = parse(params)?;
                match navigation::prepare_rename(
                    &self.snapshot(&p.text_document.uri)?,
                    p.position,
                ) {
                    Ok((range, placeholder)) => Ok(
                        json!({ "range": range, "placeholder": placeholder }),
                    ),
                    Err(_) => Ok(Value::Null),
                }
            }
            "textDocument/rename" => {
                let p: RenameParams = parse(params)?;
                let snapshot = self.snapshot(&p.text_document.uri)?;
                navigation::rename(&snapshot, p.position, &p.new_name)
                    .map(|edit| {
                        serde_json::to_value(edit).expect("serializable")
                    })
                    .map_err(|message| RequestError {
                        code: error_code::REQUEST_FAILED,
                        message,
                    })
            }
            "textDocument/documentSymbol" => {
                let p: TextDocumentParams = parse(params)?;
                let outline = symbols::document_symbols(
                    &self.snapshot(&p.text_document.uri)?,
                );
                if self.formats.hierarchical_symbols {
                    to_value(outline)
                } else {
                    to_value(symbols::flatten(&p.text_document.uri, &outline))
                }
            }
            "workspace/symbol" => {
                let p: WorkspaceSymbolParams = parse(params)?;
                to_value(symbols::workspace_symbols(
                    &self.workspace,
                    &p.query,
                ))
            }
            "textDocument/foldingRange" => {
                let p: TextDocumentParams = parse(params)?;
                to_value(folding::folding_ranges(
                    &self.snapshot(&p.text_document.uri)?,
                ))
            }
            "textDocument/selectionRange" => {
                let p: SelectionRangeParams = parse(params)?;
                to_value(selection::selection_ranges(
                    &self.snapshot(&p.text_document.uri)?,
                    &p.positions,
                ))
            }
            "textDocument/formatting" => {
                let p: DocumentFormattingParams = parse(params)?;
                to_value(formatting::format(
                    &self.snapshot(&p.text_document.uri)?,
                    &p.options,
                    None,
                ))
            }
            "textDocument/rangeFormatting" => {
                let p: DocumentRangeFormattingParams = parse(params)?;
                to_value(formatting::format(
                    &self.snapshot(&p.text_document.uri)?,
                    &p.options,
                    Some(p.range),
                ))
            }
            "textDocument/semanticTokens/full" => {
                let p: TextDocumentParams = parse(params)?;
                let data = semantic_tokens::semantic_tokens(
                    &self.snapshot(&p.text_document.uri)?,
                    None,
                );
                to_value(SemanticTokens { data })
            }
            "textDocument/semanticTokens/range" => {
                let p: RangeParams = parse(params)?;
                let data = semantic_tokens::semantic_tokens(
                    &self.snapshot(&p.text_document.uri)?,
                    Some(p.range),
                );
                to_value(SemanticTokens { data })
            }
            "textDocument/codeAction" => {
                let p: CodeActionParams = parse(params)?;
                let snapshot = self.snapshot(&p.text_document.uri)?;
                let only = p.context.only.as_deref();
                // Reuse the published diagnostics when they are current.
                let cached = self
                    .documents
                    .get(&p.text_document.uri)
                    .and_then(|open| {
                        open.diagnostics
                            .as_ref()
                            .filter(|(v, _)| *v == open.document.version)
                    })
                    .map(|(_, d)| d.clone());
                let all = cached
                    .unwrap_or_else(|| diagnostics::diagnostics(&snapshot));
                to_value(code_actions::code_actions(
                    &snapshot,
                    p.range,
                    &p.context.diagnostics,
                    only,
                    &all,
                ))
            }
            "textDocument/inlayHint" => {
                let p: RangeParams = parse(params)?;
                to_value(inlay_hints::inlay_hints(
                    &self.snapshot(&p.text_document.uri)?,
                    p.range,
                ))
            }
            "textDocument/documentLink" => {
                let p: TextDocumentParams = parse(params)?;
                to_value(links::document_links(
                    &self.snapshot(&p.text_document.uri)?,
                ))
            }
            "textDocument/prepareCallHierarchy" => {
                let p: TextDocumentPositionParams = parse(params)?;
                to_value(call_hierarchy::prepare(
                    &self.snapshot(&p.text_document.uri)?,
                    p.position,
                ))
            }
            "callHierarchy/incomingCalls" => {
                let p: CallHierarchyCallsParams = parse(params)?;
                to_value(call_hierarchy::incoming(&self.workspace, &p.item))
            }
            "callHierarchy/outgoingCalls" => {
                let p: CallHierarchyCallsParams = parse(params)?;
                to_value(call_hierarchy::outgoing(&self.workspace, &p.item))
            }
            _ => Err(RequestError {
                code: error_code::METHOD_NOT_FOUND,
                message: format!("unsupported method {method}"),
            }),
        }
    }
}
