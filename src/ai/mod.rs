use crate::{
    config::{AskAiConfig, FimConfig, FimProviderKind},
    index::{WorkspaceIndex, is_ident_char},
    lsp::LspManager,
};
use anyhow::{Context, Result, anyhow};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Component, Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use ureq::Agent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskAiPreset {
    Custom,
    Explain,
    Review,
    Tests,
    Fix,
}

impl AskAiPreset {
    pub fn default_user_label(self) -> &'static str {
        match self {
            Self::Custom => "Ask about the attached code context.",
            Self::Explain => "Explain the selected code context.",
            Self::Review => "Review the selected code for bugs, edge cases, and improvements.",
            Self::Tests => "Write focused unit tests for the selected code.",
            Self::Fix => "Diagnose issues or diagnostics in the selected code and propose fixes.",
        }
    }

    pub fn prompt_instruction(self) -> &'static str {
        match self {
            Self::Custom => "",
            Self::Explain => {
                "Explain clearly what this code does: summarize its purpose, control flow, key types/inputs/outputs, side effects, and non-obvious edge cases."
            }
            Self::Review => {
                "Review this code for correctness bugs, ownership/concurrency pitfalls, error handling gaps, performance issues, and concrete improvements."
            }
            Self::Tests => {
                "Propose concise, idiomatic unit tests covering happy paths, boundary conditions, and error cases for this code."
            }
            Self::Fix => {
                "Identify root causes for any diagnostics, bugs, or fragile invariants in this code and show minimal, exact code fixes."
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeContextAttachment {
    pub id: u64,
    pub path: PathBuf,
    pub rel_path: String,
    pub language: String,
    /// 1-based inclusive start line.
    pub start_line: u32,
    /// 1-based inclusive end line.
    pub end_line: u32,
    pub code: String,
    pub enclosing_symbol: Option<String>,
    pub from_selection: bool,
}

impl CodeContextAttachment {
    pub fn line_span_label(&self) -> String {
        if self.start_line == self.end_line {
            format!("L{}", self.start_line)
        } else {
            format!("L{}-{}", self.start_line, self.end_line)
        }
    }

    pub fn chip_label(&self) -> String {
        format!("{}:{}", self.rel_path, self.line_span_label())
    }

    pub fn line_count(&self) -> usize {
        self.end_line.saturating_sub(self.start_line) as usize + 1
    }

    pub fn same_span(&self, other: &Self) -> bool {
        self.path == other.path
            && self.start_line == other.start_line
            && self.end_line == other.end_line
            && self.code == other.code
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CodeLinkTarget {
    pub path: PathBuf,
    pub rel_path: String,
    /// 1-based inclusive start line.
    pub start_line: u32,
    /// 1-based inclusive optional end line.
    pub end_line: Option<u32>,
    /// 1-based optional column.
    pub col: Option<u32>,
}

impl CodeLinkTarget {
    pub fn display_label(&self) -> String {
        match (self.end_line, self.col) {
            (Some(end), _) if end > self.start_line => {
                format!("{}:L{}-{}", self.rel_path, self.start_line, end)
            }
            (_, Some(col)) if col > 1 => {
                format!("{}:L{}:{}", self.rel_path, self.start_line, col)
            }
            _ => format!("{}:L{}", self.rel_path, self.start_line),
        }
    }

    pub fn to_kestrel_url(&self) -> String {
        let mut url = format!(
            "kestrel://open?path={}&line={}",
            percent_encode(&self.rel_path),
            self.start_line.max(1)
        );
        if let Some(end) = self.end_line.filter(|e| *e > self.start_line) {
            url.push_str(&format!("&end={end}"));
        }
        if let Some(col) = self.col.filter(|c| *c > 1) {
            url.push_str(&format!("&col={col}"));
        }
        url
    }

    pub fn markdown_link(&self) -> String {
        format!("[{}]({})", self.display_label(), self.to_kestrel_url())
    }

    /// 0-based `(line, col)` for `KestrelIde::open_file`.
    pub fn zero_based_pos(&self) -> (u32, u32) {
        (
            self.start_line.saturating_sub(1),
            self.col.unwrap_or(1).saturating_sub(1),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStepKind {
    Thought,
    ToolCall,
    ToolResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStepStatus {
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone)]
pub struct AgentTraceStep {
    pub id: u64,
    pub kind: AgentStepKind,
    pub tool_name: Option<String>,
    pub title: String,
    pub detail: String,
    pub citations: Vec<CodeLinkTarget>,
    pub status: AgentStepStatus,
}

#[derive(Debug, Clone)]
pub enum AskAiStreamEvent {
    StepAdded(AgentTraceStep),
    StepUpdated {
        step_id: u64,
        detail_append: Option<String>,
        replace_detail: Option<String>,
        status: Option<AgentStepStatus>,
        citations: Vec<CodeLinkTarget>,
    },
    ContentDelta(String),
    Finished {
        provider_label: String,
        latency_ms: u64,
        is_error: bool,
    },
}

#[derive(Debug, Clone)]
pub struct AskAiMessage {
    pub id: u64,
    pub role: ChatRole,
    pub content: String,
    pub contexts: Vec<CodeContextAttachment>,
    pub trace_steps: Vec<AgentTraceStep>,
    pub citations: Vec<CodeLinkTarget>,
    pub streaming: bool,
    pub provider_label: Option<String>,
    pub latency_ms: Option<u64>,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatBackendTarget {
    OpenAiCompatible {
        provider_label: String,
        endpoint: String,
        model: String,
        api_key: Option<String>,
    },
    Ollama {
        endpoint: String,
        model: String,
        api_key: Option<String>,
    },
    LocalExplain,
}

impl ChatBackendTarget {
    pub fn badge_label(&self) -> String {
        match self {
            Self::OpenAiCompatible {
                provider_label,
                model,
                ..
            } => format!("{provider_label} · {model}"),
            Self::Ollama { model, .. } => format!("Ollama · {model}"),
            Self::LocalExplain => "Local AST Explain".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AskAiRequest {
    pub prompt: String,
    pub preset: AskAiPreset,
    pub contexts: Vec<CodeContextAttachment>,
    pub related_definitions: Vec<String>,
    pub diagnostics_in_scope: Vec<String>,
    pub history: Vec<(ChatRole, String)>,
    pub workspace_root: PathBuf,
    pub index: Option<WorkspaceIndex>,
    pub explore_repo: bool,
    pub fim_config: FimConfig,
    pub ask_config: AskAiConfig,
}

#[derive(Debug, Clone)]
pub struct AskAiResponse {
    pub content: String,
    pub trace_steps: Vec<AgentTraceStep>,
    pub citations: Vec<CodeLinkTarget>,
    pub provider_label: String,
    pub latency_ms: u64,
    pub is_error: bool,
}

pub fn resolve_chat_target(ask_cfg: &AskAiConfig, fim_cfg: &FimConfig) -> ChatBackendTarget {
    let custom_ep = ask_cfg.endpoint.trim();
    let custom_model = ask_cfg.model.trim();
    let custom_key = ask_cfg
        .api_key
        .trim()
        .is_empty()
        .then(|| None)
        .unwrap_or_else(|| Some(ask_cfg.api_key.trim().to_string()));

    if !custom_ep.is_empty() {
        let model = if !custom_model.is_empty() {
            custom_model.to_string()
        } else if !fim_cfg.model.trim().is_empty() {
            fim_cfg.model.clone()
        } else {
            "deepseek-chat".to_string()
        };
        let (eff_prov, _, _) = fim_cfg.resolve_auto_target();
        let api_key = custom_key.or_else(|| fim_cfg.resolved_api_key(eff_prov));
        if custom_ep.contains("11434") || custom_ep.ends_with("/api/chat") {
            return ChatBackendTarget::Ollama {
                endpoint: normalize_ollama_chat_url(custom_ep),
                model,
                api_key,
            };
        }
        let endpoint = if custom_ep.contains("deepseek.com") {
            derive_deepseek_chat_url(custom_ep)
        } else {
            normalize_chat_completions_url(custom_ep)
        };
        let provider_label = if endpoint.contains("deepseek") {
            "DeepSeek"
        } else {
            "Custom Chat"
        };
        return ChatBackendTarget::OpenAiCompatible {
            provider_label: provider_label.to_string(),
            endpoint,
            model,
            api_key,
        };
    }

    let (eff_provider, fim_ep, fim_model) = fim_cfg.resolve_auto_target();
    match eff_provider {
        FimProviderKind::DeepSeek => {
            let endpoint = derive_deepseek_chat_url(&fim_ep);
            let model = derive_deepseek_chat_model(&fim_model, custom_model);
            let api_key = custom_key.or_else(|| fim_cfg.resolved_api_key(FimProviderKind::DeepSeek));
            ChatBackendTarget::OpenAiCompatible {
                provider_label: "DeepSeek".to_string(),
                endpoint,
                model,
                api_key,
            }
        }
        FimProviderKind::OpenAiChat => ChatBackendTarget::OpenAiCompatible {
            provider_label: "OpenAI Chat".to_string(),
            endpoint: normalize_chat_completions_url(&fim_ep),
            model: pick_non_empty(custom_model, &fim_model, "gpt-4o-mini"),
            api_key: custom_key.or_else(|| fim_cfg.resolved_api_key(FimProviderKind::OpenAiChat)),
        },
        FimProviderKind::OpenAiCompletions => ChatBackendTarget::OpenAiCompatible {
            provider_label: "OpenAI Compatible".to_string(),
            endpoint: normalize_chat_completions_url(&fim_ep),
            model: pick_non_empty(custom_model, &fim_model, "qwen2.5-coder"),
            api_key: custom_key
                .or_else(|| fim_cfg.resolved_api_key(FimProviderKind::OpenAiCompletions)),
        },
        FimProviderKind::Codestral => {
            let base = if fim_ep.contains("codestral.mistral.ai") {
                "https://api.mistral.ai/v1/chat/completions"
            } else {
                &fim_ep
            };
            ChatBackendTarget::OpenAiCompatible {
                provider_label: "Mistral".to_string(),
                endpoint: normalize_chat_completions_url(base),
                model: pick_non_empty(custom_model, &fim_model, "codestral-latest"),
                api_key: custom_key.or_else(|| fim_cfg.resolved_api_key(FimProviderKind::Codestral)),
            }
        }
        FimProviderKind::Ollama => ChatBackendTarget::Ollama {
            endpoint: normalize_ollama_chat_url(&fim_ep),
            model: pick_non_empty(custom_model, &fim_model, "qwen2.5-coder:7b"),
            api_key: custom_key.or_else(|| fim_cfg.resolved_api_key(FimProviderKind::Ollama)),
        },
        FimProviderKind::Auto | FimProviderKind::LocalAst => {
            if let Some(key) = custom_key {
                ChatBackendTarget::OpenAiCompatible {
                    provider_label: "OpenAI Chat".to_string(),
                    endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
                    model: pick_non_empty(custom_model, "", "gpt-4o-mini"),
                    api_key: Some(key),
                }
            } else {
                ChatBackendTarget::LocalExplain
            }
        }
    }
}

fn pick_non_empty(first: &str, second: &str, fallback: &str) -> String {
    if !first.trim().is_empty() {
        first.trim().to_string()
    } else if !second.trim().is_empty() {
        second.trim().to_string()
    } else {
        fallback.to_string()
    }
}

fn derive_deepseek_chat_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        return trimmed.to_string();
    }
    let root = trimmed
        .strip_suffix("/beta/completions")
        .or_else(|| trimmed.strip_suffix("/completions"))
        .or_else(|| trimmed.strip_suffix("/beta"))
        .or_else(|| trimmed.strip_suffix("/v1"))
        .unwrap_or(trimmed);
    format!("{root}/chat/completions")
}

fn derive_deepseek_chat_model(fim_model: &str, custom_model: &str) -> String {
    if !custom_model.trim().is_empty() {
        return custom_model.trim().to_string();
    }
    let m = fim_model.trim();
    if m == "deepseek-chat" || m == "deepseek-reasoner" {
        m.to_string()
    } else {
        "deepseek-chat".to_string()
    }
}

fn normalize_chat_completions_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        return trimmed.to_string();
    }
    let without_comp = trimmed
        .strip_suffix("/fim/completions")
        .or_else(|| trimmed.strip_suffix("/completions"))
        .unwrap_or(trimmed);
    format!("{without_comp}/chat/completions")
}

fn normalize_ollama_chat_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/api/chat") {
        return trimmed.to_string();
    }
    let root = trimmed.strip_suffix("/api/generate").unwrap_or(trimmed);
    format!("{root}/api/chat")
}

pub fn collect_related_symbol_notes(
    contexts: &[CodeContextAttachment],
    index: &WorkspaceIndex,
    max_items: usize,
) -> Vec<String> {
    if contexts.is_empty() || max_items == 0 {
        return Vec::new();
    }
    let mut seen_idents = HashSet::new();
    let mut notes = Vec::new();
    let mut seen_locs = HashSet::new();

    for ctx in contexts {
        let mut token = String::new();
        for ch in ctx.code.chars().chain(std::iter::once(' ')) {
            if is_ident_char(ch) {
                token.push(ch);
            } else if !token.is_empty() {
                if token.len() >= 2 && seen_idents.insert(token.clone()) {
                    for def in index
                        .find_definitions_for_name(&ctx.path, &token, None)
                        .into_iter()
                        .take(2)
                    {
                        let same_selection = def.path == ctx.path
                            && def.start_line + 1 >= ctx.start_line
                            && def.start_line + 1 <= ctx.end_line;
                        if same_selection {
                            continue;
                        }
                        let loc_key = format!("{}:{}", def.rel_path, def.start_line);
                        if seen_locs.insert(loc_key) {
                            let sig = if def.signature.is_empty() {
                                &def.symbol_name
                            } else {
                                &def.signature
                            };
                            notes.push(format!(
                                "`{}` (`{}` at {}:L{})",
                                sig,
                                def.kind.label(),
                                def.rel_path,
                                def.start_line + 1
                            ));
                            if notes.len() >= max_items {
                                return notes;
                            }
                        }
                    }
                }
                token.clear();
            }
        }
    }
    notes
}

pub fn collect_scope_diagnostics(
    contexts: &[CodeContextAttachment],
    index: &WorkspaceIndex,
    lsp: &LspManager,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for ctx in contexts {
        for d in lsp.diagnostics_for(&ctx.path) {
            let line = d.range.start.line + 1;
            if line >= ctx.start_line.saturating_sub(2) && line <= ctx.end_line.saturating_add(2) {
                let entry = format!("{}:L{} — {}", ctx.rel_path, line, d.message);
                if seen.insert(entry.clone()) {
                    out.push(entry);
                }
            }
        }
        for d in index.file_diagnostics(&ctx.path) {
            let line = d.line + 1;
            if line >= ctx.start_line.saturating_sub(2) && line <= ctx.end_line.saturating_add(2) {
                let entry = format!("{}:L{} — {}", ctx.rel_path, line, d.message);
                if seen.insert(entry.clone()) {
                    out.push(entry);
                }
            }
        }
        if out.len() >= 10 {
            break;
        }
    }
    out
}

pub fn known_rel_paths_for_request(
    workspace_root: &Path,
    index: Option<&WorkspaceIndex>,
    contexts: &[CodeContextAttachment],
) -> HashSet<String> {
    let mut set = index.map(WorkspaceIndex::all_rel_paths).unwrap_or_default();
    for ctx in contexts {
        if !ctx.rel_path.is_empty() {
            set.insert(ctx.rel_path.replace('\\', "/"));
        } else if let Ok(rel) = ctx.path.strip_prefix(workspace_root) {
            set.insert(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    set
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
            out.push(bytes[i]);
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn code_ref_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?P<tick>`?)(?P<path>[A-Za-z0-9_.\-/\\]+?\.[A-Za-z0-9_]+)(?::[Ll]?(?P<line1>\d+)(?:-[Ll]?(?P<end1>\d+))?(?::(?P<col1>\d+))?|#[Ll](?P<line2>\d+)(?:-[Ll]?(?P<end2>\d+))?)(?P<endtick>`?)",
        )
        .expect("valid code_ref_regex")
    })
}

fn safe_relative_clean(raw: &str) -> Option<String> {
    let normalized = raw.trim().replace('\\', "/");
    let stripped = normalized.strip_prefix("./").unwrap_or(&normalized);
    if stripped.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for comp in Path::new(stripped).components() {
        match comp {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn resolve_known_workspace_path(
    raw_path: &str,
    workspace_root: &Path,
    known_rel_paths: &HashSet<String>,
) -> Option<(PathBuf, String)> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return None;
    }
    if Path::new(trimmed).is_absolute() {
        let abs = PathBuf::from(trimmed);
        let cand = fs::canonicalize(&abs).unwrap_or(abs);
        let root_canon = fs::canonicalize(workspace_root).ok();
        if let Some(root) = root_canon.as_ref() {
            if let Ok(rel) = cand.strip_prefix(root) {
                let rel_s = rel.to_string_lossy().replace('\\', "/");
                if known_rel_paths.contains(&rel_s) || cand.is_file() {
                    return Some((cand, rel_s));
                }
            }
        }
        return None;
    }

    let rel = safe_relative_clean(trimmed)?;
    if known_rel_paths.contains(&rel) {
        let joined = if workspace_root.as_os_str().is_empty() {
            PathBuf::from(&rel)
        } else {
            let candidate = workspace_root.join(&rel);
            fs::canonicalize(&candidate).unwrap_or(candidate)
        };
        return Some((joined, rel));
    }
    if let Some(matched) = known_rel_paths
        .iter()
        .find(|p| p.ends_with(&format!("/{rel}")))
    {
        let joined = if workspace_root.as_os_str().is_empty() {
            PathBuf::from(matched)
        } else {
            let candidate = workspace_root.join(matched);
            fs::canonicalize(&candidate).unwrap_or(candidate)
        };
        return Some((joined, matched.clone()));
    }
    if !workspace_root.as_os_str().is_empty() {
        let candidate = workspace_root.join(&rel);
        if candidate.is_file() {
            if let (Ok(root_canon), Ok(cand_canon)) =
                (fs::canonicalize(workspace_root), fs::canonicalize(&candidate))
            {
                if cand_canon.starts_with(&root_canon) {
                    return Some((cand_canon, rel));
                }
            }
        }
    }
    None
}

pub fn parse_code_link_url(
    raw_url: &str,
    workspace_root: &Path,
    known_rel_paths: &HashSet<String>,
) -> Option<CodeLinkTarget> {
    let url = raw_url.trim();
    if let Some(rest) = url
        .strip_prefix("kestrel://open?")
        .or_else(|| url.strip_prefix("flint://open?"))
    {
        let mut raw_path = None;
        let mut line = None;
        let mut end = None;
        let mut col = None;
        for pair in rest.split('&') {
            let Some((k, v)) = pair.split_once('=') else {
                continue;
            };
            match k {
                "path" => raw_path = Some(percent_decode(v)),
                "line" => line = v.parse::<u32>().ok(),
                "end" => end = v.parse::<u32>().ok(),
                "col" => col = v.parse::<u32>().ok(),
                _ => {}
            }
        }
        let raw_p = raw_path?;
        let (path, rel_path) =
            resolve_known_workspace_path(&raw_p, workspace_root, known_rel_paths).or_else(
                || {
                    let clean = safe_relative_clean(&raw_p)?;
                    let full = if workspace_root.as_os_str().is_empty() {
                        PathBuf::from(&clean)
                    } else {
                        workspace_root.join(&clean)
                    };
                    Some((full, clean))
                },
            )?;
        return Some(CodeLinkTarget {
            path,
            rel_path,
            start_line: line.unwrap_or(1).max(1),
            end_line: end,
            col,
        });
    }

    if let Some(rest) = url.strip_prefix("file://") {
        let decoded = percent_decode(rest);
        let (path_str, line) = if let Some((p, frag)) = decoded.split_once('#') {
            let l = frag.trim_start_matches(['L', 'l']).parse::<u32>().ok();
            (p, l)
        } else if let Some((p, l_str)) = decoded.rsplit_once(':') {
            if let Ok(l) = l_str.parse::<u32>() {
                (p, Some(l))
            } else {
                (decoded.as_str(), None)
            }
        } else {
            (decoded.as_str(), None)
        };
        let (path, rel_path) =
            resolve_known_workspace_path(path_str, workspace_root, known_rel_paths)?;
        return Some(CodeLinkTarget {
            path,
            rel_path,
            start_line: line.unwrap_or(1).max(1),
            end_line: None,
            col: None,
        });
    }

    if let Some(cap) = code_ref_regex().captures(url) {
        let raw_p = cap.name("path")?.as_str();
        let line = cap
            .name("line1")
            .or_else(|| cap.name("line2"))?
            .as_str()
            .parse::<u32>()
            .ok()?
            .max(1);
        let end_line = cap
            .name("end1")
            .or_else(|| cap.name("end2"))
            .and_then(|m| m.as_str().parse::<u32>().ok());
        let col = cap.name("col1").and_then(|m| m.as_str().parse::<u32>().ok());
        let (path, rel_path) =
            resolve_known_workspace_path(raw_p, workspace_root, known_rel_paths)?;
        return Some(CodeLinkTarget {
            path,
            rel_path,
            start_line: line,
            end_line,
            col,
        });
    }

    None
}

fn existing_markdown_link_spans(line: &str) -> Vec<(usize, usize)> {
    static MD_LINK_RE: OnceLock<Regex> = OnceLock::new();
    let re = MD_LINK_RE.get_or_init(|| {
        Regex::new(r"\[[^\]]+\]\([^)]+\)").expect("valid markdown link regex")
    });
    re.find_iter(line).map(|m| (m.start(), m.end())).collect()
}

pub fn linkify_code_references(
    markdown: &str,
    workspace_root: &Path,
    known_rel_paths: &HashSet<String>,
) -> String {
    if markdown.is_empty() || known_rel_paths.is_empty() && workspace_root.as_os_str().is_empty() {
        return markdown.to_string();
    }
    let re = code_ref_regex();
    let mut out = String::with_capacity(markdown.len() + 64);
    let mut in_fence = false;
    let mut fence_marker = String::new();

    for segment in markdown.split_inclusive('\n') {
        let trimmed = segment.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let marker: String = trimmed.chars().take(3).collect();
            if !in_fence {
                in_fence = true;
                fence_marker = marker;
            } else if marker == fence_marker {
                in_fence = false;
                fence_marker.clear();
            }
            out.push_str(segment);
            continue;
        }
        if in_fence {
            out.push_str(segment);
            continue;
        }

        let protected = existing_markdown_link_spans(segment);
        let mut cursor = 0usize;
        for cap in re.captures_iter(segment) {
            let Some(m) = cap.get(0) else {
                continue;
            };
            if protected
                .iter()
                .any(|(ps, pe)| m.start() < *pe && m.end() > *ps)
            {
                continue;
            }
            let tick = cap.name("tick").map(|x| x.as_str()).unwrap_or("");
            let endtick = cap.name("endtick").map(|x| x.as_str()).unwrap_or("");
            if tick != endtick {
                continue;
            }
            let raw_p = cap.name("path").map(|x| x.as_str()).unwrap_or("");
            let Some(line) = cap
                .name("line1")
                .or_else(|| cap.name("line2"))
                .and_then(|x| x.as_str().parse::<u32>().ok())
            else {
                continue;
            };
            let end_line = cap
                .name("end1")
                .or_else(|| cap.name("end2"))
                .and_then(|x| x.as_str().parse::<u32>().ok());
            let col = cap.name("col1").and_then(|x| x.as_str().parse::<u32>().ok());
            let Some((path, rel_path)) =
                resolve_known_workspace_path(raw_p, workspace_root, known_rel_paths)
            else {
                continue;
            };
            let target = CodeLinkTarget {
                path,
                rel_path,
                start_line: line.max(1),
                end_line,
                col,
            };
            out.push_str(&segment[cursor..m.start()]);
            out.push_str(&target.markdown_link());
            cursor = m.end();
        }
        out.push_str(&segment[cursor..]);
    }
    out
}

pub fn extract_code_references(
    markdown: &str,
    workspace_root: &Path,
    known_rel_paths: &HashSet<String>,
    max_items: usize,
) -> Vec<CodeLinkTarget> {
    if markdown.is_empty() || max_items == 0 {
        return Vec::new();
    }
    static KESTREL_URL_RE: OnceLock<Regex> = OnceLock::new();
    let kestrel_re = KESTREL_URL_RE.get_or_init(|| {
        Regex::new(r"(?:kestrel|flint)://open\?[^\s)>]+").expect("valid code link url regex")
    });

    let mut ordered: Vec<(usize, CodeLinkTarget)> = Vec::new();
    for m in kestrel_re.find_iter(markdown) {
        if let Some(target) = parse_code_link_url(m.as_str(), workspace_root, known_rel_paths) {
            ordered.push((m.start(), target));
        }
    }
    for cap in code_ref_regex().captures_iter(markdown) {
        let Some(m) = cap.get(0) else {
            continue;
        };
        if let Some(target) = parse_code_link_url(
            m.as_str().trim_matches('`'),
            workspace_root,
            known_rel_paths,
        ) {
            ordered.push((m.start(), target));
        }
    }
    ordered.sort_by_key(|(pos, _)| *pos);

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (_, target) in ordered {
        let key = (target.rel_path.clone(), target.start_line);
        if seen.insert(key) {
            out.push(target);
            if out.len() >= max_items {
                break;
            }
        }
    }
    out
}

pub fn build_ask_ai_messages(req: &AskAiRequest) -> Vec<(String, String)> {
    let mut messages = Vec::new();
    let react_clause = if req.explore_repo && req.index.is_some() {
        " You can invoke repository exploration tools (`list_files`, `search_symbols`, `grep_text`, `read_lines`, `find_references`) in ReAct steps to inspect callers, definitions, and neighboring files before finalizing your answer."
    } else {
        ""
    };
    let system = format!(
        "You are Flint AI, a concise, accurate coding assistant embedded in Flint IDE. Explain code structure, control flow, types, invariants, and edge cases clearly using Markdown.{react_clause} Always cite concrete file paths and 1-based line numbers using clickable Markdown links `[relative/path:L<line>](kestrel://open?path=relative/path&line=<line>)` so the user can jump directly to each referenced line. Only state symbol names, paths, line numbers, and behavior you directly observed."
    );
    messages.push(("system".to_string(), system));

    let history_start = req.history.len().saturating_sub(6);
    for (role, text) in &req.history[history_start..] {
        if !text.trim().is_empty() {
            let role_str = match role {
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
            };
            messages.push((role_str.to_string(), text.clone()));
        }
    }

    let mut user_body = String::new();
    let preset_instr = req.preset.prompt_instruction();
    let trimmed_prompt = req.prompt.trim();
    if !trimmed_prompt.is_empty() {
        user_body.push_str(trimmed_prompt);
        if !preset_instr.is_empty() && trimmed_prompt != req.preset.default_user_label() {
            user_body.push_str("\n\nInstruction: ");
            user_body.push_str(preset_instr);
        }
    } else if !preset_instr.is_empty() {
        user_body.push_str(preset_instr);
    } else {
        user_body.push_str(AskAiPreset::Explain.prompt_instruction());
    }

    if !req.contexts.is_empty() {
        user_body.push_str("\n\n### Attached Code Context\n");
        for (ix, ctx) in req.contexts.iter().enumerate() {
            let scope = ctx
                .enclosing_symbol
                .as_deref()
                .map(|s| format!(" · enclosing `{s}`"))
                .unwrap_or_default();
            user_body.push_str(&format!(
                "\n#### {}. `{}` ({}{})\n```{}\n{}\n```\n",
                ix + 1,
                ctx.chip_label(),
                ctx.language,
                scope,
                ctx.language,
                ctx.code.trim_end_matches('\n')
            ));
        }
    }

    if !req.related_definitions.is_empty() {
        user_body.push_str("\n### Related Workspace Symbols\n");
        for def in &req.related_definitions {
            user_body.push_str(&format!("- {def}\n"));
        }
    }

    if !req.diagnostics_in_scope.is_empty() {
        user_body.push_str("\n### Diagnostics In Scope\n");
        for diag in &req.diagnostics_in_scope {
            user_body.push_str(&format!("- {diag}\n"));
        }
    }

    messages.push(("user".to_string(), user_body));
    messages
}

pub fn repo_tool_definitions_json() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "list_files",
                "description": "List files in the repository matching an optional path substring or filename query.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Optional fuzzy substring or folder prefix to filter file paths." },
                        "limit": { "type": "integer", "description": "Maximum number of file entries (default 25)." }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search_symbols",
                "description": "Search workspace symbols (functions, structs, classes, traits, methods) across the repository. Supports regex mode.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Symbol name query or regex pattern." },
                        "regex": { "type": "boolean", "description": "Set true to treat query as a regex pattern." }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "grep_text",
                "description": "Search text or regex patterns across repository source files, returning file paths and 1-based line numbers.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Substring or regex pattern to match." },
                        "regex": { "type": "boolean", "description": "Set true to treat pattern as a regex." },
                        "path_filter": { "type": "string", "description": "Optional substring filter on file paths." }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_lines",
                "description": "Read a range of 1-based numbered lines from a file inside the repository.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Workspace-relative path to read." },
                        "start_line": { "type": "integer", "description": "1-based start line (default 1)." },
                        "end_line": { "type": "integer", "description": "1-based inclusive end line (max 120 lines per read)." }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "find_references",
                "description": "Find definitions and callers/references for a symbol name across all repository files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "symbol": { "type": "string", "description": "Exact symbol name to find definitions and call sites for." }
                    },
                    "required": ["symbol"]
                }
            }
        }
    ])
}

