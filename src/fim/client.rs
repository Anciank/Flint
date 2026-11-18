use crate::config::{FimConfig, FimProviderKind};
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::time::Duration;
use ureq::Agent;

#[derive(Debug, Clone)]
pub struct RemoteFimQuery {
    pub provider: FimProviderKind,
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    pub prompt_prefix: String,
    pub prompt_suffix: String,
    pub language: String,
    pub max_tokens: u32,
    pub temperature: f32,
}

fn build_agent(timeout_secs: u64) -> Agent {
    let config = Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .timeout_connect(Some(Duration::from_secs(3)))
        .build();
    config.into()
}

pub fn request_remote_fim(query: &RemoteFimQuery) -> Result<Vec<String>> {
    let agent = build_agent(10);
    let base = query.endpoint.trim_end_matches('/');

    match query.provider {
        FimProviderKind::Ollama => {
            let url = if base.ends_with("/api/generate") {
                base.to_string()
            } else {
                format!("{base}/api/generate")
            };
            let payload = json!({
                "model": query.model,
                "prompt": query.prompt_prefix,
                "suffix": query.prompt_suffix,
                "stream": false,
                "options": {
                    "num_predict": query.max_tokens,
                    "temperature": query.temperature,
                    "stop": ["\n\n\n", "<|endoftext|>", "<|fim_pad|>", "<|file_separator|>"]
                }
            });
            let mut req = agent.post(&url);
            if let Some(key) = &query.api_key {
                req = req.header("Authorization", &format!("Bearer {key}"));
            }
            let body: Value = req
                .send_json(&payload)
                .context("Ollama FIM request failed")?
                .body_mut()
                .read_json()
                .context("Failed to parse Ollama response")?;
            let text = body
                .get("response")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Missing `response` field in Ollama JSON"))?;
            Ok(vec![clean_fim_output(text)])
        }
        FimProviderKind::OpenAiCompletions | FimProviderKind::DeepSeek => {
            let url = resolve_completions_url(query.provider, base);
            let payload = json!({
                "model": query.model,
                "prompt": query.prompt_prefix,
                "suffix": query.prompt_suffix,
                "max_tokens": query.max_tokens.min(4000),
                "temperature": query.temperature,
                "stop": ["\n\n\n", "<|endoftext|>"]
            });
            let mut req = agent.post(&url);
            if let Some(key) = &query.api_key {
                req = req.header("Authorization", &format!("Bearer {key}"));
            }
            let body: Value = req
                .send_json(&payload)
                .context("Completions FIM request failed")?
                .body_mut()
                .read_json()
                .context("Failed to parse completions response")?;
            let choices = body
                .get("choices")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("Missing `choices` in FIM response"))?;
            let results: Vec<String> = choices
                .iter()
                .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                .map(clean_fim_output)
                .filter(|s| !s.is_empty())
                .collect();
            Ok(results)
        }
        FimProviderKind::Codestral => {
            let url = if base.ends_with("/fim/completions") {
                base.to_string()
            } else {
                format!("{base}/fim/completions")
            };
            let payload = json!({
                "model": query.model,
                "prompt": query.prompt_prefix,
                "suffix": query.prompt_suffix,
                "max_tokens": query.max_tokens,
                "temperature": query.temperature,
                "stop": ["\n\n\n"]
            });
            let mut req = agent.post(&url);
            if let Some(key) = &query.api_key {
                req = req.header("Authorization", &format!("Bearer {key}"));
            }
            let body: Value = req
                .send_json(&payload)
                .context("Codestral FIM request failed")?
                .body_mut()
                .read_json()
                .context("Failed to parse Codestral response")?;
            let choices = body
                .get("choices")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("Missing `choices` in Codestral response"))?;
            let results: Vec<String> = choices
                .iter()
                .filter_map(|c| {
                    c.pointer("/message/content")
                        .or_else(|| c.get("text"))
                        .and_then(|t| t.as_str())
                })
                .map(clean_fim_output)
                .filter(|s| !s.is_empty())
                .collect();
            Ok(results)
        }
        FimProviderKind::OpenAiChat => {
            let url = if base.ends_with("/chat/completions") {
                base.to_string()
            } else {
                format!("{base}/chat/completions")
            };
            let system = "You are a Fill-In-the-Middle (FIM) code completion engine. Output ONLY the exact code that belongs between <FIM_PREFIX> and <FIM_SUFFIX>. Do not repeat the prefix or suffix, do not wrap in markdown fences, and do not add explanations.";
            let user = format!(
                "Language: {}\n<FIM_PREFIX>\n{}\n</FIM_PREFIX>\n<FIM_SUFFIX>\n{}\n</FIM_SUFFIX>\n<FIM_MIDDLE>",
                query.language, query.prompt_prefix, query.prompt_suffix
            );
            let payload = json!({
                "model": query.model,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": user}
                ],
                "max_tokens": query.max_tokens,
                "temperature": query.temperature
            });
            let mut req = agent.post(&url);
            if let Some(key) = &query.api_key {
                req = req.header("Authorization", &format!("Bearer {key}"));
            }
            let body: Value = req
                .send_json(&payload)
                .context("Chat FIM request failed")?
                .body_mut()
                .read_json()
                .context("Failed to parse chat FIM response")?;
            let content = body
                .pointer("/choices/0/message/content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Missing message content in chat FIM response"))?;
            Ok(vec![strip_markdown_fences(content)])
        }
        FimProviderKind::Auto | FimProviderKind::LocalAst => Ok(Vec::new()),
    }
}

