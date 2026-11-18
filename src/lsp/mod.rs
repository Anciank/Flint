pub mod client;
pub mod transport;

use crate::{
    config::{LspConfig, LspServerConfig},
    index::{NavLocation, ReferenceHit, extractor::SymbolKind},
};
use anyhow::Result;
use client::{LspClient, path_to_uri, uri_to_path};
use lsp_types::{
    CodeAction, CodeActionContext, CodeActionOrCommand, CodeActionParams, CompletionParams,
    CompletionResponse, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverParams, Location, LocationLink, Position,
    PublishDiagnosticsParams, Range, ReferenceContext, ReferenceParams, ServerCapabilities,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, VersionedTextDocumentIdentifier,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspServerStatus {
    Stopped,
    Starting,
    Ready { server_name: String },
    Unavailable(String),
}

impl LspServerStatus {
    pub fn short_label(&self) -> String {
        match self {
            Self::Stopped => "Stopped".to_string(),
            Self::Starting => "Starting...".to_string(),
            Self::Ready { server_name } => format!("Ready ({server_name})"),
            Self::Unavailable(_) => "Unavailable".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LspWorkspaceDiagnostic {
    pub path: PathBuf,
    pub rel_path: String,
    pub diagnostic: lsp_types::Diagnostic,
}

#[derive(Debug, Clone)]
struct OpenDocState {
    path: PathBuf,
    language: String,
    version: i32,
    text: String,
}

struct ManagedServer {
    config: LspServerConfig,
    status: LspServerStatus,
    client: Option<Arc<LspClient>>,
    capabilities: Option<ServerCapabilities>,
}

pub type LspEventCallback = Arc<dyn Fn() + Send + Sync>;

struct LspManagerInner {
    config: RwLock<LspConfig>,
    workspace_root: RwLock<PathBuf>,
    servers: Mutex<HashMap<String, ManagedServer>>,
    open_docs: RwLock<HashMap<PathBuf, OpenDocState>>,
    diagnostics: RwLock<HashMap<(String, PathBuf), Vec<lsp_types::Diagnostic>>>,
    on_event: RwLock<Option<LspEventCallback>>,
    event_seq: AtomicU64,
}

#[derive(Clone)]
pub struct LspManager {
    inner: Arc<LspManagerInner>,
}

impl LspManager {
    pub fn new(config: LspConfig, workspace_root: PathBuf) -> Self {
        Self {
            inner: Arc::new(LspManagerInner {
                config: RwLock::new(config),
                workspace_root: RwLock::new(workspace_root),
                servers: Mutex::new(HashMap::new()),
                open_docs: RwLock::new(HashMap::new()),
                diagnostics: RwLock::new(HashMap::new()),
                on_event: RwLock::new(None),
                event_seq: AtomicU64::new(0),
            }),
        }
    }

    pub fn set_event_callback(&self, cb: LspEventCallback) {
        *self.inner.on_event.write().unwrap() = Some(cb);
    }

    pub fn event_seq(&self) -> u64 {
        self.inner.event_seq.load(Ordering::Relaxed)
    }

    fn emit_event(&self) {
        self.inner.event_seq.fetch_add(1, Ordering::Relaxed);
        if let Some(cb) = self.inner.on_event.read().unwrap().as_ref().cloned() {
            cb();
        }
    }

    pub fn set_workspace(&self, root: &Path) {
        let canonical = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        *self.inner.workspace_root.write().unwrap() = canonical;
        self.shutdown_all();
        self.inner.open_docs.write().unwrap().clear();
        self.inner.diagnostics.write().unwrap().clear();
        self.emit_event();
    }

    pub fn reconcile_config(&self, new_cfg: LspConfig) {
        let old_servers: Vec<(String, LspServerConfig)> = {
            let guard = self.inner.servers.lock().unwrap();
            guard
                .iter()
                .map(|(k, v)| (k.clone(), v.config.clone()))
                .collect()
        };

        for (id, old_srv) in old_servers {
            let keep = new_cfg.enabled
                && new_cfg
                    .servers
                    .iter()
                    .find(|s| s.id == id)
                    .is_some_and(|s| s == &old_srv && s.enabled);
            if !keep {
                self.stop_server(&id);
            }
        }

        *self.inner.config.write().unwrap() = new_cfg;

        // Re-ensure servers for already-open documents if newly enabled
        let docs: Vec<OpenDocState> = self
            .inner
            .open_docs
            .read()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for doc in docs {
            self.ensure_servers_for_language(&doc.language);
        }
        self.emit_event();
    }

    pub fn shutdown_all(&self) {
        let clients: Vec<Arc<LspClient>> = {
            let mut guard = self.inner.servers.lock().unwrap();
            let list = guard
                .values()
                .filter_map(|s| s.client.clone())
                .collect::<Vec<_>>();
            guard.clear();
            list
        };
        for c in clients {
            c.shutdown();
        }
        self.inner.diagnostics.write().unwrap().clear();
    }

    pub fn stop_server(&self, server_id: &str) {
        let client = {
            let mut guard = self.inner.servers.lock().unwrap();
            guard.remove(server_id).and_then(|s| s.client)
        };
        self.inner
            .diagnostics
            .write()
            .unwrap()
            .retain(|(id, _), _| id != server_id);
        if let Some(c) = client {
            c.shutdown();
        }
        self.emit_event();
    }

    pub fn restart_server(&self, server_id: &str) {
        self.stop_server(server_id);
        let cfg = {
            let guard = self.inner.config.read().unwrap();
            guard
                .servers
                .iter()
                .find(|s| s.id == server_id && s.enabled)
                .cloned()
        };
        if let Some(srv) = cfg {
            if !self.inner.config.read().unwrap().enabled {
                return;
            }
            self.ensure_server(srv);
        }
    }

    pub fn server_statuses(&self) -> Vec<(String, String, LspServerStatus)> {
        let cfg = self.inner.config.read().unwrap().clone();
        let servers_guard = self.inner.servers.lock().unwrap();
        cfg.servers
            .iter()
            .map(|srv| {
                let status = if !cfg.enabled || !srv.enabled {
                    LspServerStatus::Stopped
                } else if let Some(managed) = servers_guard.get(&srv.id) {
                    managed.status.clone()
                } else {
                    LspServerStatus::Stopped
                };
                (srv.id.clone(), srv.name.clone(), status)
            })
            .collect()
    }

    pub fn active_status_for_language(&self, language: &str) -> Option<(String, LspServerStatus)> {
        let cfg = self.inner.config.read().unwrap().clone();
        let matching = cfg.servers_for_language(language);
        if matching.is_empty() {
            return None;
        }
        let guard = self.inner.servers.lock().unwrap();

        let mut ready_names = Vec::new();
        let mut starting: Option<(String, LspServerStatus)> = None;
        let mut unavailable: Option<(String, LspServerStatus)> = None;
        let mut fallback_stopped: Option<(String, LspServerStatus)> = None;

        for srv in matching {
            let st = guard
                .get(&srv.id)
                .map(|m| m.status.clone())
                .unwrap_or(LspServerStatus::Stopped);
            match &st {
                LspServerStatus::Ready { server_name } => {
                    if !ready_names.contains(server_name) {
                        ready_names.push(server_name.clone());
                    }
                }
                LspServerStatus::Starting if starting.is_none() => {
                    starting = Some((srv.name.clone(), st));
                }
                LspServerStatus::Unavailable(_) if unavailable.is_none() => {
                    unavailable = Some((srv.name.clone(), st));
                }
                _ if fallback_stopped.is_none() => {
                    fallback_stopped = Some((srv.name.clone(), st));
                }
                _ => {}
            }
        }

        if !ready_names.is_empty() {
            let joined = ready_names.join(" + ");
            return Some((
                joined.clone(),
                LspServerStatus::Ready {
                    server_name: joined,
                },
            ));
        }
        starting.or(unavailable).or(fallback_stopped)
    }

    pub fn did_open(&self, path: &Path, language: &str, text: &str) {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let doc = OpenDocState {
            path: canonical.clone(),
            language: language.to_string(),
            version: 1,
            text: text.to_string(),
        };
        self.inner
            .open_docs
            .write()
            .unwrap()
            .insert(canonical.clone(), doc.clone());

        self.ensure_servers_for_language(language);
        for (server_id, client, caps) in self.ready_clients_for_language(language) {
            if let Some(uri) = path_to_uri(&canonical) {
                let params = DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri,
                        language_id: lsp_language_id(language).to_string(),
                        version: doc.version,
                        text: doc.text.clone(),
                    },
                };
                if let Ok(v) = serde_json::to_value(params) {
                    let _ = client.notify("textDocument/didOpen", v);
                }
                self.schedule_pull_diagnostics(server_id, client, &caps, canonical.clone());
            }
        }
    }

    pub fn did_change(&self, path: &Path, language: &str, text: &str) {
        let canonical = {
            let docs = self.inner.open_docs.read().unwrap();
            if docs.contains_key(path) {
                path.to_path_buf()
            } else {
                drop(docs);
                fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
            }
        };
        let version = {
            let mut docs = self.inner.open_docs.write().unwrap();
            let entry = docs.entry(canonical.clone()).or_insert_with(|| OpenDocState {
                path: canonical.clone(),
                language: language.to_string(),
                version: 0,
                text: String::new(),
            });
            entry.version += 1;
            entry.text = text.to_string();
            entry.version
        };

        for (server_id, client, caps) in self.ready_clients_for_language(language) {
            if let Some(uri) = path_to_uri(&canonical) {
                let params = DidChangeTextDocumentParams {
                    text_document: VersionedTextDocumentIdentifier { uri, version },
                    content_changes: vec![TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: text.to_string(),
                    }],
                };
                if let Ok(v) = serde_json::to_value(params) {
                    let _ = client.notify("textDocument/didChange", v);
                }
                self.schedule_pull_diagnostics(server_id, client, &caps, canonical.clone());
            }
        }
    }

    pub fn did_save(&self, path: &Path, language: &str, text: &str) {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        for (server_id, client, caps) in self.ready_clients_for_language(language) {
            if let Some(uri) = path_to_uri(&canonical) {
                let params = DidSaveTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri },
                    text: Some(text.to_string()),
                };
                if let Ok(v) = serde_json::to_value(params) {
                    let _ = client.notify("textDocument/didSave", v);
                }
                self.schedule_pull_diagnostics(server_id, client, &caps, canonical.clone());
            }
        }
    }

    pub fn did_close(&self, path: &Path) {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let removed = self.inner.open_docs.write().unwrap().remove(&canonical);
        self.inner
            .diagnostics
            .write()
            .unwrap()
            .retain(|(_, p), _| p != &canonical);

        if let Some(doc) = removed {
            if let Some(uri) = path_to_uri(&canonical) {
                let params = DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri },
                };
                if let Ok(v) = serde_json::to_value(params) {
                    for (_, client, _) in self.ready_clients_for_language(&doc.language) {
                        let _ = client.notify("textDocument/didClose", v.clone());
                    }
                }
            }
        }
        self.emit_event();
    }

    pub fn diagnostics_for(&self, path: &Path) -> Vec<lsp_types::Diagnostic> {
        let guard = self.inner.diagnostics.read().unwrap();
        if guard.is_empty() {
            return Vec::new();
        }
        let target = if guard.keys().any(|(_, p)| p == path) {
            path.to_path_buf()
        } else {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        };
        collect_merged_diagnostics_for_path(&guard, &target)
    }

    pub fn diagnostic_count(&self) -> usize {
        let guard = self.inner.diagnostics.read().unwrap();
        let mut by_path: HashMap<&PathBuf, usize> = HashMap::new();
        for ((_, p), list) in guard.iter() {
            *by_path.entry(p).or_default() += list.len();
        }
        by_path.values().sum()
    }

    pub fn all_diagnostics(&self) -> Vec<LspWorkspaceDiagnostic> {
        let root = self.inner.workspace_root.read().unwrap().clone();
        let guard = self.inner.diagnostics.read().unwrap();
        let mut unique_paths: Vec<PathBuf> = guard.keys().map(|(_, p)| p.clone()).collect();
        unique_paths.sort();
        unique_paths.dedup();

        let mut out = Vec::new();
        for path in unique_paths {
            let rel_path = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            for d in collect_merged_diagnostics_for_path(&guard, &path) {
                out.push(LspWorkspaceDiagnostic {
                    path: path.clone(),
                    rel_path: rel_path.clone(),
                    diagnostic: d,
                });
            }
        }
        out.sort_by(|a, b| {
            a.rel_path
                .cmp(&b.rel_path)
                .then_with(|| a.diagnostic.range.start.line.cmp(&b.diagnostic.range.start.line))
        });
        out
    }

    pub fn definition(
        &self,
        path: &Path,
        language: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<NavLocation>> {
        self.goto_request("textDocument/definition", path, language, line, col)
    }

    pub fn declaration(
        &self,
        path: &Path,
        language: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<NavLocation>> {
        self.goto_request("textDocument/declaration", path, language, line, col)
    }

    pub fn type_definition(
        &self,
        path: &Path,
        language: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<NavLocation>> {
        self.goto_request("textDocument/typeDefinition", path, language, line, col)
    }

    pub fn implementation(
        &self,
        path: &Path,
        language: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<NavLocation>> {
        self.goto_request("textDocument/implementation", path, language, line, col)
    }

    fn goto_request(
        &self,
        method: &str,
        path: &Path,
        language: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<NavLocation>> {
        let clients = self.ready_clients_for_language(language);
        if clients.is_empty() {
            return Ok(Vec::new());
        }
        let canonical = {
            let docs = self.inner.open_docs.read().unwrap();
            if docs.contains_key(path) {
                path.to_path_buf()
            } else {
                drop(docs);
                fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
            }
        };
        let Some(uri) = path_to_uri(&canonical) else {
            return Ok(Vec::new());
        };
        let timeout = self.request_timeout();
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position::new(line, col),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let params_val = serde_json::to_value(params)?;
        let root = self.inner.workspace_root.read().unwrap().clone();

        for (_, client, caps) in clients {
            if !supports_goto_capability(&caps, method) {
                continue;
            }
            if let Ok(val) = client.request(method, params_val.clone(), timeout) {
                if !val.is_null() {
                    if let Ok(resp) = serde_json::from_value::<GotoDefinitionResponse>(val) {
                        let locs = goto_response_to_nav_locations(resp, &root);
                        if !locs.is_empty() {
                            return Ok(locs);
                        }
                    }
                }
            }
        }
        Ok(Vec::new())
    }

    pub fn references(
        &self,
        path: &Path,
        language: &str,
        line: u32,
        col: u32,
        symbol_name: &str,
    ) -> Result<Vec<ReferenceHit>> {
        let Some((_, client, _)) = self
            .ready_clients_for_language(language)
            .into_iter()
            .find(|(_, _, caps)| caps.references_provider.is_some())
        else {
            return Ok(Vec::new());
        };
        let Some(uri) = path_to_uri(path) else {
            return Ok(Vec::new());
        };
        let timeout = self.request_timeout();
        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position::new(line, col),
            },
            context: ReferenceContext {
                include_declaration: true,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let val = client.request(
            "textDocument/references",
            serde_json::to_value(params)?,
            timeout,
        )?;
        if val.is_null() {
            return Ok(Vec::new());
        }
        let locs: Vec<Location> = serde_json::from_value(val)?;
        let root = self.inner.workspace_root.read().unwrap().clone();
        let open_docs = self.inner.open_docs.read().unwrap();
        let mut hits = Vec::new();
        for loc in locs {
            let Some(target_path) = uri_to_path(&loc.uri) else {
                continue;
            };
            let canonical = fs::canonicalize(&target_path).unwrap_or(target_path);
            let rel_path = canonical
                .strip_prefix(&root)
                .unwrap_or(&canonical)
                .to_string_lossy()
                .to_string();
            let line_text = open_docs
                .get(&canonical)
                .map(|d| d.text.clone())
                .or_else(|| fs::read_to_string(&canonical).ok())
                .and_then(|txt| {
                    txt.lines()
                        .nth(loc.range.start.line as usize)
                        .map(|s| s.trim().to_string())
                })
                .unwrap_or_default();

            hits.push(ReferenceHit {
                path: canonical,
                rel_path,
                name: symbol_name.to_string(),
                line: loc.range.start.line,
                col: loc.range.start.character,
                byte_range: 0..0,
                line_text,
                enclosing_symbol: None,
                is_definition: false,
                is_call: false,
            });
        }
        Ok(hits)
    }

    pub fn hover(
        &self,
        path: &Path,
        language: &str,
        line: u32,
        col: u32,
    ) -> Result<Option<Hover>> {
        let clients = self.ready_clients_for_language(language);
        if clients.is_empty() {
            return Ok(None);
        }
        let Some(uri) = path_to_uri(path) else {
            return Ok(None);
        };
        let timeout = self.request_timeout();
        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position::new(line, col),
            },
            work_done_progress_params: Default::default(),
        };
        let params_val = serde_json::to_value(params)?;
        for (_, client, caps) in clients {
            if caps.hover_provider.is_none() {
                continue;
            }
            if let Ok(val) = client.request("textDocument/hover", params_val.clone(), timeout) {
                if !val.is_null() {
                    if let Ok(hover) = serde_json::from_value::<Hover>(val) {
                        return Ok(Some(hover));
                    }
                }
            }
        }
        Ok(None)
    }

    pub fn completions(
        &self,
        path: &Path,
        language: &str,
        line: u32,
        col: u32,
    ) -> Result<Option<CompletionResponse>> {
        let Some((_, client, _)) = self
            .ready_clients_for_language(language)
            .into_iter()
            .find(|(_, _, caps)| caps.completion_provider.is_some())
        else {
            return Ok(None);
        };
        let Some(uri) = path_to_uri(path) else {
            return Ok(None);
        };
        let timeout = self.request_timeout();
        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position::new(line, col),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        let val = client.request(
            "textDocument/completion",
            serde_json::to_value(params)?,
            timeout,
        )?;
        if val.is_null() {
            return Ok(None);
        }
        let resp: CompletionResponse = serde_json::from_value(val)?;
        Ok(Some(resp))
    }

    pub fn code_actions(
        &self,
        path: &Path,
        language: &str,
        start: Position,
        end: Position,
    ) -> Result<Vec<CodeAction>> {
        let clients = self.ready_clients_for_language(language);
        if clients.is_empty() {
            return Ok(Vec::new());
        }
        let Some(uri) = path_to_uri(path) else {
            return Ok(Vec::new());
        };
        let range = Range::new(start, end);
        let diagnostics: Vec<lsp_types::Diagnostic> = self
            .diagnostics_for(path)
            .into_iter()
            .filter(|d| d.range.end >= range.start && d.range.start <= range.end)
            .collect();
        let timeout = self.request_timeout();
        let params = CodeActionParams {
            text_document: TextDocumentIdentifier { uri },
            range,
            context: CodeActionContext {
                diagnostics,
                only: None,
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let params_val = serde_json::to_value(params)?;
        let mut combined = Vec::new();
        for (_, client, caps) in clients {
            if caps.code_action_provider.is_none() {
                continue;
            }
            if let Ok(val) =
                client.request("textDocument/codeAction", params_val.clone(), timeout)
            {
                if val.is_null() {
                    continue;
                }
                if let Ok(raw) = serde_json::from_value::<Vec<CodeActionOrCommand>>(val) {
                    for item in raw {
                        let action = match item {
                            CodeActionOrCommand::CodeAction(a) => a,
                            CodeActionOrCommand::Command(cmd) => CodeAction {
                                title: cmd.title.clone(),
                                command: Some(cmd),
                                ..Default::default()
                            },
                        };
                        if !combined
                            .iter()
                            .any(|existing: &CodeAction| existing.title == action.title)
                        {
                            combined.push(action);
                        }
                    }
                }
            }
        }
        Ok(combined)
    }

    pub fn inlay_hints(
        &self,
        path: &Path,
        language: &str,
        start_line: u32,
        end_line: u32,
    ) -> Result<Vec<lsp_types::InlayHint>> {
        let clients = self.ready_clients_for_language(language);
        if clients.is_empty() {
            return Ok(Vec::new());
        }
        let Some(uri) = path_to_uri(path) else {
            return Ok(Vec::new());
        };
        let timeout = self.request_timeout();
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": start_line, "character": 0 },
                "end": { "line": end_line, "character": 0 }
            }
        });
        for (_, client, caps) in clients {
            if caps.inlay_hint_provider.is_none() {
                continue;
            }
            if let Ok(val) = client.request("textDocument/inlayHint", params.clone(), timeout) {
                if !val.is_null() {
                    if let Ok(hints) = serde_json::from_value::<Vec<lsp_types::InlayHint>>(val) {
                        if !hints.is_empty() {
                            return Ok(hints);
                        }
                    }
                }
            }
        }
        Ok(Vec::new())
    }

    fn request_timeout(&self) -> Duration {
        let ms = self
            .inner
            .config
            .read()
            .unwrap()
            .request_timeout_ms
            .clamp(500, 30000);
        Duration::from_millis(ms)
    }

    fn ready_clients_for_language(
        &self,
        language: &str,
    ) -> Vec<(String, Arc<LspClient>, ServerCapabilities)> {
        let server_ids: Vec<String> = {
            let cfg = self.inner.config.read().unwrap();
            cfg.servers_for_language(language)
                .into_iter()
                .map(|s| s.id.clone())
                .collect()
        };
        let guard = self.inner.servers.lock().unwrap();
        server_ids
            .into_iter()
            .filter_map(|id| {
                let managed = guard.get(&id)?;
                let client = managed.client.as_ref()?.clone();
                if client.is_closed() {
                    return None;
                }
                let caps = managed.capabilities.clone()?;
                Some((id, client, caps))
            })
            .collect()
    }

    fn ensure_servers_for_language(&self, language: &str) {
        let matching: Vec<LspServerConfig> = {
            let cfg = self.inner.config.read().unwrap();
            cfg.servers_for_language(language)
                .into_iter()
                .cloned()
                .collect()
        };
        for server_cfg in matching {
            self.ensure_server(server_cfg);
        }
    }

    fn ensure_server(&self, server_cfg: LspServerConfig) -> Option<Arc<LspClient>> {
        {
            let mut guard = self.inner.servers.lock().unwrap();
            if let Some(existing) = guard.get(&server_cfg.id) {
                match &existing.status {
                    LspServerStatus::Ready { .. } => {
                        if let Some(c) = &existing.client {
                            if !c.is_closed() {
                                return Some(c.clone());
                            }
                        }
                    }
                    LspServerStatus::Starting | LspServerStatus::Unavailable(_) => {
                        return None;
                    }
                    LspServerStatus::Stopped => {}
                }
            }
            guard.insert(
                server_cfg.id.clone(),
                ManagedServer {
                    config: server_cfg.clone(),
                    status: LspServerStatus::Starting,
                    client: None,
                    capabilities: None,
                },
            );
        }
        self.emit_event();

        let manager = self.clone();
        let root = self.inner.workspace_root.read().unwrap().clone();
        thread::Builder::new()
            .name(format!("lsp-init-{}", server_cfg.id))
            .spawn(move || {
                let diag_mgr = manager.clone();
                let srv_id_for_notif = server_cfg.id.clone();
                let notif_handler: client::NotificationHandler = Arc::new(move |method, params| {
                    diag_mgr.handle_notification(&srv_id_for_notif, method, params);
                });

                match LspClient::spawn(&server_cfg, &root, notif_handler) {
                    Ok(client) => match client.initialize(&root, &server_cfg, Duration::from_secs(12)) {
                        Ok(init_res) => {
                            let server_name = init_res
                                .server_info
                                .as_ref()
                                .map(|i| i.name.clone())
                                .unwrap_or_else(|| server_cfg.name.clone());
                            let caps = init_res.capabilities;
                            {
                                let mut guard = manager.inner.servers.lock().unwrap();
                                if let Some(entry) = guard.get_mut(&server_cfg.id) {
                                    entry.status = LspServerStatus::Ready { server_name };
                                    entry.client = Some(client.clone());
                                    entry.capabilities = Some(caps.clone());
                                }
                            }
                            let docs: Vec<OpenDocState> = manager
                                .inner
                                .open_docs
                                .read()
                                .unwrap()
                                .values()
                                .filter(|d| {
                                    server_cfg
                                        .languages
                                        .iter()
                                        .any(|l| l.eq_ignore_ascii_case(&d.language))
                                })
                                .cloned()
                                .collect();
                            for doc in docs {
                                if let Some(uri) = path_to_uri(&doc.path) {
                                    let params = DidOpenTextDocumentParams {
                                        text_document: TextDocumentItem {
                                            uri,
                                            language_id: lsp_language_id(&doc.language).to_string(),
                                            version: doc.version,
                                            text: doc.text,
                                        },
                                    };
                                    if let Ok(v) = serde_json::to_value(params) {
                                        let _ = client.notify("textDocument/didOpen", v);
                                    }
                                    manager.pull_diagnostics_sync(
                                        &server_cfg.id,
                                        &client,
                                        &caps,
                                        &doc.path,
                                    );
                                }
                            }
                            manager.emit_event();
                        }
                        Err(err) => {
                            client.shutdown();
                            let mut guard = manager.inner.servers.lock().unwrap();
                            if let Some(entry) = guard.get_mut(&server_cfg.id) {
                                entry.status = LspServerStatus::Unavailable(format!("{err}"));
                                entry.client = None;
                            }
                            manager.emit_event();
                        }
                    },
                    Err(err) => {
                        let mut guard = manager.inner.servers.lock().unwrap();
                        if let Some(entry) = guard.get_mut(&server_cfg.id) {
                            entry.status = LspServerStatus::Unavailable(format!("{err}"));
                            entry.client = None;
                        }
                        manager.emit_event();
                    }
                }
            })
            .ok();

        None
    }

    fn schedule_pull_diagnostics(
        &self,
        server_id: String,
        client: Arc<LspClient>,
        caps: &ServerCapabilities,
        canonical: PathBuf,
    ) {
        if caps.diagnostic_provider.is_none() || client.is_closed() {
            return;
        }
        let mgr = self.clone();
        let caps_cloned = caps.clone();
        thread::Builder::new()
            .name(format!("lsp-diag-{server_id}"))
            .spawn(move || {
                mgr.pull_diagnostics_sync(&server_id, &client, &caps_cloned, &canonical);
            })
            .ok();
    }

    fn pull_diagnostics_sync(
        &self,
        server_id: &str,
        client: &LspClient,
        caps: &ServerCapabilities,
        canonical: &Path,
    ) {
        if caps.diagnostic_provider.is_none() || client.is_closed() {
            return;
        }
        let Some(uri) = path_to_uri(canonical) else {
            return;
        };
        let params = serde_json::json!({
            "textDocument": { "uri": uri }
        });
        let timeout = self.request_timeout();
        if let Ok(val) = client.request("textDocument/diagnostic", params, timeout) {
            if let Some(items_val) = val.get("items") {
                if let Ok(items) =
                    serde_json::from_value::<Vec<lsp_types::Diagnostic>>(items_val.clone())
                {
                    self.inner
                        .diagnostics
                        .write()
                        .unwrap()
                        .insert((server_id.to_string(), canonical.to_path_buf()), items);
                    self.emit_event();
                }
            }
        }
    }

    fn handle_notification(&self, server_id: &str, method: &str, params: Value) {
        if method == "textDocument/publishDiagnostics" {
            if let Ok(parsed) = serde_json::from_value::<PublishDiagnosticsParams>(params) {
                if let Some(path) = uri_to_path(&parsed.uri) {
                    let canonical = fs::canonicalize(&path).unwrap_or(path);
                    self.inner
                        .diagnostics
                        .write()
                        .unwrap()
                        .insert((server_id.to_string(), canonical), parsed.diagnostics);
                    self.emit_event();
                }
            }
        }
    }
}

fn supports_goto_capability(caps: &ServerCapabilities, method: &str) -> bool {
    match method {
        "textDocument/definition" => caps.definition_provider.is_some(),
        "textDocument/declaration" => caps.declaration_provider.is_some(),
        "textDocument/typeDefinition" => caps.type_definition_provider.is_some(),
        "textDocument/implementation" => caps.implementation_provider.is_some(),
        _ => false,
    }
}

fn collect_merged_diagnostics_for_path(
    map: &HashMap<(String, PathBuf), Vec<lsp_types::Diagnostic>>,
    path: &Path,
) -> Vec<lsp_types::Diagnostic> {
    let mut keys: Vec<&(String, PathBuf)> = map.keys().filter(|(_, p)| p == path).collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out: Vec<lsp_types::Diagnostic> = Vec::new();
    for key in keys {
        if let Some(list) = map.get(key) {
            for diag in list {
                let dup = out.iter().any(|existing| {
                    existing.range == diag.range
                        && existing.message == diag.message
                        && existing.source == diag.source
                });
                if !dup {
                    out.push(diag.clone());
                }
            }
        }
    }
    out.sort_by(|a, b| {
        a.range
            .start
            .line
            .cmp(&b.range.start.line)
            .then_with(|| a.range.start.character.cmp(&b.range.start.character))
    });
    out
}

fn goto_response_to_nav_locations(
    resp: GotoDefinitionResponse,
    workspace_root: &Path,
) -> Vec<NavLocation> {
    let mut out = Vec::new();
    let push_range = |out: &mut Vec<NavLocation>, uri: &lsp_types::Uri, range: Range| {
        let Some(path) = uri_to_path(uri) else {
            return;
        };
        let canonical = fs::canonicalize(&path).unwrap_or(path);
        let rel_path = canonical
            .strip_prefix(workspace_root)
            .unwrap_or(&canonical)
            .to_string_lossy()
            .to_string();
        let line_text = fs::read_to_string(&canonical)
            .ok()
            .and_then(|t| {
                t.lines()
                    .nth(range.start.line as usize)
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_else(|| format!("{rel_path}:{}", range.start.line + 1));
        out.push(NavLocation {
            path: canonical,
            rel_path,
            symbol_name: String::new(),
            kind: SymbolKind::Function,
            signature: line_text,
            doc_comment: None,
            start_line: range.start.line,
            start_col: range.start.character,
            end_line: range.end.line,
            end_col: range.end.character,
            byte_range: 0..0,
            selection_range: 0..0,
            score: 100,
        });
    };

    match resp {
        GotoDefinitionResponse::Scalar(loc) => push_range(&mut out, &loc.uri, loc.range),
        GotoDefinitionResponse::Array(locs) => {
            for loc in locs {
                push_range(&mut out, &loc.uri, loc.range);
            }
        }
        GotoDefinitionResponse::Link(links) => {
            for link in links {
                push_range(&mut out, &link.target_uri, link.target_selection_range);
            }
        }
    }
    out
}

pub fn location_link_from_nav(
    loc: &NavLocation,
    origin_selection: Option<Range>,
) -> Option<LocationLink> {
    let uri = path_to_uri(&loc.path)?;
    Some(LocationLink {
        origin_selection_range: origin_selection,
        target_uri: uri,
        target_range: Range::new(
            Position::new(loc.start_line, loc.start_col),
            Position::new(loc.end_line, loc.end_col),
        ),
        target_selection_range: Range::new(
            Position::new(loc.start_line, loc.start_col),
            Position::new(
                loc.start_line,
                loc.start_col + loc.symbol_name.encode_utf16().count() as u32,
            ),
        ),
    })
}

pub fn lsp_language_id(lang: &str) -> &str {
    match lang {
        "tsx" => "typescriptreact",
        "text" => "plaintext",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tempfile::TempDir;

    fn write_mock_lsp_script(dir: &Path) -> PathBuf {
        let script = dir.join("mock_lsp.py");
        let code = r#"
import json, sys

def read_msg():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        s = line.decode("utf-8").strip()
        if not s:
            break
        if ":" in s:
            k, v = s.split(":", 1)
            headers[k.strip().lower()] = v.strip()
    length = int(headers.get("content-length", "0"))
    body = sys.stdin.buffer.read(length)
    return json.loads(body.decode("utf-8"))

def send_msg(obj):
    raw = json.dumps(obj).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {len(raw)}\r\n\r\n".encode("ascii"))
    sys.stdout.buffer.write(raw)
    sys.stdout.buffer.flush()

target_uri = None

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method")
    msg_id = msg.get("id")
    if method == "initialize":
        send_msg({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {
                "capabilities": {
                    "textDocumentSync": 1,
                    "definitionProvider": True,
                    "referencesProvider": True,
                    "hoverProvider": True,
                    "completionProvider": {},
                    "codeActionProvider": True
                },
                "serverInfo": {"name": "mock-lsp", "version": "1.0"}
            }
        })
    elif method == "textDocument/didOpen":
        uri = msg["params"]["textDocument"]["uri"]
        if target_uri is None:
            target_uri = uri
        send_msg({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": [{
                    "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 2}},
                    "severity": 2,
                    "source": "mock-lsp",
                    "message": "Mock LSP warning"
                }]
            }
        })
    elif method == "textDocument/definition":
        uri = msg["params"]["textDocument"]["uri"]
        send_msg({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": [{
                "uri": uri,
                "range": {"start": {"line": 2, "character": 4}, "end": {"line": 2, "character": 9}}
            }]
        })
    elif method == "textDocument/references":
        uri = msg["params"]["textDocument"]["uri"]
        send_msg({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": [{
                "uri": uri,
                "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 5}}
            }]
        })
    elif method == "textDocument/hover":
        send_msg({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {
                "contents": {"kind": "markdown", "value": "**mock-lsp** hover info"}
            }
        })
    elif method == "textDocument/completion":
        send_msg({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": [{"label": "mock_complete", "detail": "from mock-lsp"}]
        })
    elif method == "textDocument/codeAction":
        send_msg({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": [{"title": "Apply mock quickfix", "kind": "quickfix"}]
        })
    elif method == "shutdown":
        send_msg({"jsonrpc": "2.0", "id": msg_id, "result": None})
    elif method == "exit":
        break