#[derive(Debug, Clone)]
pub struct ToolExecutionOutput {
    pub tool_result_text: String,
    pub trace_summary: String,
    pub citations: Vec<CodeLinkTarget>,
    pub is_error: bool,
}

fn read_repo_file_content(
    raw_path: &str,
    workspace_root: &Path,
    index: Option<&WorkspaceIndex>,
) -> Result<(PathBuf, String, String)> {
    if let Some(idx) = index {
        if let Some(found) = idx.file_snapshot_by_rel_or_path(raw_path) {
            return Ok(found);
        }
    }
    let known = index.map(WorkspaceIndex::all_rel_paths).unwrap_or_default();
    if let Some((abs, rel)) = resolve_known_workspace_path(raw_path, workspace_root, &known) {
        let content = fs::read_to_string(&abs)
            .with_context(|| format!("Failed to read `{rel}`"))?;
        return Ok((abs, rel, content));
    }
    Err(anyhow!(
        "Path `{raw_path}` is outside workspace root or does not exist"
    ))
}

pub fn run_repo_tool(
    tool_name: &str,
    args_json: &str,
    workspace_root: &Path,
    index: Option<&WorkspaceIndex>,
) -> ToolExecutionOutput {
    let args: Value = serde_json::from_str(args_json.trim()).unwrap_or_else(|_| json!({}));
    let Some(idx) = index else {
        return ToolExecutionOutput {
            tool_result_text: "Workspace index unavailable for repository tools.".to_string(),
            trace_summary: "Workspace index unavailable".to_string(),
            citations: Vec::new(),
            is_error: true,
        };
    };

    match tool_name {
        "list_files" => {
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(25)
                .clamp(1, 60) as usize;
            let hits: Vec<_> = idx.search_files(pattern).into_iter().take(limit).collect();
            if hits.is_empty() {
                return ToolExecutionOutput {
                    tool_result_text: format!("No files matching `{pattern}`."),
                    trace_summary: "0 files matched".to_string(),
                    citations: Vec::new(),
                    is_error: false,
                };
            }
            let mut lines = Vec::new();
            let mut citations = Vec::new();
            for h in &hits {
                lines.push(format!(
                    "- {} ({}, {} symbols)",
                    h.rel_path, h.language, h.symbol_count
                ));
                if citations.len() < 6 {
                    citations.push(CodeLinkTarget {
                        path: h.path.clone(),
                        rel_path: h.rel_path.clone(),
                        start_line: 1,
                        end_line: None,
                        col: None,
                    });
                }
            }
            ToolExecutionOutput {
                tool_result_text: lines.join("\n"),
                trace_summary: format!("Listed {} file(s)", hits.len()),
                citations,
                is_error: false,
            }
        }
        "search_symbols" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let regex = args.get("regex").and_then(|v| v.as_bool()).unwrap_or(false);
            match idx.search_symbols_query(query, regex) {
                Ok(hits) => {
                    let top: Vec<_> = hits.into_iter().take(15).collect();
                    if top.is_empty() {
                        return ToolExecutionOutput {
                            tool_result_text: format!("No symbols matched `{query}`."),
                            trace_summary: format!("0 symbol hits for `{query}`"),
                            citations: Vec::new(),
                            is_error: false,
                        };
                    }
                    let mut out = Vec::new();
                    let mut citations = Vec::new();
                    for h in &top {
                        let line = h.location.start_line + 1;
                        let target = CodeLinkTarget {
                            path: h.location.path.clone(),
                            rel_path: h.location.rel_path.clone(),
                            start_line: line,
                            end_line: Some(h.location.end_line + 1),
                            col: Some(h.location.start_col + 1),
                        };
                        let sig = if h.location.signature.is_empty() {
                            &h.location.symbol_name
                        } else {
                            &h.location.signature
                        };
                        out.push(format!(
                            "- {} (`{}`): `{}`",
                            target.markdown_link(),
                            h.location.kind.label(),
                            sig
                        ));
                        if citations.len() < 8 {
                            citations.push(target);
                        }
                    }
                    ToolExecutionOutput {
                        tool_result_text: out.join("\n"),
                        trace_summary: format!("{} symbol match(es) for `{query}`", top.len()),
                        citations,
                        is_error: false,
                    }
                }
                Err(err) => ToolExecutionOutput {
                    tool_result_text: err.clone(),
                    trace_summary: err,
                    citations: Vec::new(),
                    is_error: true,
                },
            }
        }
        "grep_text" => {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let regex = args.get("regex").and_then(|v| v.as_bool()).unwrap_or(false);
            let path_filter = args.get("path_filter").and_then(|v| v.as_str());
            match idx.search_text_regex(pattern, regex, path_filter, 20) {
                Ok(hits) => {
                    if hits.is_empty() {
                        return ToolExecutionOutput {
                            tool_result_text: format!("No text matches for `{pattern}`."),
                            trace_summary: format!("0 matches for `{pattern}`"),
                            citations: Vec::new(),
                            is_error: false,
                        };
                    }
                    let mut out = Vec::new();
                    let mut citations = Vec::new();
                    for h in &hits {
                        let line = h.line + 1;
                        let target = CodeLinkTarget {
                            path: h.path.clone(),
                            rel_path: h.rel_path.clone(),
                            start_line: line,
                            end_line: None,
                            col: Some(h.col + 1),
                        };
                        out.push(format!("- {}: `{}`", target.markdown_link(), h.line_text));
                        if citations.len() < 8 {
                            citations.push(target);
                        }
                    }
                    ToolExecutionOutput {
                        tool_result_text: out.join("\n"),
                        trace_summary: format!("{} match(es) for `{pattern}`", hits.len()),
                        citations,
                        is_error: false,
                    }
                }
                Err(err) => ToolExecutionOutput {
                    tool_result_text: err.clone(),
                    trace_summary: err,
                    citations: Vec::new(),
                    is_error: true,
                },
            }
        }
        "read_lines" => {
            let raw_path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let start = args
                .get("start_line")
                .and_then(|v| v.as_u64())
                .unwrap_or(1)
                .max(1) as u32;
            let requested_end = args
                .get("end_line")
                .and_then(|v| v.as_u64())
                .unwrap_or((start as u64) + 59) as u32;
            let end = requested_end.clamp(start, start.saturating_add(119));
            match read_repo_file_content(raw_path, workspace_root, Some(idx)) {
                Ok((abs, rel, content)) => {
                    let all_lines: Vec<&str> = content.lines().collect();
                    let total = all_lines.len() as u32;
                    let actual_start = start.min(total.max(1));
                    let actual_end = end.min(total);
                    let mut numbered = Vec::new();
                    for line_no in actual_start..=actual_end {
                        if let Some(txt) = all_lines.get((line_no - 1) as usize) {
                            numbered.push(format!("{line_no:>4} | {txt}"));
                        }
                    }
                    let target = CodeLinkTarget {
                        path: abs,
                        rel_path: rel.clone(),
                        start_line: actual_start,
                        end_line: Some(actual_end),
                        col: None,
                    };
                    ToolExecutionOutput {
                        tool_result_text: format!(
                            "File {} (lines {}-{} of {total}):\n```\n{}\n```",
                            target.markdown_link(),
                            actual_start,
                            actual_end,
                            numbered.join("\n")
                        ),
                        trace_summary: format!(
                            "Read {}:L{}-{}",
                            rel, actual_start, actual_end
                        ),
                        citations: vec![target],
                        is_error: false,
                    }
                }
                Err(err) => ToolExecutionOutput {
                    tool_result_text: format!("{err}"),
                    trace_summary: format!("{err}"),
                    citations: Vec::new(),
                    is_error: true,
                },
            }
        }
        "find_references" => {
            let symbol = args.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
            let hits: Vec<_> = idx
                .find_references(symbol, None)
                .into_iter()
                .take(20)
                .collect();
            if hits.is_empty() {
                return ToolExecutionOutput {
                    tool_result_text: format!("No references found for `{symbol}`."),
                    trace_summary: format!("0 references for `{symbol}`"),
                    citations: Vec::new(),
                    is_error: false,
                };
            }
            let mut out = Vec::new();
            let mut citations = Vec::new();
            for h in &hits {
                let line = h.line + 1;
                let tag = if h.is_definition {
                    "def"
                } else if h.is_call {
                    "call"
                } else {
                    "ref"
                };
                let target = CodeLinkTarget {
                    path: h.path.clone(),
                    rel_path: h.rel_path.clone(),
                    start_line: line,
                    end_line: None,
                    col: Some(h.col + 1),
                };
                out.push(format!(
                    "- [{tag}] {}: `{}`",
                    target.markdown_link(),
                    h.line_text.trim()
                ));
                if citations.len() < 8 {
                    citations.push(target);
                }
            }
            ToolExecutionOutput {
                tool_result_text: out.join("\n"),
                trace_summary: format!("{} reference(s) for `{symbol}`", hits.len()),
                citations,
                is_error: false,
            }
        }
        other => ToolExecutionOutput {
            tool_result_text: format!("Unknown read-only tool `{other}`."),
            trace_summary: format!("Unknown tool `{other}`"),
            citations: Vec::new(),
            is_error: true,
        },
    }
}

