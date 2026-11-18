pub mod client;
pub mod local_ast;

use crate::{
    config::{FimConfig, FimProviderKind},
    index::{FimContextSnippet, WorkspaceIndex, ceil_char_boundary, floor_char_boundary},
};
use client::{RemoteFimQuery, request_remote_fim};
use local_ast::{LocalFimInput, generate_local_fim};
use std::{
    collections::{HashMap, VecDeque, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Instant,
};

const MAX_CACHE_ENTRIES: usize = 64;
const MAX_HISTORY_ENTRIES: usize = 20;

#[derive(Debug, Clone)]
pub struct FimCandidate {
    pub text: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ActiveFimSuggestion {
    pub file_path: PathBuf,
    pub offset: usize,
    pub text_hash: u64,
    pub candidates: Vec<FimCandidate>,
    pub selected_index: usize,
    pub provider_label: String,
    pub model: String,
    pub latency_ms: u64,
    pub cache_hit: bool,
    pub prefix_preview: String,
    pub suffix_preview: String,
    pub repo_context: Vec<FimContextSnippet>,
    pub warning: Option<String>,
}

impl ActiveFimSuggestion {
    pub fn current_text(&self) -> Option<&str> {
        self.candidates
            .get(self.selected_index)
            .map(|c| c.text.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FimStatus {
    Disabled,
    Idle,
    Predicting,
    Ready,
    Cached,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct FimHistoryEntry {
    pub file_name: String,
    pub line: usize,
    pub provider: String,
    pub latency_ms: u64,
    pub cache_hit: bool,
    pub accepted: bool,
    pub snippet: String,
}

#[derive(Debug, Clone)]
pub struct FimTelemetry {
    pub status: FimStatus,
    pub provider_label: String,
    pub model_label: String,
    pub last_latency_ms: u64,
    pub total_requests: u64,
    pub cache_hits: u64,
    pub accepted_count: u64,
    pub last_error: Option<String>,
}

impl Default for FimTelemetry {
    fn default() -> Self {
        Self {
            status: FimStatus::Idle,
            provider_label: "Auto".to_string(),
            model_label: "local-ast-fim".to_string(),
            last_latency_ms: 0,
            total_requests: 0,
            cache_hits: 0,
            accepted_count: 0,
            last_error: None,
        }
    }
}

#[derive(Debug, Default)]
struct FimEngineState {
    active: Option<ActiveFimSuggestion>,
    telemetry: FimTelemetry,
    cache: HashMap<u64, Vec<FimCandidate>>,
    cache_order: VecDeque<u64>,
    history: VecDeque<FimHistoryEntry>,
}

#[derive(Clone, Default)]
pub struct FimEngine {
    inner: Arc<RwLock<FimEngineState>>,
}

impl FimEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_status(&self, status: FimStatus) {
        self.inner.write().unwrap().telemetry.status = status;
    }

    pub fn telemetry(&self) -> FimTelemetry {
        self.inner.read().unwrap().telemetry.clone()
    }

    pub fn active(&self) -> Option<ActiveFimSuggestion> {
        self.inner.read().unwrap().active.clone()
    }

    pub fn history(&self) -> Vec<FimHistoryEntry> {
        self.inner.read().unwrap().history.iter().cloned().collect()
    }

    pub fn clear_active(&self) {
        let mut guard = self.inner.write().unwrap();
        guard.active = None;
        if matches!(
            guard.telemetry.status,
            FimStatus::Ready | FimStatus::Cached | FimStatus::Predicting
        ) {
            guard.telemetry.status = FimStatus::Idle;
        }
    }

    pub fn mark_accepted(&self) {
        let mut guard = self.inner.write().unwrap();
        guard.telemetry.accepted_count += 1;
        if let Some(last) = guard.history.front_mut() {
            last.accepted = true;
        }
        guard.active = None;
        guard.telemetry.status = FimStatus::Idle;
    }

    pub fn cycle_candidate(&self, delta: isize) -> Option<String> {
        let mut guard = self.inner.write().unwrap();
        let active = guard.active.as_mut()?;
        if active.candidates.is_empty() {
            return None;
        }
        let len = active.candidates.len() as isize;
        let next = (active.selected_index as isize + delta).rem_euclid(len) as usize;
        active.selected_index = next;
        active.current_text().map(str::to_string)
    }

    /// Consumes and returns the next word (or leading indentation + word) from the active
    /// FIM suggestion, leaving the remainder cached for seamless re-display.
    pub fn take_next_word(&self, path: &Path, offset: usize) -> Option<String> {
        let mut guard = self.inner.write().unwrap();
        let active = guard.active.as_mut()?;
        if active.file_path != path || active.offset != offset {
            return None;
        }
        let text = active.candidates.get(active.selected_index)?.text.clone();
        if text.is_empty() {
            return None;
        }
        let split_at = next_word_boundary(&text);
        let chunk = text[..split_at].to_string();
        let remainder = text[split_at..].to_string();
        if remainder.is_empty() {
            guard.telemetry.accepted_count += 1;
            guard.active = None;
        } else {
            active.candidates[active.selected_index].text = remainder;
            active.offset += chunk.len();
        }
        Some(chunk)
    }

    /// Consumes and returns the next line from the active FIM suggestion.
    pub fn take_next_line(&self, path: &Path, offset: usize) -> Option<String> {
        let mut guard = self.inner.write().unwrap();
        let active = guard.active.as_mut()?;
        if active.file_path != path || active.offset != offset {
            return None;
        }
        let text = active.candidates.get(active.selected_index)?.text.clone();
        if text.is_empty() {
            return None;
        }
        let split_at = match text.find('\n') {
            Some(0) => {
                // Leading newline: consume the newline plus the next line's content
                match text[1..].find('\n') {
                    Some(idx) => 1 + idx,
                    None => text.len(),
                }
            }
            Some(idx) => idx,
            None => text.len(),
        };
        let chunk = text[..split_at].to_string();
        let remainder = text[split_at..].to_string();
        if remainder.is_empty() {
            guard.telemetry.accepted_count += 1;
            guard.active = None;
        } else {
            active.candidates[active.selected_index].text = remainder;
            active.offset += chunk.len();
        }
        Some(chunk)
    }

    /// Executes an AI FIM completion (or reuses cached remainder / LRU entry) for the given cursor.
    pub fn complete(
        &self,
        path: &Path,
        language: &str,
        offset: usize,
        full_text: &str,
        index: &WorkspaceIndex,
        cfg: &FimConfig,
    ) -> Option<ActiveFimSuggestion> {
        if !cfg.enabled {
            self.set_status(FimStatus::Disabled);
            return None;
        }

        let safe_offset = floor_char_boundary(full_text, offset);
        let text_hash = hash_str(full_text);

        // If an active suggestion already sits at this exact (file, offset) — e.g. after partial accept
        // or cycling candidates — return it immediately without an extra network roundtrip.
        {
            let guard = self.inner.read().unwrap();
            if let Some(active) = &guard.active {
                if active.file_path == path && active.offset == safe_offset {
                    return Some(active.clone());
                }
            }
        }

        let prefix_start = floor_char_boundary(full_text, safe_offset.saturating_sub(cfg.prefix_chars));
        let suffix_end = ceil_char_boundary(full_text, safe_offset + cfg.suffix_chars);
        let raw_prefix = &full_text[prefix_start..safe_offset];
        let raw_suffix = &full_text[safe_offset..suffix_end];

        let repo_context = if cfg.include_repo_context {
            index.related_context_for_fim(path, safe_offset, full_text, 6)
        } else {
            Vec::new()
        };

        let (eff_provider, endpoint, model) = cfg.resolve_auto_target();
        let cache_key = compute_cache_key(
            path,
            language,
            raw_prefix,
            raw_suffix,
            eff_provider,
            &model,
        );

        // Check LRU cache
        if let Some(cached) = self.inner.read().unwrap().cache.get(&cache_key).cloned() {
            let suggestion = ActiveFimSuggestion {
                file_path: path.to_path_buf(),
                offset: safe_offset,
                text_hash,
                candidates: cached,
                selected_index: 0,
                provider_label: eff_provider.short_label().to_string(),
                model: model.clone(),
                latency_ms: 0,
                cache_hit: true,
                prefix_preview: tail_preview(raw_prefix, 400),
                suffix_preview: head_preview(raw_suffix, 240),
                repo_context,
                warning: None,
            };
            self.record_suggestion(&suggestion, full_text);
            return Some(suggestion);
        }

        let started = Instant::now();
        let mut warning = None;
        let mut used_provider_label = eff_provider.short_label().to_string();
        let mut used_model = model.clone();

        let mut candidates: Vec<FimCandidate> = Vec::new();
        if eff_provider != FimProviderKind::LocalAst {
            let context_header = format_repo_context_header(language, &repo_context);
            let prompt_prefix = if context_header.is_empty() {
                raw_prefix.to_string()
            } else {
                format!("{context_header}\n{raw_prefix}")
            };
            let query = RemoteFimQuery {
                provider: eff_provider,
                endpoint,
                model: model.clone(),
                api_key: cfg.resolved_api_key(eff_provider),
                prompt_prefix,
                prompt_suffix: raw_suffix.to_string(),
                language: language.to_string(),
                max_tokens: cfg.max_tokens,
                temperature: cfg.temperature,
            };
            match request_remote_fim(&query) {
                Ok(results) => {
                    for text in results {
                        let trimmed = trim_overlap(&text, raw_prefix, raw_suffix);
                        if !trimmed.is_empty() {
                            candidates.push(FimCandidate {
                                text: trimmed,
                                reason: format!("Remote {}", eff_provider.short_label()),
                            });
                        }
                    }
                }
                Err(err) => {
                    warning = Some(format!("{err}"));
                }
            }
        }

        // Run Local AST when configured directly or as offline fallback on remote error.
        if eff_provider == FimProviderKind::LocalAst || (candidates.is_empty() && warning.is_some()) {
            if eff_provider != FimProviderKind::LocalAst {
                used_provider_label = "Local AST (Fallback)".to_string();
                used_model = "local-ast-fim".to_string();
            } else {
                used_provider_label = "Local AST".to_string();
                used_model = "local-ast-fim".to_string();
            }
            let locals = generate_local_fim(&LocalFimInput {
                path,
                language,
                prefix: raw_prefix,
                suffix: raw_suffix,
                full_text,
                offset: safe_offset,
                index,
            });
            for c in locals {
                let trimmed = trim_overlap(&c.text, raw_prefix, raw_suffix);
                if !trimmed.is_empty() {
                    candidates.push(FimCandidate {
                        text: trimmed,
                        reason: c.reason.to_string(),
                    });
                }
            }
        }

        let latency_ms = started.elapsed().as_millis() as u64;
        if candidates.is_empty() {
            let mut guard = self.inner.write().unwrap();
            guard.active = None;
            guard.telemetry.total_requests += 1;
            guard.telemetry.last_latency_ms = latency_ms;
            guard.telemetry.provider_label = used_provider_label;
            guard.telemetry.model_label = used_model;
            guard.telemetry.last_error = warning.clone();
            guard.telemetry.status = match warning {
                Some(w) => FimStatus::Error(w),
                None => FimStatus::Idle,
            };
            return None;
        }

        self.insert_cache(cache_key, candidates.clone());
        let suggestion = ActiveFimSuggestion {
            file_path: path.to_path_buf(),
            offset: safe_offset,
            text_hash,
            candidates,
            selected_index: 0,
            provider_label: used_provider_label,
            model: used_model,
            latency_ms,
            cache_hit: false,
            prefix_preview: tail_preview(raw_prefix, 400),
            suffix_preview: head_preview(raw_suffix, 240),
            repo_context,
            warning,
        };
        self.record_suggestion(&suggestion, full_text);
        Some(suggestion)
    }

    fn insert_cache(&self, key: u64, candidates: Vec<FimCandidate>) {
        let mut guard = self.inner.write().unwrap();
        if !guard.cache.contains_key(&key) {
            if guard.cache_order.len() >= MAX_CACHE_ENTRIES {
                if let Some(oldest) = guard.cache_order.pop_front() {
                    guard.cache.remove(&oldest);
                }
            }
            guard.cache_order.push_back(key);
        }
        guard.cache.insert(key, candidates);
    }

    fn record_suggestion(&self, suggestion: &ActiveFimSuggestion, full_text: &str) {
        let line = full_text[..suggestion.offset.min(full_text.len())]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
            + 1;
        let file_name = suggestion
            .file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".to_string());
        let snippet = suggestion
            .current_text()
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(48)
            .collect::<String>();

        let mut guard = self.inner.write().unwrap();
        guard.active = Some(suggestion.clone());
        guard.telemetry.total_requests += 1;
        if suggestion.cache_hit {
            guard.telemetry.cache_hits += 1;
        }
        guard.telemetry.last_latency_ms = suggestion.latency_ms;
        guard.telemetry.provider_label = suggestion.provider_label.clone();
        guard.telemetry.model_label = suggestion.model.clone();
        guard.telemetry.last_error = suggestion.warning.clone();
        guard.telemetry.status = if suggestion.cache_hit {
            FimStatus::Cached
        } else {
            FimStatus::Ready
        };
        if guard.history.len() >= MAX_HISTORY_ENTRIES {
            guard.history.pop_back();
        }
        guard.history.push_front(FimHistoryEntry {
            file_name,
            line,
            provider: suggestion.provider_label.clone(),
            latency_ms: suggestion.latency_ms,
            cache_hit: suggestion.cache_hit,
            accepted: false,
            snippet,
        });
    }
}

/// Removes accidental echo of the tail of `prefix` or the head of `suffix`.
pub fn trim_overlap(completion: &str, prefix: &str, suffix: &str) -> String {
    let mut s = completion.to_string();
    // If completion repeats the last few chars of prefix on the current line
    let prefix_line = prefix.rsplit('\n').next().unwrap_or("");
    if !prefix_line.is_empty() && s.starts_with(prefix_line) {
        s = s[prefix_line.len()..].to_string();
    }
    // Trim trailing overlap with suffix start (e.g. duplicated `}` or `);`)
    let suffix_trimmed = suffix.trim_start_matches([' ', '\t']);
    if !suffix_trimmed.is_empty() && !s.is_empty() {
        let max_k = s.len().min(suffix_trimmed.len()).min(48);
        for k in (1..=max_k).rev() {
            if s.is_char_boundary(s.len() - k) && suffix_trimmed.is_char_boundary(k) {
                if s[s.len() - k..] == suffix_trimmed[..k] {
                    s.truncate(s.len() - k);
                    break;
                }
            }
        }
    }
    s
}

fn format_repo_context_header(language: &str, snippets: &[FimContextSnippet]) -> String {
    if snippets.is_empty() {
        return String::new();
    }
    let comment = match language {
        "python" | "yaml" | "toml" | "bash" => "#",
        _ => "//",
    };
    let mut lines = vec![format!("{comment} Relevant workspace definitions:")];
    for s in snippets {
        lines.push(format!("{comment} {}: {}", s.rel_path, s.signature));
        if !s.members.is_empty() {
            lines.push(format!(
                "{comment}   members: {}",
                s.members.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
            ));
        }
    }
    lines.join("\n")
}

fn next_word_boundary(text: &str) -> usize {
    let mut iter = text.char_indices().peekable();
    while let Some(&(_, ch)) = iter.peek() {
        if ch.is_whitespace() {
            iter.next();
        } else {
            break;
        }
    }
    let Some((_, first_ch)) = iter.next() else {
        return text.len();
    };
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    if is_word(first_ch) {
        for (idx, ch) in iter {
            if !is_word(ch) {
                return idx;
            }
        }
        return text.len();
    }
    for (idx, ch) in iter {
        if ch.is_whitespace() || is_word(ch) {
            return idx;
        }
    }
    text.len()
}

fn compute_cache_key(
    path: &Path,
    language: &str,
    prefix: &str,
    suffix: &str,
    provider: FimProviderKind,
    model: &str,
) -> u64 {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    language.hash(&mut h);
    prefix.hash(&mut h);
    suffix.hash(&mut h);
    (provider as u8).hash(&mut h);
    model.hash(&mut h);
    h.finish()
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn tail_preview(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let start = ceil_char_boundary(s, s.len() - max_bytes);
    format!("...{}", &s[start..])
}

fn head_preview(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let end = floor_char_boundary(s, max_bytes);
    format!("{}...", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_duplicate_prefix_and_suffix_overlap() {
        let out = trim_overlap("foo(bar);", "foo", ");\n}");
        assert_eq!(out, "(bar");
    }

    #[test]
    fn partial_word_and_line_acceptance_splits_cleanly() {
        let engine = FimEngine::new();
        let path = PathBuf::from("demo.rs");
        {
            let mut guard = engine.inner.write().unwrap();
            guard.active = Some(ActiveFimSuggestion {
                file_path: path.clone(),
                offset: 10,
                text_hash: 1,
                candidates: vec![FimCandidate {
                    text: "let value = 42;\n    println!(\"{value}\");".to_string(),
                    reason: "test".to_string(),
                }],
                selected_index: 0,
                provider_label: "Local AST".to_string(),
                model: "local".to_string(),
                latency_ms: 1,
                cache_hit: false,
                prefix_preview: String::new(),
                suffix_preview: String::new(),
                repo_context: Vec::new(),
                warning: None,
            });
        }

        let w1 = engine.take_next_word(&path, 10).unwrap();
        assert_eq!(w1, "let");
        let w2 = engine.take_next_word(&path, 13).unwrap();
        assert_eq!(w2, " value");
        let line = engine.take_next_line(&path, 19).unwrap();
        assert_eq!(line, " = 42;");
        let rem = engine.take_next_line(&path, 25).unwrap();
        assert_eq!(rem, "\n    println!(\"{value}\");");
        assert!(engine.active().is_none());
    }
}