"#;
        fs::write(&script, code).unwrap();
        script
    }

    #[test]
    fn lsp_manager_initializes_syncs_and_queries_mock_stdio_server() {
        let dir = TempDir::new().unwrap();
        let script = write_mock_lsp_script(dir.path());
        let file = dir.path().join("demo.rs");
        fs::write(&file, "fn main() {\n    let x = 1;\n    let target = x;\n}\n").unwrap();

        let cfg = LspConfig {
            enabled: true,
            request_timeout_ms: 3000,
            servers: vec![LspServerConfig {
                id: "mock".to_string(),
                name: "mock-lsp".to_string(),
                enabled: true,
                languages: vec!["rust".to_string()],
                command: "python3".to_string(),
                args: vec![script.to_string_lossy().to_string()],
                ..Default::default()
            }],
        };

        let mgr = LspManager::new(cfg.clone(), dir.path().to_path_buf());
        mgr.did_open(&file, "rust", &fs::read_to_string(&file).unwrap());

        // Wait up to 3s for mock server init + publishDiagnostics
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let ready = mgr
                .active_status_for_language("rust")
                .is_some_and(|(_, st)| matches!(st, LspServerStatus::Ready { .. }));
            let has_diag = !mgr.diagnostics_for(&file).is_empty();
            if ready && has_diag {
                break;
            }
            if Instant::now() > deadline {
                panic!(
                    "Timed out waiting for mock LSP server: status={:?}, diags={:?}",
                    mgr.active_status_for_language("rust"),
                    mgr.diagnostics_for(&file)
                );
            }
            thread::sleep(Duration::from_millis(20));
        }

        let diags = mgr.diagnostics_for(&file);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "Mock LSP warning");

        let defs = mgr.definition(&file, "rust", 0, 0).unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].start_line, 2);

        let refs = mgr.references(&file, "rust", 0, 0, "main").unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].line, 1);

        let hover = mgr.hover(&file, "rust", 0, 0).unwrap().unwrap();
        let hover_json = serde_json::to_string(&hover).unwrap();
        assert!(hover_json.contains("mock-lsp"));

        let comp = mgr.completions(&file, "rust", 1, 8).unwrap().unwrap();
        let comp_json = serde_json::to_string(&comp).unwrap();
        assert!(comp_json.contains("mock_complete"));

        let actions = mgr
            .code_actions(&file, "rust", Position::new(0, 0), Position::new(0, 2))
            .unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Apply mock quickfix");

        // Reconciling with disabled config stops the server cleanly
        let mut disabled = cfg;
        disabled.enabled = false;
        mgr.reconcile_config(disabled);
        assert!(mgr.active_status_for_language("rust").is_none());
    }

    #[test]
    fn lsp_manager_runs_multiple_servers_and_pull_diagnostics_per_language() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("app.py");
        fs::write(&file, "import os\nx: int = 'bad'\ny = unknown_name\n").unwrap();

        let make_mock = |name: &str, mode: &str| {
            let path = dir.path().join(format!("{name}.py"));
            fs::write(
                &path,
                format!(
                    r#"import sys, json

mode = {mode:?}
srv_name = {name:?}

def read_msg():
    headers = {{}}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        s = line.decode("utf-8", errors="ignore")
        if s == "\r\n" or s == "\n":
            break
        if ":" in s:
            k, v = s.split(":", 1)
            headers[k.strip().lower()] = v.strip()
    length = int(headers.get("content-length", "0"))
    if length <= 0:
        return None
    body = sys.stdin.buffer.read(length)
    return json.loads(body.decode("utf-8"))

def send_msg(obj):
    raw = json.dumps(obj).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {{len(raw)}}\r\n\r\n".encode("ascii") + raw)
    sys.stdout.buffer.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method")
    msg_id = msg.get("id")
    if method == "initialize":
        caps = {{"diagnosticProvider": {{"interFileDependencies": False, "workspaceDiagnostics": False}}, "codeActionProvider": True}}
        if mode == "type":
            caps["definitionProvider"] = True
            caps["hoverProvider"] = True
        send_msg({{"jsonrpc": "2.0", "id": msg_id, "result": {{"capabilities": caps, "serverInfo": {{"name": srv_name}}}}}})
    elif method == "textDocument/diagnostic":
        diag_msg = "Type error from mock ty" if mode == "type" else "Lint error from mock ruff"
        line = 1 if mode == "type" else 2
        send_msg({{
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {{
                "kind": "full",
                "items": [{{
                    "range": {{"start": {{"line": line, "character": 0}}, "end": {{"line": line, "character": 5}}}},
                    "severity": 1,
                    "source": srv_name,
                    "message": diag_msg
                }}]
            }}
        }})
    elif method == "textDocument/definition":
        uri = msg["params"]["textDocument"]["uri"]
        send_msg({{
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": [{{"uri": uri, "range": {{"start": {{"line": 0, "character": 0}}, "end": {{"line": 0, "character": 6}}}}}}]
        }})
    elif method == "textDocument/codeAction":
        title = "Fix type" if mode == "type" else "Ruff fix F821"
        send_msg({{"jsonrpc": "2.0", "id": msg_id, "result": [{{"title": title}}]}})
    elif method == "shutdown":
        send_msg({{"jsonrpc": "2.0", "id": msg_id, "result": None}})
    elif method == "exit":
        break
"#
                ),
            )
            .unwrap();
            path
        };

        let ty_mock = make_mock("mock_ty", "type");
        let ruff_mock = make_mock("mock_ruff", "lint");

        let cfg = LspConfig {
            enabled: true,
            request_timeout_ms: 3000,
            servers: vec![
                LspServerConfig {
                    id: "python_ty".to_string(),
                    name: "ty".to_string(),
                    enabled: true,
                    languages: vec!["python".to_string()],
                    command: "python3".to_string(),
                    args: vec![ty_mock.to_string_lossy().to_string()],
                    ..Default::default()
                },
                LspServerConfig {
                    id: "python_ruff".to_string(),
                    name: "ruff".to_string(),
                    enabled: true,
                    languages: vec!["python".to_string()],
                    command: "python3".to_string(),
                    args: vec![ruff_mock.to_string_lossy().to_string()],
                    ..Default::default()
                },
            ],
        };

        let mgr = LspManager::new(cfg, dir.path().to_path_buf());
        mgr.did_open(&file, "python", &fs::read_to_string(&file).unwrap());

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let status_txt = mgr
                .active_status_for_language("python")
                .map(|(n, _)| n)
                .unwrap_or_default();
            let diags = mgr.diagnostics_for(&file);
            if status_txt.contains("mock_ty") && status_txt.contains("mock_ruff") && diags.len() >= 2
            {
                break;
            }
            if Instant::now() > deadline {
                panic!(
                    "Timed out waiting for dual Python LSPs: status={:?}, diags={diags:?}",
                    mgr.active_status_for_language("python")
                );
            }
            thread::sleep(Duration::from_millis(20));
        }

        let diags = mgr.diagnostics_for(&file);
        assert!(diags.iter().any(|d| d.message == "Type error from mock ty"));
        assert!(diags.iter().any(|d| d.message == "Lint error from mock ruff"));

        let defs = mgr.definition(&file, "python", 1, 0).unwrap();
        assert_eq!(defs.len(), 1);

        let actions = mgr
            .code_actions(&file, "python", Position::new(0, 0), Position::new(2, 10))
            .unwrap();
        let titles: Vec<&str> = actions.iter().map(|a| a.title.as_str()).collect();
        assert!(titles.contains(&"Fix type"));
        assert!(titles.contains(&"Ruff fix F821"));

        mgr.shutdown_all();
    }

    #[test]
    fn real_ty_and_ruff_python_lsp_diagnostics_when_installed() {
        let ty_bin = client::resolve_executable("ty");
        let ruff_bin = client::resolve_executable("ruff");
        if !ty_bin.is_file() || !ruff_bin.is_file() {
            return;
        }

        let dir = TempDir::new().unwrap();
        let file = dir.path().join("demo.py");
        let src = "x: int = 'bad'\ny = undefined_symbol\n";
        fs::write(&file, src).unwrap();

        let cfg = LspConfig::default();
        let mgr = LspManager::new(cfg, dir.path().to_path_buf());
        mgr.did_open(&file, "python", src);

        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            let status_txt = mgr
                .active_status_for_language("python")
                .map(|(n, _)| n)
                .unwrap_or_default();
            let diags = mgr.diagnostics_for(&file);
            let has_ty = diags
                .iter()
                .any(|d| d.source.as_deref() == Some("ty") || d.message.contains("assignable"));
            let has_ruff = diags.iter().any(|d| {
                d.source.as_deref() == Some("Ruff") || d.message.contains("undefined_symbol")
            });
            if status_txt.contains("ty") && status_txt.contains("ruff") && has_ty && has_ruff {
                break;
            }
            if Instant::now() > deadline {
                panic!(
                    "Timed out waiting for real ty & ruff diagnostics: status={:?}, diags={diags:?}",
                    mgr.active_status_for_language("python")
                );
            }
            thread::sleep(Duration::from_millis(25));
        }

        mgr.shutdown_all();
    }
}