pub fn execute_ask_ai(req: &AskAiRequest) -> AskAiResponse {
    let (tx, rx) = async_channel::unbounded();
    let cancel = AtomicBool::new(false);
    let mut resp = execute_ask_ai_streaming(req, &tx, &cancel);
    drop(tx);
    while rx.try_recv().is_ok() {}
    if resp.citations.is_empty() {
        let known =
            known_rel_paths_for_request(&req.workspace_root, req.index.as_ref(), &req.contexts);
        resp.citations = extract_code_references(&resp.content, &req.workspace_root, &known, 12);
    }
    resp
}

pub fn execute_ask_ai_streaming(
    req: &AskAiRequest,
    tx: &async_channel::Sender<AskAiStreamEvent>,
    cancel: &AtomicBool,
) -> AskAiResponse {
    let started = Instant::now();
    let target = resolve_chat_target(&req.ask_config, &req.fim_config);
    let badge = target.badge_label();
    let known_rel_paths =
        known_rel_paths_for_request(&req.workspace_root, req.index.as_ref(), &req.contexts);

    let (raw_content, trace_steps, tool_citations, is_error) = match target {
        ChatBackendTarget::LocalExplain => stream_local_react_explore(req, tx, cancel),
        ChatBackendTarget::OpenAiCompatible {
            provider_label,
            endpoint,
            model,
            api_key,
        } => {
            let msgs = build_ask_ai_messages(req);
            match stream_openai_react_loop(
                req,
                &endpoint,
                &model,
                api_key.as_deref(),
                &msgs,
                tx,
                cancel,
            ) {
                Ok(res) => res,
                Err(err) => (
                    format!(
                        "**{provider_label} request failed**: {err}\n\nCheck your API key or endpoint in **Settings (⌘,)**, or switch provider to **Local AST** for offline ReAct repository exploration."
                    ),
                    Vec::new(),
                    Vec::new(),
                    true,
                ),
            }
        }
        ChatBackendTarget::Ollama {
            endpoint,
            model,
            api_key,
        } => stream_ollama_with_optional_explore(
            req,
            &endpoint,
            &model,
            api_key.as_deref(),
            tx,
            cancel,
        ),
    };

    let content = if is_error {
        raw_content
    } else {
        linkify_code_references(&raw_content, &req.workspace_root, &known_rel_paths)
    };
    let mut citations = extract_code_references(&content, &req.workspace_root, &known_rel_paths, 10);
    for c in tool_citations {
        if !citations
            .iter()
            .any(|e| e.rel_path == c.rel_path && e.start_line == c.start_line)
        {
            citations.push(c);
            if citations.len() >= 12 {
                break;
            }
        }
    }

    let latency_ms = started.elapsed().as_millis() as u64;
    let _ = tx.try_send(AskAiStreamEvent::Finished {
        provider_label: badge.clone(),
        latency_ms,
        is_error,
    });
    AskAiResponse {
        content,
        trace_steps,
        citations,
        provider_label: badge,
        latency_ms,
        is_error,
    }
}

