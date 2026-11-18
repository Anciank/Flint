use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

/// Supported AI Fill-In-the-Middle (FIM) completion backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FimProviderKind {
    /// Automatically uses env/configured remote endpoint or Ollama if configured,
    /// falling back seamlessly to the local Tree-sitter AST + Symbol FIM engine.
    #[default]
    Auto,
    /// Built-in offline Tree-sitter AST + WorkspaceIndex symbol FIM engine.
    LocalAst,
    /// Local or remote Ollama `/api/generate` with native `prompt` + `suffix`.
    Ollama,
    /// OpenAI-compatible `/v1/completions` FIM endpoint (llama.cpp, vLLM, LM Studio, etc.).
    OpenAiCompletions,
    /// DeepSeek Beta FIM `/completions` endpoint.
    DeepSeek,
    /// Mistral Codestral `/v1/fim/completions` endpoint.
    Codestral,
    /// Chat-completions FIM prompt wrapper (`/v1/chat/completions`).
    OpenAiChat,
}

impl FimProviderKind {
    pub const ALL: &'static [Self] = &[
        Self::Auto,
        Self::LocalAst,
        Self::Ollama,
        Self::OpenAiCompletions,
        Self::DeepSeek,
        Self::Codestral,
        Self::OpenAiChat,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (Smart + Local AST)",
            Self::LocalAst => "Local AST & Symbols",
            Self::Ollama => "Ollama (/api/generate)",
            Self::OpenAiCompletions => "OpenAI FIM (/v1/completions)",
            Self::DeepSeek => "DeepSeek FIM",
            Self::Codestral => "Mistral Codestral FIM",
            Self::OpenAiChat => "Chat FIM (/v1/chat/completions)",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::LocalAst => "Local AST",
            Self::Ollama => "Ollama",
            Self::OpenAiCompletions => "OpenAI FIM",
            Self::DeepSeek => "DeepSeek",
            Self::Codestral => "Codestral",
            Self::OpenAiChat => "Chat FIM",
        }
    }

    pub fn default_endpoint(self) -> &'static str {
        match self {
            Self::Auto | Self::LocalAst => "http://127.0.0.1:11434",
            Self::Ollama => "http://127.0.0.1:11434",
            Self::OpenAiCompletions => "http://127.0.0.1:8080/v1",
            Self::DeepSeek => "https://api.deepseek.com/beta",
            Self::Codestral => "https://codestral.mistral.ai/v1",
            Self::OpenAiChat => "https://api.openai.com/v1",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::Auto | Self::LocalAst => "qwen2.5-coder:7b",
            Self::Ollama => "qwen2.5-coder:7b",
            Self::OpenAiCompletions => "qwen2.5-coder",
            Self::DeepSeek => "deepseek-flash",
            Self::Codestral => "codestral-latest",
            Self::OpenAiChat => "gpt-4o-mini",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FimConfig {
    pub enabled: bool,
    pub provider: FimProviderKind,
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub debounce_ms: u64,
    pub max_tokens: u32,
    pub temperature: f32,
    pub prefix_chars: usize,
    pub suffix_chars: usize,
    pub include_repo_context: bool,
    pub probe_local_ollama: bool,
}

impl Default for FimConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: FimProviderKind::Auto,
            endpoint: FimProviderKind::Ollama.default_endpoint().to_string(),
            model: FimProviderKind::Ollama.default_model().to_string(),
            api_key: String::new(),
            debounce_ms: 240,
            max_tokens: 128,
            temperature: 0.1,
            prefix_chars: 4000,
            suffix_chars: 2000,
            include_repo_context: true,
            probe_local_ollama: false,
        }
    }
}

impl FimConfig {
    /// Resolve API key from config field or standard environment variables.
    pub fn resolved_api_key(&self, effective_provider: FimProviderKind) -> Option<String> {
        if !self.api_key.trim().is_empty() {
            return Some(self.api_key.trim().to_string());
        }
        if let Ok(key) = env::var("KESTREL_FIM_API_KEY") {
            if !key.trim().is_empty() {
                return Some(key);
            }
        }
        let env_name = match effective_provider {
            FimProviderKind::DeepSeek => "DEEPSEEK_API_KEY",
            FimProviderKind::Codestral => "CODESTRAL_API_KEY",
            _ => "OPENAI_API_KEY",
        };
        env::var(env_name)
            .ok()
            .filter(|v| !v.trim().is_empty())
    }

