use gpui_kit::component::highlighter::LanguageRegistry;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, ops::Range, path::Path};
use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Interface,
    Class,
    Impl,
    Module,
    Constant,
    TypeAlias,
    Field,
    Variant,
    Variable,
    Section,
}

impl SymbolKind {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Method => "m",
            Self::Struct => "S",
            Self::Enum => "E",
            Self::Trait => "T",
            Self::Interface => "I",
            Self::Class => "C",
            Self::Impl => "im",
            Self::Module => "mod",
            Self::Constant => "K",
            Self::TypeAlias => "ty",
            Self::Field => "f",
            Self::Variant => "v",
            Self::Variable => "let",
            Self::Section => "#",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Class => "class",
            Self::Impl => "impl",
            Self::Module => "module",
            Self::Constant => "constant",
            Self::TypeAlias => "type",
            Self::Field => "field",
            Self::Variant => "variant",
            Self::Variable => "variable",
            Self::Section => "section",
        }
    }

    pub fn to_lsp_kind(self) -> lsp_types::SymbolKind {
        match self {
            Self::Function => lsp_types::SymbolKind::FUNCTION,
            Self::Method => lsp_types::SymbolKind::METHOD,
            Self::Struct => lsp_types::SymbolKind::STRUCT,
            Self::Enum => lsp_types::SymbolKind::ENUM,
            Self::Trait | Self::Interface => lsp_types::SymbolKind::INTERFACE,
            Self::Class | Self::Impl => lsp_types::SymbolKind::CLASS,
            Self::Module => lsp_types::SymbolKind::MODULE,
            Self::Constant => lsp_types::SymbolKind::CONSTANT,
            Self::TypeAlias => lsp_types::SymbolKind::TYPE_PARAMETER,
            Self::Field => lsp_types::SymbolKind::FIELD,
            Self::Variant => lsp_types::SymbolKind::ENUM_MEMBER,
            Self::Variable => lsp_types::SymbolKind::VARIABLE,
            Self::Section => lsp_types::SymbolKind::NAMESPACE,
        }
    }

    pub fn to_completion_kind(self) -> lsp_types::CompletionItemKind {
        match self {
            Self::Function => lsp_types::CompletionItemKind::FUNCTION,
            Self::Method => lsp_types::CompletionItemKind::METHOD,
            Self::Struct => lsp_types::CompletionItemKind::STRUCT,
            Self::Enum => lsp_types::CompletionItemKind::ENUM,
            Self::Trait | Self::Interface => lsp_types::CompletionItemKind::INTERFACE,
            Self::Class | Self::Impl => lsp_types::CompletionItemKind::CLASS,
            Self::Module | Self::Section => lsp_types::CompletionItemKind::MODULE,
            Self::Constant => lsp_types::CompletionItemKind::CONSTANT,
            Self::TypeAlias => lsp_types::CompletionItemKind::TYPE_PARAMETER,
            Self::Field => lsp_types::CompletionItemKind::FIELD,
            Self::Variant => lsp_types::CompletionItemKind::ENUM_MEMBER,
            Self::Variable => lsp_types::CompletionItemKind::VARIABLE,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SymbolEntry {
    pub name: String,
    pub kind: SymbolKind,
    pub signature: String,
    pub doc_comment: Option<String>,
    pub byte_range: Range<usize>,
    pub selection_range: Range<usize>,
    /// 0-based line & UTF-8 column.
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub parent_idx: Option<usize>,
    pub members: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ImportEntry {
    pub raw: String,
    pub names: Vec<String>,
    pub line: u32,
    pub byte_range: Range<usize>,
}

#[derive(Debug, Clone)]
pub struct ReferenceEntry {
    pub name: String,
    pub byte_range: Range<usize>,
    pub line: u32,
    pub col: u32,
    pub line_text: String,
    pub enclosing_symbol_idx: Option<usize>,
    pub is_definition: bool,
    pub is_call: bool,
}

#[derive(Debug, Clone)]
pub struct IndexDiagnostic {
    pub line: u32,
    pub col: u32,
    pub byte_range: Range<usize>,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct ExtractedFile {
    pub language: String,
    pub symbols: Vec<SymbolEntry>,
    pub imports: Vec<ImportEntry>,
    pub references: Vec<ReferenceEntry>,
    pub diagnostics: Vec<IndexDiagnostic>,
    pub line_count: usize,
}

/// Maps a file path to a `gpui_component` / tree-sitter language name.
pub fn detect_language(path: &Path) -> &'static str {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if file_name.eq_ignore_ascii_case("Makefile") {
        return "make";
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascript",
        "py" | "pyi" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "md" | "markdown" | "mdx" => "markdown",
        "yaml" | "yml" => "yaml",
        "sh" | "bash" | "zsh" => "bash",
        "css" | "scss" => "css",
        "html" | "htm" => "html",
        _ => "text",
    }
}

pub fn extract_file(path: &Path, text: &str) -> ExtractedFile {
    let language = detect_language(path);
    let line_count = text.lines().count().max(1);
    let mut out = ExtractedFile {
        language: language.to_string(),
        line_count,
        ..Default::default()
    };

    let Some(grammar) = LanguageRegistry::singleton().language(language) else {
        extract_fallback_tokens(text, &mut out);
        return out;
    };
    let Some(ts_lang) = grammar.language else {
        extract_fallback_tokens(text, &mut out);
        return out;
    };

    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        extract_fallback_tokens(text, &mut out);
        return out;
    }
    let Some(tree) = parser.parse(text, None) else {
        return out;
    };

    let lines: Vec<&str> = text.lines().collect();
    let mut def_ranges: HashSet<(usize, usize)> = HashSet::new();

    let ctx = ExtractContext {
        language,
        text,
        lines: &lines,
    };
    walk_symbols(
        tree.root_node(),
        &ctx,
        None,
        &mut out.symbols,
        &mut out.imports,
        &mut def_ranges,
    );

    // Collect references and AST syntax diagnostics in a clean pass.
    walk_refs_and_errors(
        tree.root_node(),
        &ctx,
        &out.symbols,
        &def_ranges,
        &mut out.references,
        &mut out.diagnostics,
    );

    out
}

struct ExtractContext<'a> {
    language: &'a str,
    text: &'a str,
    lines: &'a [&'a str],
}

