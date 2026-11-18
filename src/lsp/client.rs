use crate::{
    config::LspServerConfig,
    lsp::transport::{read_lsp_message, write_lsp_message},
};
use anyhow::{Context, Result, anyhow};
use lsp_types::{
    ClientCapabilities, CodeActionClientCapabilities, CodeActionLiteralSupport,
    CodeActionKindLiteralSupport, CompletionClientCapabilities, CompletionItemCapability,
    DiagnosticClientCapabilities, GotoCapability, HoverClientCapabilities, InitializeParams,
    InitializeResult, MarkupKind, PublishDiagnosticsClientCapabilities,
    ReferenceClientCapabilities, TextDocumentClientCapabilities,
    TextDocumentSyncClientCapabilities, WorkspaceClientCapabilities, WorkspaceFolder,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    io::{BufReader, BufWriter},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::Duration,
};

pub type NotificationHandler = Arc<dyn Fn(&str, Value) + Send + Sync>;

pub struct LspClient {
    next_id: AtomicI64,
    outbound: Mutex<Option<Sender<Value>>>,
    pending: Arc<Mutex<HashMap<i64, Sender<Result<Value>>>>>,
    child: Mutex<Option<Child>>,
    closed: AtomicBool,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

impl LspClient {
    pub fn spawn(
        server: &LspServerConfig,
        workspace_root: &Path,
        on_notification: NotificationHandler,
    ) -> Result<Arc<Self>> {
        let resolved_cmd = resolve_executable(&server.command);
        let mut cmd = Command::new(&resolved_cmd);
        cmd.args(&server.args)
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to launch LSP server `{}`", server.command))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to capture stdin for `{}`", server.command))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to capture stdout for `{}`", server.command))?;
        let stderr = child.stderr.take();