fn build_streaming_http_agent() -> Agent {
    Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .timeout_global(Some(Duration::from_secs(120)))
        .http_status_as_error(false)
        .build()
        .into()
}

#[derive(Default)]
struct PendingToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}

#[allow(clippy::too_many_arguments)]
fn stream_openai_react_loop(
    req: &AskAiRequest,
    endpoint: &str,
    model: &str,
    api_key: Option<&str>,
    initial_messages: &[(String, String)],
    tx: &async_channel::Sender<AskAiStreamEvent>,
    cancel: &AtomicBool,
) -> Result<(String, Vec<AgentTraceStep>, Vec<CodeLinkTarget>, bool)> {
    let agent = build_streaming_http_agent();
    let max_tokens = req.ask_config.max_tokens.max(256);
    let temperature = req.ask_config.temperature;
    let mut use_tools = req.explore_repo && req.index.is_some();
    let tools_json = repo_tool_definitions_json();
    let mut messages: Vec<Value> = initial_messages
        .iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect();

    let mut next_step_id = 1u64;
    let mut all_steps: Vec<AgentTraceStep> = Vec::new();
    let mut all_citations: Vec<CodeLinkTarget> = Vec::new();
    let mut final_content = String::new();

    for turn in 0..6 {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let mut payload = json!({
            "model": model,
            "messages": messages,
            "max_tokens": max_tokens,
            "temperature": temperature,
            "stream": true
        });
        if use_tools {
            payload
                .as_object_mut()
                .unwrap()
                .insert("tools".to_string(), tools_json.clone());
        }

        let mut http_req = agent.post(endpoint);
        if let Some(key) = api_key {
            if !key.trim().is_empty() {
                http_req = http_req.header("Authorization", &format!("Bearer {}", key.trim()));
            }
        }

        let resp = http_req
            .send_json(&payload)
            .context("Chat completions HTTP request failed")?;
        let status = resp.status().as_u16();
        let mime = resp.body().mime_type().unwrap_or("").to_ascii_lowercase();

        if status >= 400 {
            let err_body = resp.into_body().read_to_string().unwrap_or_default();
            if use_tools && turn == 0 && status == 400 && err_body.to_lowercase().contains("tool") {
                use_tools = false;
                continue;
            }
            if let Ok(parsed) = serde_json::from_str::<Value>(&err_body) {
                if let Some(msg) = parsed.pointer("/error/message").and_then(|v| v.as_str()) {
                    return Err(anyhow!("{msg}"));
                }
            }
            return Err(anyhow!("HTTP {status}: {}", err_body.trim()));
        }

        let mut turn_content = String::new();
        let mut turn_tool_calls: BTreeMap<usize, PendingToolCallAcc> = BTreeMap::new();
        let mut thought_step_id: Option<u64> = None;

        if mime.contains("json") && !mime.contains("event-stream") {
            let body: Value = resp
                .into_body()
                .read_json()
                .context("Failed to parse chat completions JSON response")?;
            if let Some(err_msg) = body.pointer("/error/message").and_then(|v| v.as_str()) {
                return Err(anyhow!("{err_msg}"));
            }
            if let Some(reasoning) = body
                .pointer("/choices/0/message/reasoning_content")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let sid = alloc_step_id(&mut next_step_id);
                let step = AgentTraceStep {
                    id: sid,
                    kind: AgentStepKind::Thought,
                    tool_name: None,
                    title: format!("Thought (step {})", turn + 1),
                    detail: reasoning.to_string(),
                    citations: Vec::new(),
                    status: AgentStepStatus::Done,
                };
                all_steps.push(step.clone());
                let _ = tx.try_send(AskAiStreamEvent::StepAdded(step));
            }
            if let Some(tcs) = body
                .pointer("/choices/0/message/tool_calls")
                .and_then(|v| v.as_array())
            {
                for (ix, tc) in tcs.iter().enumerate() {
                    turn_tool_calls.insert(
                        ix,
                        PendingToolCallAcc {
                            id: tc
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            name: tc
                                .pointer("/function/name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            arguments: tc
                                .pointer("/function/arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}")
                                .to_string(),
                        },
                    );
                }
            }
            if let Some(txt) = body
                .pointer("/choices/0/message/content")
                .and_then(|v| v.as_str())
            {
                turn_content.push_str(txt);
                for chunk in chunk_text_for_streaming(txt) {
                    let _ = tx.try_send(AskAiStreamEvent::ContentDelta(chunk));
                }
            }
        } else {
            let mut reader = BufReader::new(resp.into_body().into_reader());
            let mut line = String::new();
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                line.clear();
                let n = reader
                    .read_line(&mut line)
                    .context("Failed reading chat SSE stream")?;
                if n == 0 {
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with(':') {
                    continue;
                }
                let data_str = trimmed
                    .strip_prefix("data:")
                    .map(str::trim)
                    .unwrap_or(trimmed);
                if data_str == "[DONE]" {
                    break;
                }
                let Ok(val) = serde_json::from_str::<Value>(data_str) else {
                    continue;
                };
                if let Some(err_msg) = val.pointer("/error/message").and_then(|v| v.as_str()) {
                    return Err(anyhow!("{err_msg}"));
                }
                let Some(choice) = val.pointer("/choices/0") else {
                    continue;
                };
                let delta = choice.get("delta").unwrap_or(&Value::Null);

                if let Some(rc) = delta
                    .get("reasoning_content")
                    .or_else(|| delta.get("reasoning"))
                    .and_then(|v| v.as_str())
                {
                    if !rc.is_empty() {
                        if let Some(sid) = thought_step_id {
                            if let Some(s) = all_steps.iter_mut().find(|s| s.id == sid) {
                                s.detail.push_str(rc);
                            }
                            let _ = tx.try_send(AskAiStreamEvent::StepUpdated {
                                step_id: sid,
                                detail_append: Some(rc.to_string()),
                                replace_detail: None,
                                status: None,
                                citations: Vec::new(),
                            });
                        } else {
                            let sid = alloc_step_id(&mut next_step_id);
                            thought_step_id = Some(sid);
                            let step = AgentTraceStep {
                                id: sid,
                                kind: AgentStepKind::Thought,
                                tool_name: None,
                                title: format!("Thought (step {})", turn + 1),
                                detail: rc.to_string(),
                                citations: Vec::new(),
                                status: AgentStepStatus::Running,
                            };
                            all_steps.push(step.clone());
                            let _ = tx.try_send(AskAiStreamEvent::StepAdded(step));
                        }
                    }
                }

                if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tcs {
                        let ix = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        let entry = turn_tool_calls.entry(ix).or_default();
                        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                            if !id.is_empty() {
                                entry.id = id.to_string();
                            }
                        }
                        if let Some(name) = tc.pointer("/function/name").and_then(|v| v.as_str()) {
                            entry.name.push_str(name);
                        }
                        if let Some(args) =
                            tc.pointer("/function/arguments").and_then(|v| v.as_str())
                        {
                            entry.arguments.push_str(args);
                        }
                    }
                }

                if let Some(piece) = delta.get("content").and_then(|v| v.as_str()) {
                    if !piece.is_empty() {
                        if let Some(sid) = thought_step_id.take() {
                            if let Some(s) = all_steps.iter_mut().find(|s| s.id == sid) {
                                s.status = AgentStepStatus::Done;
                            }
                            let _ = tx.try_send(AskAiStreamEvent::StepUpdated {
                                step_id: sid,
                                detail_append: None,
                                replace_detail: None,
                                status: Some(AgentStepStatus::Done),
                                citations: Vec::new(),
                            });
                        }
                        turn_content.push_str(piece);
                        let _ = tx.try_send(AskAiStreamEvent::ContentDelta(piece.to_string()));
                    }
                }
            }
        }

        if let Some(sid) = thought_step_id.take() {
            if let Some(s) = all_steps.iter_mut().find(|s| s.id == sid) {
                s.status = AgentStepStatus::Done;
            }
            let _ = tx.try_send(AskAiStreamEvent::StepUpdated {
                step_id: sid,
                detail_append: None,
                replace_detail: None,
                status: Some(AgentStepStatus::Done),
                citations: Vec::new(),
            });
        }

        if turn_tool_calls.is_empty() || cancel.load(Ordering::Relaxed) {
            final_content.push_str(&turn_content);
            break;
        }

        if !turn_content.trim().is_empty() && !turn_tool_calls.is_empty() {
            // ReAct intermediate narration before final answer stays in final_content only if separated cleanly.
            final_content.push_str(&turn_content);
            if !final_content.ends_with('\n') {
                final_content.push_str("\n\n");
                let _ = tx.try_send(AskAiStreamEvent::ContentDelta("\n\n".to_string()));
            }
        }

        let assistant_tool_calls_json: Vec<Value> = turn_tool_calls
            .values()
            .enumerate()
            .map(|(i, tc)| {
                let id = if tc.id.is_empty() {
                    format!("call_{turn}_{i}")
                } else {
                    tc.id.clone()
                };
                json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": if tc.arguments.trim().is_empty() { "{}" } else { tc.arguments.as_str() }
                    }
                })
            })
            .collect();
        messages.push(json!({
            "role": "assistant",
            "content": if turn_content.is_empty() { Value::Null } else { json!(turn_content) },
            "tool_calls": assistant_tool_calls_json
        }));

        for (i, tc) in turn_tool_calls.into_values().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let call_id = if tc.id.is_empty() {
                format!("call_{turn}_{i}")
            } else {
                tc.id.clone()
            };
            let call_sid = alloc_step_id(&mut next_step_id);
            let step = AgentTraceStep {
                id: call_sid,
                kind: AgentStepKind::ToolCall,
                tool_name: Some(tc.name.clone()),
                title: format!("Action: `{}`", tc.name),
                detail: tc.arguments.trim().to_string(),
                citations: Vec::new(),
                status: AgentStepStatus::Running,
            };
            all_steps.push(step.clone());
            let _ = tx.try_send(AskAiStreamEvent::StepAdded(step));

            let exec = run_repo_tool(
                &tc.name,
                &tc.arguments,
                &req.workspace_root,
                req.index.as_ref(),
            );
            let done_status = if exec.is_error {
                AgentStepStatus::Error
            } else {
                AgentStepStatus::Done
            };
            all_citations.extend(exec.citations.clone());

            if let Some(s) = all_steps.iter_mut().find(|s| s.id == call_sid) {
                s.status = done_status;
                s.title = format!("Action `{}` · {}", tc.name, exec.trace_summary);
                s.detail = format!("Args: {}\n\n{}", tc.arguments.trim(), exec.tool_result_text);
                s.citations = exec.citations.clone();
            }
            let _ = tx.try_send(AskAiStreamEvent::StepUpdated {
                step_id: call_sid,
                detail_append: None,
                replace_detail: Some(format!(
                    "Args: {}\n\n{}",
                    tc.arguments.trim(),
                    exec.tool_result_text
                )),
                status: Some(done_status),
                citations: exec.citations,
            });

            messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": exec.tool_result_text
            }));
        }
    }

    Ok((final_content.trim().to_string(), all_steps, all_citations, false))
}