pub fn test_fim_connection(cfg: &FimConfig) -> Result<String> {
    let (provider, endpoint, model) = cfg.resolve_auto_target();
    if provider == FimProviderKind::LocalAst {
        return Ok("Local AST & Symbol FIM engine is ready (offline mode).".to_string());
    }
    let query = RemoteFimQuery {
        provider,
        endpoint: endpoint.clone(),
        model: model.clone(),
        api_key: cfg.resolved_api_key(provider),
        prompt_prefix: "fn add(a: i32, b: i32) -> i32 {\n    ".to_string(),
        prompt_suffix: "\n}".to_string(),
        language: "rust".to_string(),
        max_tokens: 24,
        temperature: 0.0,
    };
    let start = std::time::Instant::now();
    let res = request_remote_fim(&query)?;
    let ms = start.elapsed().as_millis();
    let sample = res.first().map(|s| s.trim()).unwrap_or("<empty>");
    Ok(format!(
        "Connected to {} ({model}) in {ms}ms. Sample: `{sample}`",
        provider.short_label()
    ))
}

fn resolve_completions_url(provider: FimProviderKind, base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/completions") {
        return trimmed.to_string();
    }
    if provider == FimProviderKind::DeepSeek && !trimmed.ends_with("/beta") {
        let stripped = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
        return format!("{stripped}/beta/completions");
    }
    format!("{trimmed}/completions")
}

fn clean_fim_output(raw: &str) -> String {
    let mut s = raw
        .replace("<|fim_middle|>", "")
        .replace("<|endoftext|>", "")
        .replace("<|fim_pad|>", "");
    if s.ends_with('\r') {
        s.pop();
    }
    s
}

fn strip_markdown_fences(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(after_open) = trimmed.strip_prefix("```") {
        let body = if let Some(nl) = after_open.find('\n') {
            &after_open[nl + 1..]
        } else {
            after_open
        };
        if let Some(stripped) = body.strip_suffix("```") {
            return stripped.trim_end_matches('\n').to_string();
        }
        return body.to_string();
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markdown_code_fences_from_chat_fim() {
        let raw = "```rust\nlet x = 42;\n```";
        assert_eq!(strip_markdown_fences(raw), "let x = 42;");
    }

    #[test]
    fn normalizes_deepseek_beta_completions_url() {
        assert_eq!(
            resolve_completions_url(FimProviderKind::DeepSeek, "https://api.deepseek.com/beta"),
            "https://api.deepseek.com/beta/completions"
        );
        assert_eq!(
            resolve_completions_url(FimProviderKind::DeepSeek, "https://api.deepseek.com"),
            "https://api.deepseek.com/beta/completions"
        );
        assert_eq!(
            resolve_completions_url(FimProviderKind::DeepSeek, "https://api.deepseek.com/v1"),
            "https://api.deepseek.com/beta/completions"
        );
    }
}