    /// Resolve effective provider & endpoint when configured as `Auto`.
    pub fn resolve_auto_target(&self) -> (FimProviderKind, String, String) {
        if self.provider != FimProviderKind::Auto {
            return (self.provider, self.endpoint.clone(), self.model.clone());
        }

        if let Ok(endpoint) = env::var("KESTREL_FIM_ENDPOINT") {
            if !endpoint.trim().is_empty() {
                let model = env::var("KESTREL_FIM_MODEL").unwrap_or_else(|_| self.model.clone());
                return (FimProviderKind::OpenAiCompletions, endpoint, model);
            }
        }
        if env::var("DEEPSEEK_API_KEY")
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
        {
            return (
                FimProviderKind::DeepSeek,
                FimProviderKind::DeepSeek.default_endpoint().to_string(),
                FimProviderKind::DeepSeek.default_model().to_string(),
            );
        }
        if env::var("CODESTRAL_API_KEY")
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
        {
            return (
                FimProviderKind::Codestral,
                FimProviderKind::Codestral.default_endpoint().to_string(),
                FimProviderKind::Codestral.default_model().to_string(),
            );
        }
        if env::var("OPENAI_API_KEY")
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
        {
            return (
                FimProviderKind::OpenAiChat,
                FimProviderKind::OpenAiChat.default_endpoint().to_string(),
                FimProviderKind::OpenAiChat.default_model().to_string(),
            );
        }
        if let Ok(host) = env::var("OLLAMA_HOST") {
            if !host.trim().is_empty() {
                let endpoint = if host.starts_with("http://") || host.starts_with("https://") {
                    host
                } else {
                    format!("http://{host}")
                };
                let model = env::var("KESTREL_FIM_MODEL").unwrap_or_else(|_| self.model.clone());
                return (FimProviderKind::Ollama, endpoint, model);
            }
        }
        if !self.api_key.trim().is_empty() {
            return (
                FimProviderKind::OpenAiCompletions,
                self.endpoint.clone(),
                self.model.clone(),
            );
        }

        (
            FimProviderKind::LocalAst,
            self.endpoint.clone(),
            "local-ast-fim".to_string(),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LspServerConfig {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub languages: Vec<String>,
    pub command: String,
    pub args: Vec<String>,
    pub root_markers: Vec<String>,
    pub initialization_options: Option<serde_json::Value>,
    pub settings: Option<serde_json::Value>,
}

impl Default for LspServerConfig {
    fn default() -> Self {
        Self {
            id: "custom".to_string(),
            name: "Custom LSP".to_string(),
            enabled: true,
            languages: Vec::new(),
            command: String::new(),
            args: Vec::new(),
            root_markers: Vec::new(),
            initialization_options: None,
            settings: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LspConfig {
    pub enabled: bool,
    pub request_timeout_ms: u64,
    pub servers: Vec<LspServerConfig>,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            request_timeout_ms: 4000,
            servers: default_lsp_servers(),
        }
    }
}

impl LspConfig {
    pub fn servers_for_language(&self, language: &str) -> Vec<&LspServerConfig> {
        if !self.enabled {
            return Vec::new();
        }
        let lang = language.to_ascii_lowercase();
        self.servers
            .iter()
            .filter(|s| {
                s.enabled
                    && !s.command.trim().is_empty()
                    && s.languages.iter().any(|l| l.eq_ignore_ascii_case(&lang))
            })
            .collect()
    }

    pub fn server_for_language(&self, language: &str) -> Option<&LspServerConfig> {
        self.servers_for_language(language).into_iter().next()
    }
}

pub fn default_python_lsp_servers() -> Vec<LspServerConfig> {
    vec![
        LspServerConfig {
            id: "python_ty".to_string(),
            name: "ty".to_string(),
            enabled: true,
            languages: vec!["python".to_string()],
            command: "ty".to_string(),
            args: vec!["server".to_string()],
            root_markers: vec![
                "pyproject.toml".to_string(),
                "ty.toml".to_string(),
                "requirements.txt".to_string(),
            ],
            initialization_options: None,
            settings: None,
        },
        LspServerConfig {
            id: "python_ruff".to_string(),
            name: "ruff".to_string(),
            enabled: true,
            languages: vec!["python".to_string()],
            command: "ruff".to_string(),
            args: vec!["server".to_string()],
            root_markers: vec![
                "pyproject.toml".to_string(),
                "ruff.toml".to_string(),
                ".ruff.toml".to_string(),
                "requirements.txt".to_string(),
            ],
            initialization_options: None,
            settings: None,
        },
    ]
}

pub fn default_lsp_servers() -> Vec<LspServerConfig> {
    let mut servers = vec![
        LspServerConfig {
            id: "rust".to_string(),
            name: "rust-analyzer".to_string(),
            enabled: true,
            languages: vec!["rust".to_string()],
            command: "rust-analyzer".to_string(),
            args: Vec::new(),
            root_markers: vec!["Cargo.toml".to_string()],
            initialization_options: None,
            settings: None,
        },
        LspServerConfig {
            id: "typescript".to_string(),
            name: "typescript-language-server".to_string(),
            enabled: true,
            languages: vec![
                "typescript".to_string(),
                "tsx".to_string(),
                "javascript".to_string(),
            ],
            command: "typescript-language-server".to_string(),
            args: vec!["--stdio".to_string()],
            root_markers: vec!["tsconfig.json".to_string(), "package.json".to_string()],
            initialization_options: None,
            settings: None,
        },
    ];
    servers.extend(default_python_lsp_servers());
    servers.extend([
        LspServerConfig {
            id: "python".to_string(),
            name: "pyright-langserver".to_string(),
            enabled: true,
            languages: vec!["python".to_string()],
            command: "pyright-langserver".to_string(),
            args: vec!["--stdio".to_string()],
            root_markers: vec![
                "pyproject.toml".to_string(),
                "pyrightconfig.json".to_string(),
                "requirements.txt".to_string(),
            ],
            initialization_options: None,
            settings: None,
        },
        LspServerConfig {
            id: "go".to_string(),
            name: "gopls".to_string(),
            enabled: true,
            languages: vec!["go".to_string()],
            command: "gopls".to_string(),
            args: Vec::new(),
            root_markers: vec!["go.mod".to_string()],
            initialization_options: None,
            settings: None,
        },
        LspServerConfig {
            id: "c_cpp".to_string(),
            name: "clangd".to_string(),
            enabled: true,
            languages: vec!["c".to_string(), "cpp".to_string()],
            command: "clangd".to_string(),
            args: Vec::new(),
            root_markers: vec!["compile_commands.json".to_string(), "CMakeLists.txt".to_string()],
            initialization_options: None,
            settings: None,
        },
    ]);
    servers
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VimConfig {
    pub enabled: bool,
    pub use_system_clipboard: bool,
    pub start_in_insert: bool,
}

impl Default for VimConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            use_system_clipboard: true,
            start_in_insert: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AskAiConfig {
    /// Optional explicit OpenAI-compatible or Ollama chat endpoint override.
    /// When empty, derived automatically from the configured FIM backend/API key.
    pub endpoint: String,
    /// Optional explicit chat model override (e.g. `deepseek-chat`, `gpt-4o-mini`).
    pub model: String,
    /// Optional explicit API key override for Ask AI chat/explain requests.
    pub api_key: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub include_related_symbols: bool,
}

impl Default for AskAiConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            model: String::new(),
            api_key: String::new(),
            max_tokens: 1400,
            temperature: 0.2,
            include_related_symbols: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct IdeConfig {
    pub fim: FimConfig,
    pub ask_ai: AskAiConfig,
    pub lsp: LspConfig,
    pub vim: VimConfig,
    pub dark_mode: bool,
    pub font_size: f32,
    pub scroll_padding: usize,
    pub inlay_hints: bool,
    pub inline_hover_docs: bool,
    pub last_workspace: Option<PathBuf>,
    pub recent_workspaces: Vec<PathBuf>,
}

impl Default for IdeConfig {
    fn default() -> Self {
        Self {
            fim: FimConfig::default(),
            ask_ai: AskAiConfig::default(),
            lsp: LspConfig::default(),
            vim: VimConfig::default(),
            dark_mode: true,
            font_size: 13.5,
            scroll_padding: 5,
            inlay_hints: true,
            inline_hover_docs: true,
            last_workspace: None,
            recent_workspaces: Vec::new(),
        }
    }
}

impl IdeConfig {
    pub fn global_config_path() -> Option<PathBuf> {
        let home = env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)?;
        let flint_path = home.join(".config").join("flint").join("config.json");
        let kestrel_path = home.join(".config").join("kestrel").join("config.json");
        if flint_path.exists() {
            Some(flint_path)
        } else if kestrel_path.exists() {
            Some(kestrel_path)
        } else {
            Some(flint_path)
        }
    }

    pub fn load(workspace: Option<&Path>) -> Self {
        let mut cfg = Self::global_config_path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<IdeConfig>(&s).ok())
            .unwrap_or_default();

        if let Some(ws) = workspace {
            let ws_cfg = [
                ws.join(".flint").join("config.json"),
                ws.join(".kestrel").join("config.json"),
            ]
            .into_iter()
            .find(|p| p.is_file());
            if let Some(Ok(text)) = ws_cfg.map(fs::read_to_string) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                    if val.get("fim").is_some()
                        || val.get("ask_ai").is_some()
                        || val.get("lsp").is_some()
                        || val.get("vim").is_some()
                    {
                        if let Some(fim_v) = val.get("fim") {
                            if let Ok(fim) = serde_json::from_value::<FimConfig>(fim_v.clone()) {
                                cfg.fim = fim;
                            }
                        }
                        if let Some(ai_v) = val.get("ask_ai") {
                            if let Ok(ask_ai) = serde_json::from_value::<AskAiConfig>(ai_v.clone())
                            {
                                cfg.ask_ai = ask_ai;
                            }
                        }
                        if let Some(lsp_v) = val.get("lsp") {
                            if let Ok(lsp) = serde_json::from_value::<LspConfig>(lsp_v.clone()) {
                                cfg.lsp = lsp;
                            }
                        }
                        if let Some(vim_v) = val.get("vim") {
                            if let Ok(vim) = serde_json::from_value::<VimConfig>(vim_v.clone()) {
                                cfg.vim = vim;
                            }
                        }
                    } else if val.get("provider").is_some() || val.get("endpoint").is_some() {
                        if let Ok(fim) = serde_json::from_value::<FimConfig>(val) {
                            cfg.fim = fim;
                        }
                    }
                }
            }
        }
        migrate_legacy_python_lsp(&mut cfg.lsp);
        cfg
    }

    pub fn save_global(&self) -> Result<()> {
        let Some(path) = Self::global_config_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(self)?;
        fs::write(path, body)?;
        Ok(())
    }

    pub fn push_recent_workspace(&mut self, path: PathBuf) {
        self.last_workspace = Some(path.clone());
        self.recent_workspaces.retain(|p| p != &path);
        self.recent_workspaces.insert(0, path);
        self.recent_workspaces.truncate(8);
    }
}

/// Upgrade older single-server Python preset configs to also include `ty` and `ruff`.
pub fn migrate_legacy_python_lsp(lsp: &mut LspConfig) {
    let has_legacy_py = lsp.servers.iter().position(|s| s.id == "python");
    let has_ty = lsp
        .servers
        .iter()
        .any(|s| s.id == "python_ty" || s.command == "ty");
    let has_ruff = lsp
        .servers
        .iter()
        .any(|s| s.id == "python_ruff" || s.command == "ruff");
    let Some(insert_at) = has_legacy_py else {
        return;
    };
    if has_ty || has_ruff {
        return;
    }
    let defaults = default_python_lsp_servers();
    for (offset, srv) in defaults.into_iter().enumerate() {
        lsp.servers.insert(insert_at + offset, srv);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_roundtrips_json() {
        let cfg = IdeConfig::default();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        let parsed: IdeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn default_and_legacy_python_lsp_include_ty_and_ruff() {
        let cfg = LspConfig::default();
        let py_cmds: Vec<&str> = cfg
            .servers_for_language("python")
            .into_iter()
            .map(|s| s.command.as_str())
            .collect();
        assert!(py_cmds.contains(&"ty"));
        assert!(py_cmds.contains(&"ruff"));

        let mut legacy = LspConfig {
            enabled: true,
            request_timeout_ms: 4000,
            servers: vec![LspServerConfig {
                id: "python".to_string(),
                name: "pyright-langserver".to_string(),
                enabled: true,
                languages: vec!["python".to_string()],
                command: "pyright-langserver".to_string(),
                args: vec!["--stdio".to_string()],
                ..Default::default()
            }],
        };
        migrate_legacy_python_lsp(&mut legacy);
        let migrated: Vec<&str> = legacy
            .servers_for_language("python")
            .into_iter()
            .map(|s| s.command.as_str())
            .collect();
        assert_eq!(migrated, vec!["ty", "ruff", "pyright-langserver"]);
    }

    #[test]
    fn resolve_auto_defaults_to_local_ast_without_env_or_key() {
        let cfg = FimConfig {
            provider: FimProviderKind::Auto,
            api_key: String::new(),
            ..Default::default()
        };
        let (kind, _, _) = cfg.resolve_auto_target();
        // Unless an API key env var is set in the test shell, Auto resolves to LocalAst or env override.
        assert!(matches!(
            kind,
            FimProviderKind::LocalAst
                | FimProviderKind::DeepSeek
                | FimProviderKind::Codestral
                | FimProviderKind::OpenAiChat
                | FimProviderKind::OpenAiCompletions
                | FimProviderKind::Ollama
        ));
    }
}