impl<'a> ExtractContext<'a> {
    fn slice(&self, node: Node) -> &str {
        &self.text[node.byte_range()]
    }

    fn line_text(&self, row: usize) -> String {
        self.lines
            .get(row)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
}

fn walk_symbols(
    node: Node,
    ctx: &ExtractContext,
    parent_idx: Option<usize>,
    symbols: &mut Vec<SymbolEntry>,
    imports: &mut Vec<ImportEntry>,
    def_ranges: &mut HashSet<(usize, usize)>,
) {
    // Check imports first
    if is_import_node(ctx.language, node.kind()) {
        if let Some(imp) = extract_import(node, ctx) {
            imports.push(imp);
        }
    }

    let mut next_parent = parent_idx;
    if let Some((entry, sel_range)) = extract_symbol_node(node, ctx, parent_idx) {
        let idx = symbols.len();
        def_ranges.insert((sel_range.start, sel_range.end));
        symbols.push(entry);
        next_parent = Some(idx);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_symbols(child, ctx, next_parent, symbols, imports, def_ranges);
    }
}

fn is_import_node(lang: &str, kind: &str) -> bool {
    match lang {
        "rust" => kind == "use_declaration" || kind == "mod_item",
        "typescript" | "tsx" | "javascript" => kind == "import_statement",
        "python" => kind == "import_statement" || kind == "import_from_statement",
        "go" => kind == "import_spec",
        "c" | "cpp" => kind == "preproc_include",
        _ => false,
    }
}

fn extract_import(node: Node, ctx: &ExtractContext) -> Option<ImportEntry> {
    let raw = collapse_ws(ctx.slice(node));
    if raw.is_empty() {
        return None;
    }
    let mut names = Vec::new();
    collect_identifiers(node, ctx.text, &mut names);
    // Keep meaningful imported identifiers
    names.retain(|n| n.len() > 1 && !matches!(n.as_str(), "self" | "super" | "crate" | "from" | "as"));
    Some(ImportEntry {
        raw,
        names,
        line: node.start_position().row as u32,
        byte_range: node.byte_range(),
    })
}

fn collect_identifiers(node: Node, text: &str, out: &mut Vec<String>) {
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "property_identifier" | "shorthand_field_identifier"
    ) {
        let s = &text[node.byte_range()];
        if !s.is_empty() && !out.iter().any(|existing| existing == s) {
            out.push(s.to_string());
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_identifiers(child, text, out);
    }
}

fn extract_symbol_node(
    node: Node,
    ctx: &ExtractContext,
    parent_idx: Option<usize>,
) -> Option<(SymbolEntry, Range<usize>)> {
    let kind_str = node.kind();
    let (sym_kind, name_node, signature) = match ctx.language {
        "rust" => match kind_str {
            "function_item" => {
                let name = node.child_by_field_name("name")?;
                let is_method = parent_idx.is_some();
                let sk = if is_method {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                (sk, name, header_signature(ctx.slice(node), &['{', ';']))
            }
            "struct_item" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Struct,
                    name,
                    header_signature(ctx.slice(node), &['{', ';']),
                )
            }
            "enum_item" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Enum,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "trait_item" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Trait,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "impl_item" => {
                let ty = node.child_by_field_name("type")?;
                let sig = header_signature(ctx.slice(node), &['{']);
                (SymbolKind::Impl, ty, sig)
            }
            "mod_item" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Module,
                    name,
                    header_signature(ctx.slice(node), &['{', ';']),
                )
            }
            "type_item" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::TypeAlias,
                    name,
                    header_signature(ctx.slice(node), &[';']),
                )
            }
            "const_item" | "static_item" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Constant,
                    name,
                    header_signature(ctx.slice(node), &['=', ';']),
                )
            }
            "field_declaration" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Field,
                    name,
                    collapse_ws(ctx.slice(node)).trim_end_matches(',').to_string(),
                )
            }
            "enum_variant" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Variant,
                    name,
                    collapse_ws(ctx.slice(node)).trim_end_matches(',').to_string(),
                )
            }
            _ => return None,
        },
        "typescript" | "tsx" | "javascript" => match kind_str {
            "function_declaration" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Function,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "method_definition" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Method,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "class_declaration" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Class,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "interface_declaration" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Interface,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "type_alias_declaration" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::TypeAlias,
                    name,
                    header_signature(ctx.slice(node), &[';']),
                )
            }
            "enum_declaration" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Enum,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "variable_declarator" => {
                let name = node.child_by_field_name("name")?;
                let value = node.child_by_field_name("value")?;
                if matches!(
                    value.kind(),
                    "arrow_function" | "function_expression" | "function"
                ) {
                    (
                        SymbolKind::Function,
                        name,
                        format!("const {} = {}", ctx.slice(name), header_signature(ctx.slice(value), &['{'])),
                    )
                } else {
                    return None;
                }
            }
            _ => return None,
        },
        "python" => match kind_str {
            "function_definition" => {
                let name = node.child_by_field_name("name")?;
                let sk = if parent_idx.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                (sk, name, header_signature(ctx.slice(node), &[':']))
            }
            "class_definition" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Class,
                    name,
                    header_signature(ctx.slice(node), &[':']),
                )
            }
            _ => return None,
        },
        "go" => match kind_str {
            "function_declaration" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Function,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "method_declaration" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Method,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "type_spec" => {
                let name = node.child_by_field_name("name")?;
                let ty = node.child_by_field_name("type");
                let sk = match ty.map(|t| t.kind()) {
                    Some("struct_type") => SymbolKind::Struct,
                    Some("interface_type") => SymbolKind::Interface,
                    _ => SymbolKind::TypeAlias,
                };
                (sk, name, header_signature(ctx.slice(node), &['{']))
            }
            _ => return None,
        },
        "c" | "cpp" => match kind_str {
            "function_definition" => {
                let decl = node.child_by_field_name("declarator")?;
                let name = find_first_identifier(decl)?;
                (
                    SymbolKind::Function,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "struct_specifier" => {
                let name = node.child_by_field_name("name")?;
                node.child_by_field_name("body")?;
                (
                    SymbolKind::Struct,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "class_specifier" => {
                let name = node.child_by_field_name("name")?;
                node.child_by_field_name("body")?;
                (
                    SymbolKind::Class,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "enum_specifier" => {
                let name = node.child_by_field_name("name")?;
                node.child_by_field_name("body")?;
                (
                    SymbolKind::Enum,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            "namespace_definition" => {
                let name = node.child_by_field_name("name")?;
                (
                    SymbolKind::Module,
                    name,
                    header_signature(ctx.slice(node), &['{']),
                )
            }
            _ => return None,
        },
        "toml" => match kind_str {
            "table" | "table_array_element" => {
                let sig = collapse_ws(ctx.slice(node).lines().next().unwrap_or(""));
                let name_node = find_first_identifier(node).unwrap_or(node);
                (SymbolKind::Section, name_node, sig)
            }
            _ => return None,
        },
        "markdown" => match kind_str {
            "atx_heading" => {
                let sig = collapse_ws(ctx.slice(node));
                (SymbolKind::Section, node, sig)
            }
            _ => return None,
        },
        _ => return None,
    };

    let raw_name = ctx.slice(name_node).trim();
    if raw_name.is_empty() {
        return None;
    }
    let name = if sym_kind == SymbolKind::Section {
        raw_name
            .trim_start_matches(['#', '[', ' '])
            .trim_end_matches([']', ' '])
            .to_string()
    } else if sym_kind == SymbolKind::Impl {
        // Handle generic or scoped impl targets cleanly (e.g. Foo<T> -> Foo)
        raw_name
            .split('<')
            .next()
            .unwrap_or(raw_name)
            .rsplit("::")
            .next()
            .unwrap_or(raw_name)
            .trim()
            .to_string()
    } else {
        raw_name.to_string()
    };

    if name.is_empty() {
        return None;
    }

    let start = node.start_position();
    let end = node.end_position();
    let doc_comment = extract_doc_comment(start.row, ctx.lines)
        .or_else(|| (ctx.language == "python").then(|| extract_python_docstring(node, ctx.text)).flatten());
    let members = extract_member_names(node, ctx.text, sym_kind);

    let sel_range = name_node.byte_range();
    let entry = SymbolEntry {
        name,
        kind: sym_kind,
        signature,
        doc_comment,
        byte_range: node.byte_range(),
        selection_range: sel_range.clone(),
        start_line: start.row as u32,
        start_col: start.column as u32,
        end_line: end.row as u32,
        end_col: end.column as u32,
        parent_idx,
        members,
    };
    Some((entry, sel_range))
}

fn find_first_identifier(node: Node) -> Option<Node> {
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "field_identifier" | "bare_key" | "quoted_key"
    ) {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_first_identifier(child) {
            return Some(found);
        }
    }
    None
}

fn extract_member_names(node: Node, text: &str, kind: SymbolKind) -> Vec<String> {
    if !matches!(
        kind,
        SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Interface | SymbolKind::Class
    ) {
        return Vec::new();
    }
    let mut members = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let mut inner = child.walk();
        for item in child.children(&mut inner) {
            if matches!(
                item.kind(),
                "field_declaration"
                    | "enum_variant"
                    | "property_signature"
                    | "field_declaration_list"
            ) {
                if let Some(name) = item.child_by_field_name("name") {
                    let s = text[name.byte_range()].trim().to_string();
                    if !s.is_empty() && !members.contains(&s) {
                        members.push(s);
                    }
                }
            }
        }
    }
    members
}

fn extract_doc_comment(start_row: usize, lines: &[&str]) -> Option<String> {
    if start_row == 0 {
        return None;
    }
    let mut collected = Vec::new();
    let mut row = start_row;
    let mut in_block_comment = false;
    while row > 0 {
        row -= 1;
        let line = lines.get(row).copied().unwrap_or("").trim();
        if line.starts_with("///") {
            collected.push(line.trim_start_matches("///").trim().to_string());
        } else if line.starts_with("//!") {
            collected.push(line.trim_start_matches("//!").trim().to_string());
        } else if !in_block_comment && (line.starts_with("#[") || line.starts_with('@')) {
            // Skip attributes/decorators right above item
            continue;
        } else if !in_block_comment && (line.starts_with("# ") || line == "#") {
            collected.push(line.trim_start_matches('#').trim().to_string());
        } else if line.starts_with("/**") && line.ends_with("*/") && line.len() >= 5 {
            let inner = line
                .trim_start_matches("/**")
                .trim_end_matches("*/")
                .trim();
            if !inner.is_empty() {
                collected.push(inner.to_string());
            }
            break;
        } else if line.ends_with("*/") && !in_block_comment {
            in_block_comment = true;
            let inner = line
                .trim_start_matches("/**")
                .trim_start_matches("/*")
                .trim_end_matches("*/")
                .trim_start_matches('*')
                .trim();
            if !inner.is_empty() {
                collected.push(inner.to_string());
            }
            if line.starts_with("/*") {
                break;
            }
        } else if in_block_comment {
            let clean = line
                .trim_start_matches("/**")
                .trim_start_matches("/*")
                .trim_start_matches('*')
                .trim();
            if !clean.is_empty() {
                collected.push(clean.to_string());
            }
            if line.starts_with("/*") {
                break;
            }
        } else {
            break;
        }
    }
    if collected.is_empty() {
        return None;
    }
    collected.reverse();
    Some(collected.join("\n"))
}

fn extract_python_docstring(node: Node, text: &str) -> Option<String> {
    let body = node.child_by_field_name("body").or_else(|| {
        let mut cursor = node.walk();
        let res = node
            .children(&mut cursor)
            .find(|c| c.kind() == "block");
        res
    })?;
    let mut cursor = body.walk();
    let first_stmt = body
        .children(&mut cursor)
        .find(|c| c.kind() == "expression_statement")?;
    let mut stmt_cursor = first_stmt.walk();
    let str_node = first_stmt
        .children(&mut stmt_cursor)
        .find(|c| c.kind() == "string")?;
    let raw = text[str_node.byte_range()].trim();
    let stripped = if let Some(inner) = raw.strip_prefix("\"\"\"").and_then(|s| s.strip_suffix("\"\"\"")) {
        inner
    } else if let Some(inner) = raw.strip_prefix("'''").and_then(|s| s.strip_suffix("'''")) {
        inner
    } else if let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        inner
    } else if let Some(inner) = raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        inner
    } else {
        return None;
    };
    let normalized = stripped
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!normalized.is_empty()).then_some(normalized)
}

fn walk_refs_and_errors(
    node: Node,
    ctx: &ExtractContext,
    symbols: &[SymbolEntry],
    def_ranges: &HashSet<(usize, usize)>,
    references: &mut Vec<ReferenceEntry>,
    diagnostics: &mut Vec<IndexDiagnostic>,
) {
    if node.is_error() || node.is_missing() {
        let pos = node.start_position();
        let message = if node.is_missing() {
            format!("Missing syntax token near `{}`", collapse_ws(ctx.slice(node)))
        } else {
            format!("Syntax error near `{}`", collapse_ws(ctx.slice(node)))
        };
        diagnostics.push(IndexDiagnostic {
            line: pos.row as u32,
            col: pos.column as u32,
            byte_range: node.byte_range(),
            message,
        });
    }

    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "field_identifier" | "property_identifier"
    ) {
        let range = node.byte_range();
        let name = &ctx.text[range.clone()];
        if !name.is_empty() && name.len() <= 80 {
            let is_definition = def_ranges.contains(&(range.start, range.end));
            let is_call = node.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    "call_expression"
                        | "method_call_expression"
                        | "macro_invocation"
                        | "call"
                ) || p.parent().is_some_and(|gp| {
                    matches!(
                        gp.kind(),
                        "call_expression" | "method_call_expression" | "call"
                    )
                })
            });
            let pos = node.start_position();
            let enclosing_symbol_idx = enclosing_symbol_at(symbols, range.start);
            references.push(ReferenceEntry {
                name: name.to_string(),
                byte_range: range,
                line: pos.row as u32,
                col: pos.column as u32,
                line_text: ctx.line_text(pos.row),
                enclosing_symbol_idx,
                is_definition,
                is_call,
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_refs_and_errors(child, ctx, symbols, def_ranges, references, diagnostics);
    }
}

