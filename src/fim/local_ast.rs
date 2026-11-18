use crate::index::{WorkspaceIndex, extractor::SymbolKind, is_ident_char};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct LocalFimInput<'a> {
    pub path: &'a Path,
    pub language: &'a str,
    pub prefix: &'a str,
    pub suffix: &'a str,
    pub full_text: &'a str,
    pub offset: usize,
    pub index: &'a WorkspaceIndex,
}

#[derive(Debug, Clone)]
pub struct LocalCandidate {
    pub text: String,
    pub reason: &'static str,
}

/// Offline AST- and WorkspaceIndex-driven FIM completion generator.
/// Produces contextually grounded completions using tree-sitter symbols,
/// local scope types, struct fields, enum variants, and language syntax.
pub fn generate_local_fim(input: &LocalFimInput<'_>) -> Vec<LocalCandidate> {
    let mut out = Vec::new();
    let current_line = input.prefix.rsplit('\n').next().unwrap_or("");
    let indent = leading_whitespace(current_line);
    let trimmed = current_line.trim();
    let suffix_trimmed = input.suffix.trim_start();

    // 1. Doc comment generation when user types `/// ` above a signature
    if trimmed == "///" || trimmed == "//!" {
        if let Some(doc) = suggest_doc_comment(input.suffix, indent) {
            out.push(LocalCandidate {
                text: doc,
                reason: "Docstring from AST signature",
            });
        }
    }

    // 2. Match expression arm scaffolding for known Enum types
    if let Some(arms) = suggest_match_arms(input, current_line, indent) {
        out.push(LocalCandidate {
            text: arms,
            reason: "Enum variants from WorkspaceIndex",
        });
    }

    // 3. Struct literal fields scaffolding (`StructName {` or `StructName { `)
    if let Some(fields) = suggest_struct_literal(input, current_line, indent) {
        out.push(LocalCandidate {
            text: fields,
            reason: "Struct fields from WorkspaceIndex",
        });
    }

    // 4. Member / method completion after `.` or `::`
    if let Some(mut members) = suggest_member_after_separator(input, current_line) {
        out.append(&mut members);
    }

    // 5. Function call argument completion right after `func_name(`
    if let Some(args) = suggest_call_arguments(input, current_line) {
        out.push(LocalCandidate {
            text: args,
            reason: "Call arguments from symbol signature",
        });
    }

    // 6. Control-flow / block body continuation
    if let Some(body) = suggest_block_continuation(input, current_line, indent, suffix_trimmed) {
        out.push(LocalCandidate {
            text: body,
            reason: "AST block continuation",
        });
    }

    // 7. Identifier completion from current file + WorkspaceIndex
    if out.is_empty() {
        suggest_identifier_completion(input, current_line, &mut out);
    }

    // Deduplicate while preserving rank order
    let mut deduped: Vec<LocalCandidate> = Vec::new();
    for cand in out {
        if !cand.text.is_empty() && !deduped.iter().any(|existing| existing.text == cand.text) {
            deduped.push(cand);
        }
    }
    deduped.truncate(3);
    deduped
}

fn suggest_doc_comment(suffix: &str, indent: &str) -> Option<String> {
    let next_line = suffix.lines().find(|l| !l.trim().is_empty())?.trim();
    if let Some(rest) = next_line
        .strip_prefix("pub fn ")
        .or_else(|| next_line.strip_prefix("pub async fn "))
        .or_else(|| next_line.strip_prefix("async fn "))
        .or_else(|| next_line.strip_prefix("fn "))
    {
        let fn_name = rest.split(['(', '<']).next()?.trim();
        let readable = fn_name.replace('_', " ");
        return Some(format!(" {readable}."));
    }
    if let Some(rest) = next_line
        .strip_prefix("pub struct ")
        .or_else(|| next_line.strip_prefix("struct "))
    {
        let name = rest.split(['{', '<', '(']).next()?.trim();
        return Some(format!(" {name} state and configuration.\n{indent}///"));
    }
    None
}