fn stream_ollama_with_optional_explore(
    req: &AskAiRequest,
    endpoint: &str,
    model: &str,
    api_key: Option<&str>,
    tx: &async_channel::Sender<AskAiStreamEvent>,
    cancel: &AtomicBool,
) -> (String, Vec<AgentTraceStep>, Vec<CodeLinkTarget>, bool) {
    let mut step_id = 1u64;
    let mut steps = Vec::new();
    let mut citations = Vec::new();
    let mut extra_repo_context = String::new();

    if req.explore_repo {
        if let Some(idx) = &req.index {
            let sid = alloc_step_id(&mut step_id);
            let terms = extract_exploration_terms(req);
            let mut found_notes = Vec::new();
            for term in terms.into_iter().take(2) {
                let exec = run_repo_tool(
                    "search_symbols",
                    &json!({ "query": term }).to_string(),
                    &req.workspace_root,
                    Some(idx),
                );
                if !exec.citations.is_empty() {
                    citations.extend(exec.citations.clone());
                    found_notes.push(exec.tool_result_text);
                }
            }
            if !found_notes.is_empty() {
                let detail = found_notes.join("\n");
                let step = AgentTraceStep {
                    id: sid,
                    kind: AgentStepKind::ToolResult,
                    tool_name: Some("search_symbols".to_string()),
                    title: format!("Action `search_symbols` · {} link(s)", citations.len()),
                    detail: detail.clone(),
                    citations: citations.clone(),
                    status: AgentStepStatus::Done,
                };
                steps.push(step.clone());
                let _ = tx.try_send(AskAiStreamEvent::StepAdded(step));
                extra_repo_context = format!("\n\n### Repository Search Observations\n{detail}");
            }
        }
    }

    let mut msgs = build_ask_ai_messages(req);
    if !extra_repo_context.is_empty() {
        if let Some((_, last_user)) = msgs.last_mut() {
            last_user.push_str(&extra_repo_context);
        }
    }

    let agent = build_streaming_http_agent();
    let json_msgs: Vec<Value> = msgs
        .into_iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect();
    let payload = json!({
        "model": model,
        "messages": json_msgs,
        "stream": true,
        "options": {
            "num_predict": req.ask_config.max_tokens.max(256),
            "temperature": req.ask_config.temperature
        }
    });
    let mut http_req = agent.post(endpoint);
    if let Some(key) = api_key {
        if !key.trim().is_empty() {
            http_req = http_req.header("Authorization", &format!("Bearer {}", key.trim()));
        }
    }

    let resp = match http_req.send_json(&payload) {
        Ok(r) => r,
        Err(err) => {
            return (
                format!(
                    "**Ollama request failed**: {err}\n\nVerify your Ollama server is running at `{endpoint}`."
                ),
                steps,
                citations,
                true,
            );
        }
    };
    if resp.status().as_u16() >= 400 {
        let body = resp.into_body().read_to_string().unwrap_or_default();
        return (
            format!("**Ollama request failed**: {}", body.trim()),
            steps,
            citations,
            true,
        );
    }

    let mut reader = BufReader::new(resp.into_body().into_reader());
    let mut line = String::new();
    let mut content = String::new();
    let mut thought_id: Option<u64> = None;

    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        line.clear();
        let Ok(n) = reader.read_line(&mut line) else {
            break;
        };
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(t) = val
            .pointer("/message/thinking")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            if let Some(tid) = thought_id {
                if let Some(s) = steps.iter_mut().find(|s| s.id == tid) {
                    s.detail.push_str(t);
                }
                let _ = tx.try_send(AskAiStreamEvent::StepUpdated {
                    step_id: tid,
                    detail_append: Some(t.to_string()),
                    replace_detail: None,
                    status: None,
                    citations: Vec::new(),
                });
            } else {
                let tid = alloc_step_id(&mut step_id);
                thought_id = Some(tid);
                let step = AgentTraceStep {
                    id: tid,
                    kind: AgentStepKind::Thought,
                    tool_name: None,
                    title: "Thought".to_string(),
                    detail: t.to_string(),
                    citations: Vec::new(),
                    status: AgentStepStatus::Running,
                };
                steps.push(step.clone());
                let _ = tx.try_send(AskAiStreamEvent::StepAdded(step));
            }
        }
        let piece = val
            .pointer("/message/content")
            .or_else(|| val.get("response"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !piece.is_empty() {
            if let Some(tid) = thought_id.take() {
                if let Some(s) = steps.iter_mut().find(|s| s.id == tid) {
                    s.status = AgentStepStatus::Done;
                }
                let _ = tx.try_send(AskAiStreamEvent::StepUpdated {
                    step_id: tid,
                    detail_append: None,
                    replace_detail: None,
                    status: Some(AgentStepStatus::Done),
                    citations: Vec::new(),
                });
            }
            content.push_str(piece);
            let _ = tx.try_send(AskAiStreamEvent::ContentDelta(piece.to_string()));
        }
        if val.get("done").and_then(|v| v.as_bool()) == Some(true) {
            break;
        }
    }

    if let Some(tid) = thought_id.take() {
        if let Some(s) = steps.iter_mut().find(|s| s.id == tid) {
            s.status = AgentStepStatus::Done;
        }
        let _ = tx.try_send(AskAiStreamEvent::StepUpdated {
            step_id: tid,
            detail_append: None,
            replace_detail: None,
            status: Some(AgentStepStatus::Done),
            citations: Vec::new(),
        });
    }

    (content.trim().to_string(), steps, citations, false)
}