        let (tx, rx) = mpsc::channel::<Value>();
        let pending: Arc<Mutex<HashMap<i64, Sender<Result<Value>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(24)));

        let client = Arc::new(Self {
            next_id: AtomicI64::new(1),
            outbound: Mutex::new(Some(tx.clone())),
            pending: pending.clone(),
            child: Mutex::new(Some(child)),
            closed: AtomicBool::new(false),
            stderr_tail: stderr_tail.clone(),
        });

        // Writer thread
        thread::Builder::new()
            .name(format!("lsp-write-{}", server.id))
            .spawn(move || {
                let mut writer = BufWriter::new(stdin);
                while let Ok(msg) = rx.recv() {
                    if write_lsp_message(&mut writer, &msg).is_err() {
                        break;
                    }
                }
            })?;

        // Stderr drain thread
        if let Some(err_pipe) = stderr {
            let tail = stderr_tail.clone();
            thread::Builder::new()
                .name(format!("lsp-stderr-{}", server.id))
                .spawn(move || {
                    use std::io::BufRead;
                    let mut reader = BufReader::new(err_pipe);
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                let trimmed = line.trim().to_string();
                                if !trimmed.is_empty() {
                                    let mut q = tail.lock().unwrap();
                                    if q.len() >= 24 {
                                        q.pop_front();
                                    }
                                    q.push_back(trimmed);
                                }
                            }
                        }
                    }
                })?;
        }

        // Reader thread
        let server_settings = server.settings.clone();
        let root_uri = path_to_uri(workspace_root);
        let root_name = workspace_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "workspace".to_string());

        let pending_reader = pending.clone();
        let tx_reply = tx;
        let weak_client = Arc::downgrade(&client);
        thread::Builder::new()
            .name(format!("lsp-read-{}", server.id))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    match read_lsp_message(&mut reader) {
                        Ok(Some(msg)) => {
                            let has_id = msg.get("id").is_some() && !msg["id"].is_null();
                            let has_method = msg.get("method").and_then(|m| m.as_str()).is_some();

                            if has_id && !has_method {
                                // Response to our request
                                if let Some(id) = msg["id"].as_i64() {
                                    if let Some(waiter) =
                                        pending_reader.lock().unwrap().remove(&id)
                                    {
                                        if let Some(err) = msg.get("error").filter(|v| !v.is_null())
                                        {
                                            let message = err
                                                .get("message")
                                                .and_then(|m| m.as_str())
                                                .unwrap_or("LSP error");
                                            let _ = waiter.send(Err(anyhow!("{message}")));
                                        } else {
                                            let _ = waiter.send(Ok(msg["result"].clone()));
                                        }
                                    }
                                }
                            } else if has_id && has_method {
                                // Server -> Client request: always reply promptly so server never stalls
                                let method = msg["method"].as_str().unwrap_or("");
                                let result = handle_server_request(
                                    method,
                                    &msg["params"],
                                    server_settings.as_ref(),
                                    root_uri.as_ref(),
                                    &root_name,
                                );
                                let reply = json!({
                                    "jsonrpc": "2.0",
                                    "id": msg["id"],
                                    "result": result,
                                });
                                let _ = tx_reply.send(reply);
                            } else if has_method {
                                // Server notification
                                if let Some(method) = msg["method"].as_str() {
                                    on_notification(method, msg["params"].clone());
                                }
                            }
                        }
                        Ok(None) | Err(_) => {
                            break;
                        }
                    }
                }

                if let Some(c) = weak_client.upgrade() {
                    c.closed.store(true, Ordering::SeqCst);
                }
                let mut map = pending_reader.lock().unwrap();
                for (_, waiter) in map.drain() {
                    let _ = waiter.send(Err(anyhow!("LSP server closed connection")));
                }
            })?;

        Ok(client)
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn last_stderr(&self) -> String {
        self.stderr_tail
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ")
    }

    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        if self.is_closed() {
            return Err(anyhow!("LSP client is closed"));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);

        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let send_res = {
            let guard = self.outbound.lock().unwrap();
            match guard.as_ref() {
                Some(sender) => sender
                    .send(payload)
                    .map_err(|_| anyhow!("LSP writer channel closed")),
                None => Err(anyhow!("LSP client shut down")),
            }
        };
        if let Err(err) = send_res {
            self.pending.lock().unwrap().remove(&id);
            return Err(err);
        }

        match rx.recv_timeout(timeout) {
            Ok(res) => res,
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                let stderr_hint = self.last_stderr();
                if stderr_hint.is_empty() {
                    Err(anyhow!("LSP request `{method}` timed out"))
                } else {
                    Err(anyhow!("LSP request `{method}` timed out ({stderr_hint})"))
                }
            }
        }
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        if self.is_closed() {
            return Err(anyhow!("LSP client is closed"));
        }
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let guard = self.outbound.lock().unwrap();
        match guard.as_ref() {
            Some(sender) => sender
                .send(payload)
                .map_err(|_| anyhow!("LSP writer channel closed")),
            None => Err(anyhow!("LSP client shut down")),
        }
    }

    pub fn initialize(
        &self,
        workspace_root: &Path,
        server: &LspServerConfig,
        timeout: Duration,
    ) -> Result<InitializeResult> {
        let root_uri = path_to_uri(workspace_root);
        let root_name = workspace_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "workspace".to_string());

        let workspace_folders = root_uri.clone().map(|uri| {
            vec![WorkspaceFolder {
                uri,
                name: root_name,
            }]
        });

        let capabilities = ClientCapabilities {
            workspace: Some(WorkspaceClientCapabilities {
                configuration: Some(true),
                workspace_folders: Some(true),
                ..Default::default()
            }),
            text_document: Some(TextDocumentClientCapabilities {
                synchronization: Some(TextDocumentSyncClientCapabilities {
                    did_save: Some(true),
                    ..Default::default()
                }),
                completion: Some(CompletionClientCapabilities {
                    completion_item: Some(CompletionItemCapability {
                        snippet_support: Some(false),
                        documentation_format: Some(vec![
                            MarkupKind::Markdown,
                            MarkupKind::PlainText,
                        ]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                hover: Some(HoverClientCapabilities {
                    content_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
                    ..Default::default()
                }),
                definition: Some(GotoCapability {
                    link_support: Some(true),
                    ..Default::default()
                }),
                references: Some(ReferenceClientCapabilities {
                    dynamic_registration: Some(false),
                }),
                code_action: Some(CodeActionClientCapabilities {
                    code_action_literal_support: Some(CodeActionLiteralSupport {
                        code_action_kind: CodeActionKindLiteralSupport {
                            value_set: vec![
                                "quickfix".to_string(),
                                "refactor".to_string(),
                                "source".to_string(),
                            ],
                        },
                    }),
                    ..Default::default()
                }),
                publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                    related_information: Some(true),
                    ..Default::default()
                }),
                diagnostic: Some(DiagnosticClientCapabilities::default()),
                ..Default::default()
            }),
            ..Default::default()
        };

        #[allow(deprecated)]
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri,
            workspace_folders,
            initialization_options: server.initialization_options.clone(),
            capabilities,
            client_info: Some(lsp_types::ClientInfo {
                name: crate::brand::APP_NAME.to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            ..Default::default()
        };

        let result_val = self.request("initialize", serde_json::to_value(params)?, timeout)?;
        let init_res: InitializeResult = serde_json::from_value(result_val)?;
        self.notify("initialized", json!({}))?;

        if let Some(settings) = &server.settings {
            let _ = self.notify(
                "workspace/didChangeConfiguration",
                json!({ "settings": settings }),
            );
        }

        Ok(init_res)
    }

    pub fn shutdown(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.request_quiet("shutdown", json!(null), Duration::from_millis(250));
        if let Some(sender) = self.outbound.lock().unwrap().take() {
            let _ = sender.send(json!({
                "jsonrpc": "2.0",
                "method": "exit",
                "params": null
            }));
        }
        if let Some(mut child) = self.child.lock().unwrap().take() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
    }

    fn request_quiet(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Some(sender) = self.outbound.lock().unwrap().as_ref() {
            let _ = sender.send(payload);
        }
        let res = rx
            .recv_timeout(timeout)
            .map_err(|_| anyhow!("timeout"))
            .and_then(|r| r);
        self.pending.lock().unwrap().remove(&id);
        res
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn handle_server_request(
    method: &str,
    params: &Value,
    server_settings: Option<&Value>,
    root_uri: Option<&lsp_types::Uri>,
    root_name: &str,
) -> Value {
    match method {
        "workspace/configuration" => {
            let count = params
                .get("items")
                .and_then(|i| i.as_array())
                .map(|a| a.len())
                .unwrap_or(1);
            let item = server_settings.cloned().unwrap_or_else(|| json!({}));
            Value::Array(vec![item; count])
        }
        "workspace/workspaceFolders" => {
            if let Some(uri) = root_uri {
                json!([{ "uri": uri, "name": root_name }])
            } else {
                Value::Array(vec![])
            }
        }
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create" => Value::Null,
        "workspace/applyEdit" => json!({ "applied": false }),
        _ => Value::Null,
    }
}

pub fn path_to_uri(path: &Path) -> Option<lsp_types::Uri> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let u = url::Url::from_file_path(canonical).ok()?;
    u.as_str().parse().ok()
}

pub fn uri_to_path(uri: &lsp_types::Uri) -> Option<std::path::PathBuf> {
    let parsed = url::Url::parse(uri.as_str()).ok()?;
    parsed.to_file_path().ok()
}

pub fn resolve_executable(command: &str) -> std::path::PathBuf {
    let trimmed = command.trim();
    let candidate = std::path::Path::new(trimmed);
    if trimmed.contains('/') || trimmed.contains('\\') || candidate.is_absolute() {
        return candidate.to_path_buf();
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let full = dir.join(trimmed);
            if full.is_file() {
                return full;
            }
        }
    }
    let mut fallback_dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        fallback_dirs.push(home.join(".local").join("bin"));
        fallback_dirs.push(home.join(".cargo").join("bin"));
        fallback_dirs.push(home.join("brew").join("bin"));
    }
    fallback_dirs.push(std::path::PathBuf::from("/opt/homebrew/bin"));
    fallback_dirs.push(std::path::PathBuf::from("/usr/local/bin"));
    for dir in fallback_dirs {
        let full = dir.join(trimmed);
        if full.is_file() {
            return full;
        }
    }
    candidate.to_path_buf()
}