fn enclosing_symbol_at(symbols: &[SymbolEntry], offset: usize) -> Option<usize> {
    symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| s.byte_range.start <= offset && offset <= s.byte_range.end)
        .min_by_key(|(_, s)| s.byte_range.end - s.byte_range.start)
        .map(|(idx, _)| idx)
}

fn extract_fallback_tokens(text: &str, out: &mut ExtractedFile) {
    for (row, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("# ") {
            let offset = text.find(line).unwrap_or(0);
            out.symbols.push(SymbolEntry {
                name: heading.trim().to_string(),
                kind: SymbolKind::Section,
                signature: trimmed.to_string(),
                doc_comment: None,
                byte_range: offset..offset + line.len(),
                selection_range: offset..offset + line.len(),
                start_line: row as u32,
                start_col: 0,
                end_line: row as u32,
                end_col: line.len() as u32,
                parent_idx: None,
                members: Vec::new(),
            });
        }
    }
}

fn header_signature(raw: &str, terminators: &[char]) -> String {
    let cutoff = raw
        .find(terminators)
        .unwrap_or_else(|| raw.find('\n').unwrap_or(raw.len()));
    let head = &raw[..cutoff.min(raw.len())];
    let compact = collapse_ws(head);
    if compact.len() > 140 {
        format!("{}...", &compact[..137])
    } else {
        compact
    }
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_symbols_imports_refs_and_members() {
        let code = r#"
use std::collections::HashMap;

/// Represents a user profile.
pub struct User {
    pub id: u64,
    pub name: String,
}

impl User {
    /// Creates a new user.
    pub fn new(id: u64, name: String) -> Self {
        Self { id, name }
    }
}

pub fn greet(user: &User) -> String {
    let created = User::new(user.id, user.name.clone());
    created.name
}
"#;
        let extracted = extract_file(Path::new("src/user.rs"), code);
        assert_eq!(extracted.language, "rust");
        assert!(
            extracted
                .symbols
                .iter()
                .any(|s| s.name == "User" && s.kind == SymbolKind::Struct)
        );
        let user_sym = extracted
            .symbols
            .iter()
            .find(|s| s.name == "User" && s.kind == SymbolKind::Struct)
            .unwrap();
        assert_eq!(
            user_sym.doc_comment.as_deref(),
            Some("Represents a user profile.")
        );
        assert!(user_sym.members.contains(&"id".to_string()));
        assert!(user_sym.members.contains(&"name".to_string()));
        assert!(
            extracted
                .symbols
                .iter()
                .any(|s| s.name == "new" && s.kind == SymbolKind::Method)
        );
        assert!(
            extracted
                .symbols
                .iter()
                .any(|s| s.name == "greet" && s.kind == SymbolKind::Function)
        );
        assert!(!extracted.imports.is_empty());
        assert!(extracted.references.iter().any(|r| r.name == "User" && !r.is_definition));
    }

    #[test]
    fn extracts_typescript_symbols_and_imports() {
        let code = r#"
import { Engine } from './engine';

export interface Config {
    maxTokens: number;
}

export class Runner {
    run(cfg: Config): void {
        const e = new Engine();
    }
}
"#;
        let extracted = extract_file(Path::new("src/runner.ts"), code);
        assert_eq!(extracted.language, "typescript");
        assert!(
            extracted
                .symbols
                .iter()
                .any(|s| s.name == "Config" && s.kind == SymbolKind::Interface)
        );
        assert!(
            extracted
                .symbols
                .iter()
                .any(|s| s.name == "Runner" && s.kind == SymbolKind::Class)
        );
        assert!(
            extracted
                .symbols
                .iter()
                .any(|s| s.name == "run" && s.kind == SymbolKind::Method)
        );
        assert_eq!(extracted.imports.len(), 1);
    }

    #[test]
    fn detects_syntax_diagnostics() {
        let bad = "fn broken( { let x = ; }";
        let extracted = extract_file(Path::new("src/broken.rs"), bad);
        assert!(!extracted.diagnostics.is_empty());
    }

    #[test]
    fn extracts_jsdoc_and_python_docstrings() {
        let ts_code = r#"
/** Configuration object for server. */
export interface ServerCfg {
    port: number;
}
"#;
        let ts = extract_file(Path::new("src/cfg.ts"), ts_code);
        let cfg = ts.symbols.iter().find(|s| s.name == "ServerCfg").unwrap();
        assert_eq!(cfg.doc_comment.as_deref(), Some("Configuration object for server."));

        let py_code = r#"
def compute(x: int) -> int:
    """Doubles the given integer."""
    return x * 2
"#;
        let py = extract_file(Path::new("src/app.py"), py_code);
        let comp = py.symbols.iter().find(|s| s.name == "compute").unwrap();
        assert_eq!(comp.doc_comment.as_deref(), Some("Doubles the given integer."));
    }
}