fn alloc_step_id(counter: &mut u64) -> u64 {
    let id = *counter;
    *counter = counter.wrapping_add(1);
    id
}

fn chunk_text_for_streaming(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        cur.push_str(line);
        if cur.len() >= 96 {
            chunks.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

fn extract_exploration_terms(req: &AskAiRequest) -> Vec<String> {
    let mut terms = Vec::new();
    let mut seen = HashSet::new();
    let stop: HashSet<&str> = [
        "what", "where", "when", "which", "this", "that", "with", "from", "into", "about",
        "code", "line", "file", "repo", "explain", "review", "test", "tests", "fix", "how",
        "does", "connect", "find", "show", "attached", "selected", "context", "please",
    ]
    .into_iter()
    .collect();

    for ctx in &req.contexts {
        if let Some(sym) = &ctx.enclosing_symbol {
            let clean = sym.trim();
            if !clean.is_empty() && seen.insert(clean.to_string()) {
                terms.push(clean.to_string());
            }
        }
    }

    let mut tok = String::new();
    for ch in req.prompt.chars().chain(std::iter::once(' ')) {
        if is_ident_char(ch) {
            tok.push(ch);
        } else if !tok.is_empty() {
            let lower = tok.to_ascii_lowercase();
            if tok.len() >= 3 && !stop.contains(lower.as_str()) && seen.insert(tok.clone()) {
                terms.push(tok.clone());
            }
            tok.clear();
        }
    }

    if terms.len() < 4 {
        for ctx in &req.contexts {
            let mut t = String::new();
            for ch in ctx.code.chars().chain(std::iter::once(' ')) {
                if is_ident_char(ch) {
                    t.push(ch);
                } else if !t.is_empty() {
                    let starts_upper = t.chars().next().is_some_and(char::is_uppercase);
                    if (starts_upper || t.contains('_')) && t.len() >= 3 && seen.insert(t.clone())
                    {
                        terms.push(t.clone());
                        if terms.len() >= 4 {
                            return terms;
                        }
                    }
                    t.clear();
                }
            }
        }
    }
    terms
}

fn stream_local_react_explore(
    req: &AskAiRequest,
    tx: &async_channel::Sender<AskAiStreamEvent>,
    cancel: &AtomicBool,
) -> (String, Vec<AgentTraceStep>, Vec<CodeLinkTarget>, bool) {
    let mut step_id = 1u64;
    let mut steps = Vec::new();
    let mut citations = Vec::new();
    let mut findings: Vec<String> = Vec::new();

    let emit_thought = |id: &mut u64,
                        title: &str,
                        detail: &str,
                        steps: &mut Vec<AgentTraceStep>,
                        tx: &async_channel::Sender<AskAiStreamEvent>| {
        let sid = alloc_step_id(id);
        let step = AgentTraceStep {
            id: sid,
            kind: AgentStepKind::Thought,
            tool_name: None,
            title: title.to_string(),
            detail: detail.to_string(),
            citations: Vec::new(),
            status: AgentStepStatus::Done,
        };
        steps.push(step.clone());
        let _ = tx.try_send(AskAiStreamEvent::StepAdded(step));
    };

    if req.explore_repo && !cancel.load(Ordering::Relaxed) {
        if let Some(idx) = &req.index {
            let terms = extract_exploration_terms(req);
            emit_thought(
                &mut step_id,
                "Thought 1 · Plan repo exploration",
                &if terms.is_empty() {
                    "Listing repository entry files to map the codebase structure.".to_string()
                } else {
                    format!(
                        "Tracing symbols, definitions, and callers for: {}",
                        terms.join(", ")
                    )
                },
                &mut steps,
                tx,
            );

            // Step A: list_files or search_symbols
            if !cancel.load(Ordering::Relaxed) {
                let query = terms.first().cloned().unwrap_or_default();
                let (tool_name, args_json) = if query.is_empty() {
                    ("list_files", json!({ "limit": 12 }).to_string())
                } else {
                    ("search_symbols", json!({ "query": query }).to_string())
                };
                let sid = alloc_step_id(&mut step_id);
                let running = AgentTraceStep {
                    id: sid,
                    kind: AgentStepKind::ToolCall,
                    tool_name: Some(tool_name.to_string()),
                    title: format!("Action: `{tool_name}`"),
                    detail: args_json.clone(),
                    citations: Vec::new(),
                    status: AgentStepStatus::Running,
                };
                steps.push(running.clone());
                let _ = tx.try_send(AskAiStreamEvent::StepAdded(running));

                let exec = run_repo_tool(tool_name, &args_json, &req.workspace_root, Some(idx));
                let status = if exec.is_error {
                    AgentStepStatus::Error
                } else {
                    AgentStepStatus::Done
                };
                let full_detail = format!("Args: {args_json}\n\n{}", exec.tool_result_text);
                if let Some(s) = steps.iter_mut().find(|s| s.id == sid) {
                    s.title = format!("Action `{tool_name}` · {}", exec.trace_summary);
                    s.detail = full_detail.clone();
                    s.citations = exec.citations.clone();
                    s.status = status;
                }
                let _ = tx.try_send(AskAiStreamEvent::StepUpdated {
                    step_id: sid,
                    detail_append: None,
                    replace_detail: Some(full_detail),
                    status: Some(status),
                    citations: exec.citations.clone(),
                });
                if !exec.citations.is_empty() {
                    findings.push(exec.tool_result_text);
                    citations.extend(exec.citations);
                }
            }

            // Step B: find_references or grep_text for the primary symbol / term
            if !cancel.load(Ordering::Relaxed) {
                if let Some(primary) = terms.first() {
                    let args_json = json!({ "symbol": primary }).to_string();
                    let sid = alloc_step_id(&mut step_id);
                    let running = AgentTraceStep {
                        id: sid,
                        kind: AgentStepKind::ToolCall,
                        tool_name: Some("find_references".to_string()),
                        title: format!("Action: `find_references` ({primary})"),
                        detail: args_json.clone(),
                        citations: Vec::new(),
                        status: AgentStepStatus::Running,
                    };
                    steps.push(running.clone());
                    let _ = tx.try_send(AskAiStreamEvent::StepAdded(running));

                    let exec =
                        run_repo_tool("find_references", &args_json, &req.workspace_root, Some(idx));
                    let exec = if exec.citations.is_empty() {
                        run_repo_tool(
                            "grep_text",
                            &json!({ "pattern": primary }).to_string(),
                            &req.workspace_root,
                            Some(idx),
                        )
                    } else {
                        exec
                    };
                    let status = if exec.is_error {
                        AgentStepStatus::Error
                    } else {
                        AgentStepStatus::Done
                    };
                    let full_detail = format!("Args: {args_json}\n\n{}", exec.tool_result_text);
                    if let Some(s) = steps.iter_mut().find(|s| s.id == sid) {
                        s.title = format!("Action `find_references` · {}", exec.trace_summary);
                        s.detail = full_detail.clone();
                        s.citations = exec.citations.clone();
                        s.status = status;
                    }
                    let _ = tx.try_send(AskAiStreamEvent::StepUpdated {
                        step_id: sid,
                        detail_append: None,
                        replace_detail: Some(full_detail),
                        status: Some(status),
                        citations: exec.citations.clone(),
                    });
                    if !exec.citations.is_empty() {
                        findings.push(exec.tool_result_text);
                        citations.extend(exec.citations);
                    }
                }
            }

            // Step C: read_lines on top discovered non-context location
            if !cancel.load(Ordering::Relaxed) {
                if let Some(top_cite) = citations.iter().find(|c| {
                    !req.contexts
                        .iter()
                        .any(|att| att.rel_path == c.rel_path && att.start_line == c.start_line)
                }) {
                    let read_args = json!({
                        "path": top_cite.rel_path,
                        "start_line": top_cite.start_line,
                        "end_line": top_cite.start_line + 14
                    })
                    .to_string();
                    let sid = alloc_step_id(&mut step_id);
                    let running = AgentTraceStep {
                        id: sid,
                        kind: AgentStepKind::ToolCall,
                        tool_name: Some("read_lines".to_string()),
                        title: format!("Action: `read_lines` ({})", top_cite.display_label()),
                        detail: read_args.clone(),
                        citations: Vec::new(),
                        status: AgentStepStatus::Running,
                    };
                    steps.push(running.clone());
                    let _ = tx.try_send(AskAiStreamEvent::StepAdded(running));

                    let exec =
                        run_repo_tool("read_lines", &read_args, &req.workspace_root, Some(idx));
                    let status = if exec.is_error {
                        AgentStepStatus::Error
                    } else {
                        AgentStepStatus::Done
                    };
                    let full_detail = format!("Args: {read_args}\n\n{}", exec.tool_result_text);
                    if let Some(s) = steps.iter_mut().find(|s| s.id == sid) {
                        s.title = format!("Action `read_lines` · {}", exec.trace_summary);
                        s.detail = full_detail.clone();
                        s.citations = exec.citations.clone();
                        s.status = status;
                    }
                    let _ = tx.try_send(AskAiStreamEvent::StepUpdated {
                        step_id: sid,
                        detail_append: None,
                        replace_detail: Some(full_detail),
                        status: Some(status),
                        citations: exec.citations.clone(),
                    });
                }
            }

            emit_thought(
                &mut step_id,
                "Thought 2 · Synthesize findings",
                &format!(
                    "Collected {} clickable location citation(s); streaming final Markdown answer.",
                    citations.len()
                ),
                &mut steps,
                tx,
            );
        }
    }

    let md = generate_local_explanation_with_findings(req, &findings);
    let known =
        known_rel_paths_for_request(&req.workspace_root, req.index.as_ref(), &req.contexts);
    let linked = linkify_code_references(&md, &req.workspace_root, &known);
    for chunk in chunk_text_for_streaming(&linked) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let _ = tx.try_send(AskAiStreamEvent::ContentDelta(chunk));
    }
    (linked, steps, citations, false)
}

pub fn generate_local_explanation(req: &AskAiRequest) -> String {
    generate_local_explanation_with_findings(req, &[])
}

fn generate_local_explanation_with_findings(
    req: &AskAiRequest,
    explored_findings: &[String],
) -> String {
    let mut md = String::new();
    let title = match req.preset {
        AskAiPreset::Explain => "### Code Explanation (Local AST Analysis)",
        AskAiPreset::Review => "### Code Review Notes (Local AST Analysis)",
        AskAiPreset::Tests => "### Suggested Test Outline (Local AST Analysis)",
        AskAiPreset::Fix => "### Diagnostics & Fix Hints (Local AST Analysis)",
        AskAiPreset::Custom => "### Context & Repo Exploration (Local ReAct AST)",
    };
    md.push_str(title);
    md.push_str("\n\n");

    if !req.prompt.trim().is_empty() && req.prompt.trim() != req.preset.default_user_label() {
        md.push_str(&format!("**Prompt:** {}\n\n", req.prompt.trim()));
    }

    if req.contexts.is_empty() && explored_findings.is_empty() {
        md.push_str(&format!(
            "No code snippet is currently attached and no symbol query matched.\n\nSelect code in the editor and press **⌘L** (attach) or **⌘⇧E** (explain), or ask about any workspace symbol.\n\n- Question: `{}`\n- Tip: Add an API key in **Settings (⌘,)** (DeepSeek / OpenAI / Ollama) for full LLM answers.",
            if req.prompt.trim().is_empty() {
                req.preset.default_user_label()
            } else {
                req.prompt.trim()
            }
        ));
        return md;
    }

    for ctx in &req.contexts {
        let source_kind = if ctx.from_selection {
            "Selected code"
        } else {
            "Active scope"
        };
        let scope = ctx
            .enclosing_symbol
            .as_deref()
            .map(|s| format!(" inside `{s}`"))
            .unwrap_or_default();
        let target = CodeLinkTarget {
            path: ctx.path.clone(),
            rel_path: ctx.rel_path.clone(),
            start_line: ctx.start_line,
            end_line: Some(ctx.end_line),
            col: None,
        };
        md.push_str(&format!(
            "#### {} — {} ({} lines{}{})\n",
            target.markdown_link(),
            ctx.language,
            ctx.line_count(),
            if ctx.language.is_empty() { "" } else { ", " },
            format!("{source_kind}{scope}")
        ));

        let trimmed_lines: Vec<&str> = ctx
            .code
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let decls: Vec<&str> = trimmed_lines
            .iter()
            .copied()
            .filter(|l| {
                l.starts_with("pub fn ")
                    || l.starts_with("fn ")
                    || l.starts_with("async fn ")
                    || l.starts_with("pub struct ")
                    || l.starts_with("struct ")
                    || l.starts_with("pub enum ")
                    || l.starts_with("enum ")
                    || l.starts_with("impl ")
                    || l.starts_with("def ")
                    || l.starts_with("class ")
                    || l.starts_with("function ")
                    || l.starts_with("export ")
                    || l.starts_with("func ")
            })
            .take(5)
            .collect();

        if !decls.is_empty() {
            md.push_str("\n**Key declarations:**\n");
            for d in decls {
                md.push_str(&format!("- `{}`\n", d.trim_end_matches('{').trim()));
            }
        }

        let mut traits = Vec::new();
        let code = &ctx.code;
        if code.contains("match ") || code.contains("switch ") {
            traits.push("branches via pattern/switch dispatch");
        }
        if code.contains("for ") || code.contains("while ") || code.contains(".iter(") {
            traits.push("iterates or loops over elements");
        }
        if code.contains("async ") || code.contains(".await") {
            traits.push("contains asynchronous execution (`async`/`.await`)");
        }
        if code.contains('?') || code.contains("Result<") || code.contains("try ") {
            traits.push("propagates or handles fallible errors");
        }
        if !traits.is_empty() {
            md.push_str("\n**Control flow observed:**\n");
            for t in traits {
                md.push_str(&format!("- {t}\n"));
            }
        }

        let preview: String = ctx.code.lines().take(8).collect::<Vec<_>>().join("\n");
        md.push_str(&format!("\n```{}\n{}\n```\n\n", ctx.language, preview));
    }

    if !explored_findings.is_empty() {
        md.push_str("**Explored Repository Locations:**\n");
        for block in explored_findings {
            md.push_str(block);
            md.push('\n');
        }
        md.push('\n');
    }

    if !req.related_definitions.is_empty() {
        md.push_str("**Referenced workspace symbols:**\n");
        for def in &req.related_definitions {
            md.push_str(&format!("- {def}\n"));
        }
        md.push('\n');
    }

    if !req.diagnostics_in_scope.is_empty() {
        md.push_str("**Diagnostics in range:**\n");
        for d in &req.diagnostics_in_scope {
            md.push_str(&format!("- {d}\n"));
        }
        md.push('\n');
    }

    md.push_str("_Running in offline Local AST ReAct mode. Configure DeepSeek/OpenAI/Ollama in **Settings (⌘,)** for full streaming LLM repository exploration._");
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use tempfile::TempDir;

    fn read_http_request(stream: &mut TcpStream) -> String {
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut expected_total: Option<usize> = None;
        loop {
            let n = stream.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..n]);
            if expected_total.is_none() {
                if let Some(hdr_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let hdr_str = String::from_utf8_lossy(&raw[..hdr_end]);
                    let content_len = hdr_str
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    expected_total = Some(hdr_end + 4 + content_len);
                }
            }
            if let Some(total) = expected_total {
                if raw.len() >= total {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&raw).to_string()
    }

    #[test]
    fn resolves_deepseek_fim_config_to_chat_completions_endpoint_and_model() {
        let fim = FimConfig {
            provider: FimProviderKind::DeepSeek,
            endpoint: "https://api.deepseek.com/beta".to_string(),
            model: "deepseek-flash".to_string(),
            api_key: "ds-test-key".to_string(),
            ..Default::default()
        };
        let ask = AskAiConfig::default();
        assert_eq!(
            resolve_chat_target(&ask, &fim),
            ChatBackendTarget::OpenAiCompatible {
                provider_label: "DeepSeek".to_string(),
                endpoint: "https://api.deepseek.com/chat/completions".to_string(),
                model: "deepseek-chat".to_string(),
                api_key: Some("ds-test-key".to_string()),
            }
        );

        let override_ask = AskAiConfig {
            model: "deepseek-reasoner".to_string(),
            ..Default::default()
        };
        assert_eq!(
            resolve_chat_target(&override_ask, &fim),
            ChatBackendTarget::OpenAiCompatible {
                provider_label: "DeepSeek".to_string(),
                endpoint: "https://api.deepseek.com/chat/completions".to_string(),
                model: "deepseek-reasoner".to_string(),
                api_key: Some("ds-test-key".to_string()),
            }
        );
    }

    #[test]
    fn builds_contextual_prompt_with_selected_code_and_related_symbols() {
        let req = AskAiRequest {
            prompt: "Why does credit return &mut Account?".to_string(),
            preset: AskAiPreset::Explain,
            contexts: vec![CodeContextAttachment {
                id: 1,
                path: PathBuf::from("src/lib.rs"),
                rel_path: "src/lib.rs".to_string(),
                language: "rust".to_string(),
                start_line: 10,
                end_line: 14,
                code: "pub fn credit(acc: &mut Account, amount: i64) {\n    acc.balance += amount;\n}"
                    .to_string(),
                enclosing_symbol: Some("credit".to_string()),
                from_selection: true,
            }],
            related_definitions: vec![
                "`pub struct Account` (`Struct` at src/lib.rs:L2)".to_string(),
            ],
            diagnostics_in_scope: vec![],
            history: vec![],
            workspace_root: PathBuf::new(),
            index: None,
            explore_repo: false,
            fim_config: FimConfig::default(),
            ask_config: AskAiConfig::default(),
        };

        let msgs = build_ask_ai_messages(&req);
        assert_eq!(msgs[0].0, "system");
        assert_eq!(msgs[1].0, "user");
        assert!(msgs[1].1.contains("Why does credit return &mut Account?"));
        assert!(msgs[1].1.contains("src/lib.rs:L10-14"));
        assert!(msgs[1].1.contains("enclosing `credit`"));
        assert!(msgs[1].1.contains("```rust"));
        assert!(msgs[1].1.contains("acc.balance += amount;"));
        assert!(msgs[1].1.contains("pub struct Account"));
    }

    #[test]
    fn offline_local_explain_formats_markdown_explanation() {
        let req = AskAiRequest {
            prompt: "".to_string(),
            preset: AskAiPreset::Explain,
            contexts: vec![CodeContextAttachment {
                id: 1,
                path: PathBuf::from("src/main.rs"),
                rel_path: "src/main.rs".to_string(),
                language: "rust".to_string(),
                start_line: 5,
                end_line: 9,
                code: "fn process(x: Option<i32>) -> i32 {\n    match x {\n        Some(v) => v,\n        None => 0,\n    }\n}"
                    .to_string(),
                enclosing_symbol: Some("process".to_string()),
                from_selection: true,
            }],
            related_definitions: vec![],
            diagnostics_in_scope: vec![],
            history: vec![],
            workspace_root: PathBuf::new(),
            index: None,
            explore_repo: false,
            fim_config: FimConfig {
                provider: FimProviderKind::LocalAst,
                ..Default::default()
            },
            ask_config: AskAiConfig::default(),
        };
        let out = execute_ask_ai(&req);
        assert!(!out.is_error);
        assert_eq!(out.provider_label, "Local AST Explain");
        assert!(out.content.contains("src/main.rs:L5-9"));
        assert!(out.content.contains("fn process"));
        assert!(out.content.contains("branches via pattern/switch dispatch"));
    }

    #[test]
    fn ask_ai_queries_mock_openai_compatible_http_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server_thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let req_str = read_http_request(&mut stream);
            assert!(req_str.contains("POST /v1/chat/completions"));
            assert!(req_str.contains("selected snippet line"));

            let body = r####"{"choices":[{"message":{"content":"### Explanation\nSee src/math.rs:L2 where double runs cleanly."}}]}"####;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.flush().unwrap();
        });

        let req = AskAiRequest {
            prompt: "Explain this helper".to_string(),
            preset: AskAiPreset::Explain,
            contexts: vec![CodeContextAttachment {
                id: 42,
                path: PathBuf::from("src/math.rs"),
                rel_path: "src/math.rs".to_string(),
                language: "rust".to_string(),
                start_line: 1,
                end_line: 3,
                code: "fn double(x: i32) -> i32 { x * 2 } // selected snippet line".to_string(),
                enclosing_symbol: Some("double".to_string()),
                from_selection: true,
            }],
            related_definitions: vec![],
            diagnostics_in_scope: vec![],
            history: vec![],
            workspace_root: PathBuf::new(),
            index: None,
            explore_repo: false,
            fim_config: FimConfig::default(),
            ask_config: AskAiConfig {
                endpoint: format!("http://{addr}/v1/chat/completions"),
                model: "deepseek-chat".to_string(),
                api_key: "test-key".to_string(),
                ..Default::default()
            },
        };

        let out = execute_ask_ai(&req);
        server_thread.join().unwrap();
        assert!(!out.is_error, "unexpected error: {}", out.content);
        assert!(out
            .content
            .contains("[src/math.rs:L2](kestrel://open?path=src/math.rs&line=2)"));
        assert!(!out.citations.is_empty());
    }

    #[test]
    fn linkifies_and_parses_code_references_preserving_fences() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("models.rs"), "pub struct Account {\n    pub id: u64,\n}\n").unwrap();
        let mut known = HashSet::new();
        known.insert("src/models.rs".to_string());

        let md = "Check `src/models.rs:2` and src/models.rs:L1-2.\n```rust\n// ignore inside fence src/models.rs:3\n```\nAlso [already](kestrel://open?path=src/models.rs&line=1).";
        let linked = linkify_code_references(md, dir.path(), &known);
        assert!(linked.contains("[src/models.rs:L2](kestrel://open?path=src/models.rs&line=2)"));
        assert!(linked.contains("[src/models.rs:L1-2](kestrel://open?path=src/models.rs&line=1&end=2)"));
        assert!(linked.contains("// ignore inside fence src/models.rs:3"));
        assert!(linked.contains("[already](kestrel://open?path=src/models.rs&line=1)"));

        let parsed = parse_code_link_url(
            "kestrel://open?path=src%2Fmodels.rs&line=2&col=5",
            dir.path(),
            &known,
        )
        .unwrap();
        assert_eq!(parsed.rel_path, "src/models.rs");
        assert_eq!(parsed.start_line, 2);
        assert_eq!(parsed.col, Some(5));
        assert_eq!(parsed.zero_based_pos(), (1, 4));

        let parsed_flint = parse_code_link_url(
            "flint://open?path=src%2Fmodels.rs&line=3",
            dir.path(),
            &known,
        )
        .unwrap();
        assert_eq!(parsed_flint.rel_path, "src/models.rs");
        assert_eq!(parsed_flint.start_line, 3);
    }

    #[test]
    fn repo_explorer_tools_enforce_isolation_and_return_code_links() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("ledger.rs"),
            "pub fn post_entry(amount: i64) -> i64 {\n    amount * 2\n}\n",
        )
        .unwrap();
        fs::write(
            src.join("main.rs"),
            "mod ledger;\nfn main() {\n    let _v = ledger::post_entry(21);\n}\n",
        )
        .unwrap();
        let idx = WorkspaceIndex::new();
        idx.index_workspace(dir.path());

        let sym = run_repo_tool(
            "search_symbols",
            r#"{"query":"^post_.*","regex":true}"#,
            dir.path(),
            Some(&idx),
        );
        assert!(!sym.is_error);
        assert!(sym.tool_result_text.contains("post_entry"));
        assert!(sym.citations.iter().any(|c| c.rel_path == "src/ledger.rs" && c.start_line == 1));

        let refs = run_repo_tool(
            "find_references",
            r#"{"symbol":"post_entry"}"#,
            dir.path(),
            Some(&idx),
        );
        assert!(!refs.is_error);
        assert!(refs.citations.iter().any(|c| c.rel_path == "src/main.rs" && c.start_line == 3));

        let bad = run_repo_tool(
            "read_lines",
            r#"{"path":"../../etc/passwd","start_line":1,"end_line":5}"#,
            dir.path(),
            Some(&idx),
        );
        assert!(bad.is_error);
    }

    #[test]
    fn ask_ai_streaming_sse_react_tool_calls_and_reasoning() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("calc.rs"),
            "/// Computes tax in cents.\npub fn compute_tax(cents: i64) -> i64 {\n    cents / 10\n}\n",
        )
        .unwrap();
        let idx = WorkspaceIndex::new();
        idx.index_workspace(dir.path());

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server_thread = std::thread::spawn(move || {
            // Turn 1: SSE stream with reasoning_content + tool_call(`read_lines`)
            let (mut s1, _) = listener.accept().unwrap();
            let req1 = read_http_request(&mut s1);
            assert!(req1.contains("read_lines"));
            let sse1 = concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Need to inspect compute_tax.\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_lines\",\"arguments\":\"{\\\"path\\\":\\\"src/calc.rs\\\",\\\"start_line\\\":1,\\\"end_line\\\":4}\"}}]}}]}\n\n",
                "data: [DONE]\n\n"
            );
            let resp1 = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse1.len(),
                sse1
            );
            s1.write_all(resp1.as_bytes()).unwrap();
            s1.flush().unwrap();

            // Turn 2: Receive tool output & stream final answer with code ref
            let (mut s2, _) = listener.accept().unwrap();
            let req2 = read_http_request(&mut s2);
            assert!(req2.contains("cents / 10"));
            let sse2 = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"`compute_tax` divides by 10 at \"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"src/calc.rs:L2.\"}}]}\n\n",
                "data: [DONE]\n\n"
            );
            let resp2 = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse2.len(),
                sse2
            );
            s2.write_all(resp2.as_bytes()).unwrap();
            s2.flush().unwrap();
        });

        let req = AskAiRequest {
            prompt: "How does compute_tax work?".to_string(),
            preset: AskAiPreset::Custom,
            contexts: Vec::new(),
            related_definitions: Vec::new(),
            diagnostics_in_scope: Vec::new(),
            history: Vec::new(),
            workspace_root: dir.path().to_path_buf(),
            index: Some(idx),
            explore_repo: true,
            fim_config: FimConfig::default(),
            ask_config: AskAiConfig {
                endpoint: format!("http://{addr}/v1/chat/completions"),
                model: "deepseek-chat".to_string(),
                api_key: "test-key".to_string(),
                ..Default::default()
            },
        };

        let (tx, rx) = async_channel::unbounded();
        let cancel = AtomicBool::new(false);
        let resp = execute_ask_ai_streaming(&req, &tx, &cancel);
        server_thread.join().unwrap();
        drop(tx);

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        assert!(!resp.is_error);
        assert!(resp
            .content
            .contains("[src/calc.rs:L2](kestrel://open?path=src/calc.rs&line=2)"));
        assert!(resp
            .trace_steps
            .iter()
            .any(|s| s.kind == AgentStepKind::Thought && s.detail.contains("inspect compute_tax")));
        assert!(resp
            .trace_steps
            .iter()
            .any(|s| s.tool_name.as_deref() == Some("read_lines")
                && s.status == AgentStepStatus::Done));
        assert!(events
            .iter()
            .any(|e| matches!(e, AskAiStreamEvent::ContentDelta(_))));
    }

    #[test]
    fn ask_ai_local_react_streams_steps_and_citations() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("billing.rs"),
            "pub struct Invoice { pub total: i64 }\npub fn charge_invoice(inv: &Invoice) -> i64 { inv.total }\n",
        )
        .unwrap();
        let idx = WorkspaceIndex::new();
        idx.index_workspace(dir.path());

        let req = AskAiRequest {
            prompt: "Where is charge_invoice defined?".to_string(),
            preset: AskAiPreset::Custom,
            contexts: Vec::new(),
            related_definitions: Vec::new(),
            diagnostics_in_scope: Vec::new(),
            history: Vec::new(),
            workspace_root: dir.path().to_path_buf(),
            index: Some(idx),
            explore_repo: true,
            fim_config: FimConfig {
                provider: FimProviderKind::LocalAst,
                ..Default::default()
            },
            ask_config: AskAiConfig::default(),
        };

        let (tx, rx) = async_channel::unbounded();
        let cancel = AtomicBool::new(false);
        let resp = execute_ask_ai_streaming(&req, &tx, &cancel);
        drop(tx);

        let mut streamed_deltas = 0usize;
        let mut streamed_steps = 0usize;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AskAiStreamEvent::StepAdded(_) => streamed_steps += 1,
                AskAiStreamEvent::ContentDelta(_) => streamed_deltas += 1,
                _ => {}
            }
        }
        assert!(!resp.is_error);
        assert!(streamed_steps >= 3);
        assert!(streamed_deltas >= 1);
        assert!(resp
            .citations
            .iter()
            .any(|c| c.rel_path == "src/billing.rs" && c.start_line == 2));
        assert!(resp.content.contains("kestrel://open?path=src/billing.rs"));
    }
}