fn suggest_match_arms(
    input: &LocalFimInput<'_>,
    current_line: &str,
    indent: &str,
) -> Option<String> {
    let on_new_line = current_line.trim().is_empty();
    let target_line = if on_new_line {
        prev_non_empty_line(input.prefix)?
    } else {
        current_line
    };
    let trimmed = target_line.trim();
    if !trimmed.ends_with('{') {
        return None;
    }
    // Check if line looks like `match expr {`
    let after_match = trimmed.strip_prefix("match ")?;
    let expr = after_match.trim_end_matches('{').trim();
    let var_name = expr.trim_start_matches('&').split('.').next()?.trim();

    // Find a type name in local prefix (e.g. `status: Status` or `let status = Status::...`)
    let type_name = infer_var_type_in_prefix(input.prefix, var_name)
        .or_else(|| capitalize_first(var_name))?;

    let members = input.index.members_for_symbol(&type_name);
    if members.is_empty() {
        return None;
    }
    let base_indent = leading_whitespace(target_line);
    let body_indent = format!("{base_indent}    ");
    let arm_indent = format!("{body_indent}    ");
    let mut buf = String::new();
    for (idx, m) in members.iter().take(8).enumerate() {
        if idx == 0 && on_new_line {
            if indent.len() < body_indent.len() {
                buf.push_str(&body_indent[indent.len()..]);
            }
        } else {
            buf.push('\n');
            buf.push_str(&body_indent);
        }
        buf.push_str(&format!("{type_name}::{m} => {{\n{arm_indent}todo!()\n{body_indent}}},"));
    }
    if !input.suffix.trim_start().starts_with('}') {
        buf.push('\n');
        buf.push_str(base_indent);
        buf.push('}');
    }
    Some(buf)
}

fn suggest_struct_literal(
    input: &LocalFimInput<'_>,
    current_line: &str,
    indent: &str,
) -> Option<String> {
    let on_new_line = current_line.trim().is_empty();
    let target_line = if on_new_line {
        prev_non_empty_line(input.prefix)?
    } else {
        current_line
    };
    let trimmed = target_line.trim_end();
    if !trimmed.ends_with('{') {
        return None;
    }
    let before_brace = trimmed.trim_end_matches('{').trim();
    let type_name = before_brace
        .rsplit(|c: char| !is_ident_char(c))
        .next()
        .unwrap_or("");
    if type_name.is_empty() || !type_name.chars().next()?.is_ascii_uppercase() {
        return None;
    }
    let fields = input.index.members_for_symbol(type_name);
    if fields.is_empty() {
        return None;
    }
    let base_indent = leading_whitespace(target_line);
    let field_indent = format!("{base_indent}    ");
    let mut buf = String::new();
    for (idx, f) in fields.iter().take(8).enumerate() {
        if idx == 0 && on_new_line {
            if indent.len() < field_indent.len() {
                buf.push_str(&field_indent[indent.len()..]);
            }
        } else {
            buf.push('\n');
            buf.push_str(&field_indent);
        }
        buf.push_str(&format!("{f},"));
    }
    if !input.suffix.trim_start().starts_with('}') {
        buf.push('\n');
        buf.push_str(base_indent);
        buf.push('}');
    }
    Some(buf)
}

fn prev_non_empty_line(prefix: &str) -> Option<&str> {
    let without_cur = prefix.rsplit_once('\n')?.0;
    without_cur.rsplit('\n').find(|l| !l.trim().is_empty())
}

fn suggest_member_after_separator(
    input: &LocalFimInput<'_>,
    current_line: &str,
) -> Option<Vec<LocalCandidate>> {
    // Pattern 1: `TypeName::partial`
    if let Some(idx) = current_line.rfind("::") {
        let before = &current_line[..idx];
        let partial = &current_line[idx + 2..];
        if partial.chars().all(is_ident_char) {
            let type_name = before
                .rsplit(|c: char| !is_ident_char(c))
                .next()
                .unwrap_or("");
            if !type_name.is_empty() {
                let mut candidates = Vec::new();
                // Check enum variants / struct fields first
                for member in input.index.members_for_symbol(type_name) {
                    if member.starts_with(partial) && member != partial {
                        candidates.push(LocalCandidate {
                            text: member[partial.len()..].to_string(),
                            reason: "Type member from WorkspaceIndex",
                        });
                    }
                }
                // Check methods in file symbols
                for sym in input.index.search_symbols(partial) {
                    if matches!(sym.location.kind, SymbolKind::Method | SymbolKind::Function)
                        && sym.location.symbol_name.starts_with(partial)
                        && sym.location.symbol_name != partial
                    {
                        let snippet = format_call_snippet(&sym.location.signature, &sym.location.symbol_name);
                        if let Some(rest) = snippet.strip_prefix(partial) {
                            candidates.push(LocalCandidate {
                                text: rest.to_string(),
                                reason: "Associated method from WorkspaceIndex",
                            });
                        }
                    }
                }
                if !candidates.is_empty() {
                    return Some(candidates);
                }
            }
        }
    }

    // Pattern 2: `var.partial`
    if let Some(idx) = current_line.rfind('.') {
        let before = &current_line[..idx];
        let partial = &current_line[idx + 1..];
        if partial.chars().all(is_ident_char) {
            let var_name = before
                .rsplit(|c: char| !is_ident_char(c))
                .next()
                .unwrap_or("");
            if !var_name.is_empty() {
                if let Some(type_name) = infer_var_type_in_prefix(input.prefix, var_name) {
                    let members = input.index.members_for_symbol(&type_name);
                    let mut cands = Vec::new();
                    for m in members {
                        if m.starts_with(partial) && m != partial {
                            cands.push(LocalCandidate {
                                text: m[partial.len()..].to_string(),
                                reason: "Field on inferred local type",
                            });
                        }
                    }
                    if !cands.is_empty() {
                        return Some(cands);
                    }
                }
            }
        }
    }
    None
}

fn suggest_call_arguments(input: &LocalFimInput<'_>, current_line: &str) -> Option<String> {
    let trimmed = current_line.trim_end();
    if !trimmed.ends_with('(') {
        return None;
    }
    let before = trimmed.trim_end_matches('(').trim_end();
    let fn_name = before
        .rsplit(|c: char| !is_ident_char(c))
        .next()
        .unwrap_or("");
    if fn_name.is_empty() || matches!(fn_name, "if" | "while" | "for" | "match" | "return") {
        return None;
    }
    let loc = input.index.symbol_by_name(fn_name)?;
    let sig = &loc.signature;
    let open = sig.find('(')?;
    let close = sig[open + 1..].find(')')?;
    let params_str = sig[open + 1..open + 1 + close].trim();
    if params_str.is_empty() {
        return Some(if input.suffix.starts_with(')') {
            String::new()
        } else {
            ")".to_string()
        });
    }

    let arg_names: Vec<&str> = params_str
        .split(',')
        .filter_map(|p| {
            let p = p.trim();
            if p.starts_with("&self") || p == "self" || p.starts_with("&mut self") {
                return None;
            }
            p.split(':').next().map(|n| n.trim().trim_start_matches("mut "))
        })
        .filter(|n| !n.is_empty())
        .collect();

    if arg_names.is_empty() {
        return None;
    }
    let joined = arg_names.join(", ");
    Some(if input.suffix.trim_start().starts_with(')') {
        joined
    } else {
        format!("{joined})")
    })
}

fn suggest_block_continuation(
    input: &LocalFimInput<'_>,
    current_line: &str,
    indent: &str,
    suffix_trimmed: &str,
) -> Option<String> {
    let trimmed = current_line.trim();
    let inner_indent = format!("{indent}    ");

    match input.language {
        "rust" => {
            if trimmed.starts_with("pub fn ") || trimmed.starts_with("fn ") {
                if trimmed.ends_with('{') && !suffix_trimmed.starts_with('}') {
                    return Some(format!("\n{inner_indent}todo!()\n{indent}}}"));
                }
                if trimmed.ends_with(')') && !trimmed.contains('{') && !suffix_trimmed.starts_with('{') {
                    return Some(format!(" {{\n{inner_indent}todo!()\n{indent}}}"));
                }
            }
            if trimmed == "impl" || (trimmed.starts_with("impl ") && trimmed.ends_with('{')) {
                if trimmed.ends_with('{') && !suffix_trimmed.starts_with('}') {
                    return Some(format!("\n{inner_indent}pub fn new() -> Self {{\n{inner_indent}    todo!()\n{inner_indent}}}\n{indent}}}"));
                }
            }
            if trimmed == "#[test]" {
                return Some(format!("\n{indent}fn works() {{\n{inner_indent}assert!(true);\n{indent}}}"));
            }
            if (trimmed.starts_with("if ") || trimmed.starts_with("for ") || trimmed.starts_with("while "))
                && trimmed.ends_with('{')
                && !suffix_trimmed.starts_with('}')
            {
                return Some(format!("\n{inner_indent}\n{indent}}}"));
            }
            if trimmed.starts_with("let ") && !trimmed.ends_with(';') && !trimmed.ends_with('{') && !trimmed.ends_with('=') && suffix_trimmed.is_empty() {
                return Some(";".to_string());
            }
        }
        "typescript" | "tsx" | "javascript" => {
            if (trimmed.starts_with("function ") || trimmed.starts_with("export function "))
                && trimmed.ends_with('{')
                && !suffix_trimmed.starts_with('}')
            {
                return Some(format!("\n{inner_indent}return;\n{indent}}}"));
            }
            if trimmed.starts_with("interface ") && trimmed.ends_with('{') && !suffix_trimmed.starts_with('}') {
                return Some(format!("\n{inner_indent}id: string;\n{indent}}}"));
            }
        }
        "python" => {
            if trimmed.starts_with("def ") && trimmed.ends_with(':') {
                return Some(format!("\n{inner_indent}pass"));
            }
            if trimmed.starts_with("class ") && trimmed.ends_with(':') {
                return Some(format!("\n{inner_indent}def __init__(self):\n{inner_indent}    pass"));
            }
        }
        "go" => {
            if trimmed == "if err != nil" {
                return Some(format!(" {{\n{inner_indent}return err\n{indent}}}"));
            }
            if trimmed.starts_with("func ") && trimmed.ends_with('{') && !suffix_trimmed.starts_with('}') {
                return Some(format!("\n{inner_indent}return\n{indent}}}"));
            }
        }
        _ => {}
    }
    None
}

fn suggest_identifier_completion(
    input: &LocalFimInput<'_>,
    current_line: &str,
    out: &mut Vec<LocalCandidate>,
) {
    let token_start = current_line
        .rfind(|c: char| !is_ident_char(c))
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let partial = &current_line[token_start..];
    if partial.len() < 2 {
        return;
    }

    let cands = input
        .index
        .completion_candidates(input.path, partial, input.full_text);
    for c in cands {
        if c.label.starts_with(partial) && c.label.len() > partial.len() {
            let text = if matches!(
                c.kind,
                lsp_types::CompletionItemKind::FUNCTION | lsp_types::CompletionItemKind::METHOD
            ) && !c.detail.contains("buffer")
            {
                let snippet = format_call_snippet(&c.detail, &c.label);
                snippet[partial.len()..].to_string()
            } else {
                c.label[partial.len()..].to_string()
            };
            out.push(LocalCandidate {
                text,
                reason: "Workspace symbol prefix",
            });
            if out.len() >= 3 {
                break;
            }
        }
    }
}

fn format_call_snippet(signature: &str, name: &str) -> String {
    if let Some(open) = signature.find('(') {
        if let Some(close) = signature[open + 1..].find(')') {
            let params = signature[open + 1..open + 1 + close].trim();
            if params.is_empty() || params == "&self" || params == "&mut self" || params == "self" {
                return format!("{name}()");
            }
        }
    }
    name.to_string()
}

fn infer_var_type_in_prefix(prefix: &str, var_name: &str) -> Option<String> {
    // Search backwards for `<var_name>: ... TypeName` or `let <var_name> = TypeName::`
    let needle_colon = format!("{var_name}:");
    for line in prefix.lines().rev().take(80) {
        if let Some(idx) = line.find(&needle_colon) {
            let after = line[idx + needle_colon.len()..].trim();
            let cleaned = after
                .trim_start_matches('&')
                .trim_start_matches("mut ")
                .trim_start_matches("Option<")
                .trim_start_matches("Vec<");
            let ty = cleaned
                .split(|c: char| !is_ident_char(c))
                .find(|s| !s.is_empty())?;
            if ty.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                return Some(ty.to_string());
            }
        }
        let needle_eq = format!("{var_name} = ");
        if let Some(idx) = line.find(&needle_eq) {
            let after = line[idx + needle_eq.len()..].trim();
            let ty = after
                .split(|c: char| !is_ident_char(c))
                .find(|s| !s.is_empty())?;
            if ty.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                return Some(ty.to_string());
            }
        }
    }
    None
}

fn capitalize_first(s: &str) -> Option<String> {
    let mut chars = s.chars();
    let first = chars.next()?;
    if first.is_ascii_uppercase() {
        return Some(s.to_string());
    }
    Some(format!("{}{}", first.to_ascii_uppercase(), chars.as_str()))
}

fn leading_whitespace(line: &str) -> &str {
    let len = line
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(idx, _)| idx)
        .unwrap_or(line.len());
    &line[..len]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn generates_struct_literal_fields_from_workspace_index() {
        let dir = TempDir::new().unwrap();
        let models = dir.path().join("models.rs");
        fs::write(
            &models,
            "pub struct Point {\n    pub x: f32,\n    pub y: f32,\n}\n",
        )
        .unwrap();
        let main = dir.path().join("main.rs");
        let code = "fn make() -> Point {\n    Point {\n";
        fs::write(&main, code).unwrap();

        let index = WorkspaceIndex::new();
        index.index_workspace(dir.path());

        let cands = generate_local_fim(&LocalFimInput {
            path: &main,
            language: "rust",
            prefix: code,
            suffix: "",
            full_text: code,
            offset: code.len(),
            index: &index,
        });
        assert!(!cands.is_empty());
        assert!(cands[0].text.contains("x,"));
        assert!(cands[0].text.contains("y,"));
    }

    #[test]
    fn generates_match_arms_from_enum_variants() {
        let dir = TempDir::new().unwrap();
        let main = dir.path().join("main.rs");
        let code = "pub enum Mode {\n    Fast,\n    Accurate,\n}\nfn run(mode: Mode) {\n    match mode {";
        fs::write(&main, code).unwrap();

        let index = WorkspaceIndex::new();
        index.index_workspace(dir.path());

        let cands = generate_local_fim(&LocalFimInput {
            path: &main,
            language: "rust",
            prefix: code,
            suffix: "",
            full_text: code,
            offset: code.len(),
            index: &index,
        });
        assert!(!cands.is_empty());
        assert!(cands[0].text.contains("Mode::Fast =>"));
        assert!(cands[0].text.contains("Mode::Accurate =>"));
    }
}
