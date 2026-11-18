pub mod extractor;
pub mod fuzzy;

use extractor::{ExtractedFile, ImportEntry, IndexDiagnostic, SymbolEntry, SymbolKind, extract_file};
use fuzzy::fuzzy_match;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

const MAX_INDEXED_FILE_BYTES: u64 = 750 * 1024;
const MAX_INDEXED_FILES: usize = 2500;

#[derive(Debug, Clone)]
pub struct FileSummary {
    pub path: PathBuf,
    pub rel_path: String,
    pub language: String,
    pub symbol_count: usize,
    pub error_count: usize,
    pub line_count: usize,
}

#[derive(Debug, Clone)]
pub struct OutlineNode {
    pub idx: usize,
    pub name: String,
    pub kind: SymbolKind,
    pub signature: String,
    pub doc_comment: Option<String>,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub byte_range: Range<usize>,
    pub selection_range: Range<usize>,
    pub depth: usize,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavLocation {
    pub path: PathBuf,
    pub rel_path: String,
    pub symbol_name: String,
    pub kind: SymbolKind,
    pub signature: String,
    pub doc_comment: Option<String>,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub byte_range: Range<usize>,
    pub selection_range: Range<usize>,
    pub score: i64,
}

impl NavLocation {
    pub fn to_lsp_location_link(&self, origin_selection: Option<lsp_types::Range>) -> Option<lsp_types::LocationLink> {
        let uri = url::Url::from_file_path(&self.path).ok()?;
        let uri: lsp_types::Uri = uri.as_str().parse().ok()?;
        let target_range = lsp_types::Range::new(
            lsp_types::Position::new(self.start_line, self.start_col),
            lsp_types::Position::new(self.end_line, self.end_col),
        );
        let sel_range = lsp_types::Range::new(
            lsp_types::Position::new(self.start_line, self.start_col),
            lsp_types::Position::new(
                self.start_line,
                self.start_col + self.symbol_name.encode_utf16().count() as u32,
            ),
        );
        Some(lsp_types::LocationLink {
            origin_selection_range: origin_selection,
            target_uri: uri,
            target_range,
            target_selection_range: sel_range,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ReferenceHit {
    pub path: PathBuf,
    pub rel_path: String,
    pub name: String,
    pub line: u32,
    pub col: u32,
    pub byte_range: Range<usize>,
    pub line_text: String,
    pub enclosing_symbol: Option<String>,
    pub is_definition: bool,
    pub is_call: bool,
}

#[derive(Debug, Clone)]
pub struct HoverInfo {
    pub symbol_name: String,
    pub kind: SymbolKind,
    pub signature: String,
    pub doc_comment: Option<String>,
    pub defined_rel_path: String,
    pub defined_line: u32,
    pub reference_count: usize,
    pub members: Vec<String>,
    pub range: Range<usize>,
}

#[derive(Debug, Clone)]
pub struct CodeMapSnapshot {
    pub enclosing: Option<OutlineNode>,
    pub cursor_symbol: Option<String>,
    pub cursor_definitions: Vec<NavLocation>,
    pub outgoing_symbols: Vec<NavLocation>,
    pub incoming_references: Vec<ReferenceHit>,
    pub file_imports: Vec<ImportEntry>,
}

#[derive(Debug, Clone)]
pub struct SymbolHit {
    pub location: NavLocation,
    pub score: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlayHintKindTag {
    Type,
    Parameter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHintItem {
    pub line: u32,
    pub character: u32,
    pub label: String,
    pub kind: InlayHintKindTag,
}

#[derive(Debug, Clone)]
pub struct FileSearchHit {
    pub path: PathBuf,
    pub rel_path: String,
    pub language: String,
    pub symbol_count: usize,
    pub score: i64,
}

#[derive(Debug, Clone)]
pub struct TextSearchHit {
    pub path: PathBuf,
    pub rel_path: String,
    pub line: u32,
    pub col: u32,
    pub line_text: String,
    pub byte_offset: usize,
}

#[derive(Debug, Clone)]
pub struct WorkspaceDiagnostic {
    pub path: PathBuf,
    pub rel_path: String,
    pub diagnostic: IndexDiagnostic,
}

#[derive(Debug, Clone)]
pub struct CompletionCandidate {
    pub label: String,
    pub kind: lsp_types::CompletionItemKind,
    pub detail: String,
    pub documentation: Option<String>,
    pub priority: i64,
}

#[derive(Debug, Clone)]
pub struct FimContextSnippet {
    pub rel_path: String,
    pub symbol_name: String,
    pub kind: SymbolKind,
    pub signature: String,
    pub doc_comment: Option<String>,
    pub members: Vec<String>,
}

#[derive(Debug, Clone)]
struct IndexedFileRecord {
    path: PathBuf,
    rel_path: String,
    content: String,
    extracted: ExtractedFile,
}

#[derive(Debug, Default)]
struct IndexData {
    root: Option<PathBuf>,
    files: BTreeMap<PathBuf, IndexedFileRecord>,
    by_symbol_name: HashMap<String, Vec<(PathBuf, usize)>>,
    by_ref_files: HashMap<String, Vec<PathBuf>>,
    total_symbols: usize,
    total_diagnostics: usize,
    revision: u64,
}

impl IndexData {
    fn resolve_path<'a>(&self, path: &'a Path) -> Cow<'a, Path> {
        if self.files.contains_key(path) {
            Cow::Borrowed(path)
        } else {
            Cow::Owned(fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceIndex {
    inner: Arc<RwLock<IndexData>>,
}

impl WorkspaceIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.inner.read().unwrap().root.clone()
    }

    pub fn revision(&self) -> u64 {
        self.inner.read().unwrap().revision
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        let guard = self.inner.read().unwrap();
        (guard.files.len(), guard.total_symbols, guard.total_diagnostics)
    }

    pub fn index_workspace(&self, root: &Path) -> (usize, usize) {
        let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let mut records = BTreeMap::new();

        let walker = WalkBuilder::new(&canonical_root)
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .filter_entry(|entry| {
                let name = entry.file_name().to_string_lossy();
                !matches!(
                    name.as_ref(),
                    ".git" | "target" | "node_modules" | ".venv" | "dist" | "build" | ".next"
                )
            })
            .build();

        for entry in walker.flatten() {
            if records.len() >= MAX_INDEXED_FILES {
                break;
            }
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            let path = entry.path().to_path_buf();
            if !is_indexable_extension(&path) {
                continue;
            }
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            if meta.len() > MAX_INDEXED_FILE_BYTES {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let rel_path = make_rel_path(&canonical_root, &path);
            let extracted = extract_file(&path, &content);
            records.insert(
                path.clone(),
                IndexedFileRecord {
                    path,
                    rel_path,
                    content,
                    extracted,
                },
            );
        }

        let mut by_symbol_name: HashMap<String, Vec<(PathBuf, usize)>> = HashMap::new();
        let mut by_ref_files: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut total_symbols = 0usize;
        let mut total_diagnostics = 0usize;
        for (path, rec) in &records {
            total_diagnostics += rec.extracted.diagnostics.len();
            for (idx, sym) in rec.extracted.symbols.iter().enumerate() {
                total_symbols += 1;
                by_symbol_name
                    .entry(sym.name.clone())
                    .or_default()
                    .push((path.clone(), idx));
            }
            let mut seen_refs = HashSet::new();
            for r in &rec.extracted.references {
                if seen_refs.insert(r.name.as_str()) {
                    by_ref_files
                        .entry(r.name.clone())
                        .or_default()
                        .push(path.clone());
                }
            }
        }
        let file_count = records.len();

        let mut guard = self.inner.write().unwrap();
        guard.root = Some(canonical_root);
        guard.files = records;
        guard.by_symbol_name = by_symbol_name;
        guard.by_ref_files = by_ref_files;
        guard.total_symbols = total_symbols;
        guard.total_diagnostics = total_diagnostics;
        guard.revision = guard.revision.wrapping_add(1);
        (file_count, total_symbols)
    }

    /// Run Tree-sitter extraction without holding the index lock (safe for background threads).
    pub fn prepare_reindex_file(path: &Path, text: &str) -> (PathBuf, ExtractedFile) {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let extracted = extract_file(&canonical, text);
        (canonical, extracted)
    }

    pub fn commit_reindex_file(&self, canonical: PathBuf, extracted: ExtractedFile, text: String) {
        let mut guard = self.inner.write().unwrap();
        let rel_path = guard
            .root
            .as_ref()
            .map(|r| make_rel_path(r, &canonical))
            .unwrap_or_else(|| {
                canonical
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| canonical.display().to_string())
            });

        // Remove old postings and adjust cached counters for this path.
        let old_info = guard.files.get(&canonical).map(|old| {
            let old_names: Vec<String> =
                old.extracted.symbols.iter().map(|s| s.name.clone()).collect();
            let old_ref_names: HashSet<String> =
                old.extracted.references.iter().map(|r| r.name.clone()).collect();
            (
                old.extracted.symbols.len(),
                old.extracted.diagnostics.len(),
                old_names,
                old_ref_names,
            )
        });
        if let Some((sym_len, diag_len, old_names, old_ref_names)) = old_info {
            guard.total_symbols = guard.total_symbols.saturating_sub(sym_len);
            guard.total_diagnostics = guard.total_diagnostics.saturating_sub(diag_len);
            for name in old_names {
                if let Some(list) = guard.by_symbol_name.get_mut(&name) {
                    list.retain(|(p, _)| p != &canonical);
                    if list.is_empty() {
                        guard.by_symbol_name.remove(&name);
                    }
                }
            }
            for ref_name in old_ref_names {
                if let Some(paths) = guard.by_ref_files.get_mut(&ref_name) {
                    paths.retain(|p| p != &canonical);
                    if paths.is_empty() {
                        guard.by_ref_files.remove(&ref_name);
                    }
                }
            }
        }

        guard.total_symbols += extracted.symbols.len();
        guard.total_diagnostics += extracted.diagnostics.len();
        guard.revision = guard.revision.wrapping_add(1);

        for (idx, sym) in extracted.symbols.iter().enumerate() {
            guard
                .by_symbol_name
                .entry(sym.name.clone())
                .or_default()
                .push((canonical.clone(), idx));
        }
        let mut seen_refs = HashSet::new();
        for r in &extracted.references {
            if seen_refs.insert(r.name.as_str()) {
                guard
                    .by_ref_files
                    .entry(r.name.clone())
                    .or_default()
                    .push(canonical.clone());
            }
        }

        guard.files.insert(
            canonical.clone(),
            IndexedFileRecord {
                path: canonical,
                rel_path,
                content: text,
                extracted,
            },
        );
    }

    pub fn reindex_file(&self, path: &Path, text: &str) {
        let canonical = {
            let guard = self.inner.read().unwrap();
            guard.resolve_path(path).into_owned()
        };
        let extracted = extract_file(&canonical, text);
        self.commit_reindex_file(canonical, extracted, text.to_string());
    }

    pub fn rel_path_for(&self, path: &Path) -> String {
        let guard = self.inner.read().unwrap();
        let canonical = guard.resolve_path(path);
        if let Some(rec) = guard.files.get(canonical.as_ref()) {
            return rec.rel_path.clone();
        }
        if let Some(root) = &guard.root {
            return make_rel_path(root, canonical.as_ref());
        }
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    }

    pub fn file_entries(&self) -> Vec<FileSummary> {
        let guard = self.inner.read().unwrap();
        let mut out: Vec<FileSummary> = guard
            .files
            .values()
            .map(|rec| FileSummary {
                path: rec.path.clone(),
                rel_path: rec.rel_path.clone(),
                language: rec.extracted.language.clone(),
                symbol_count: rec.extracted.symbols.len(),
                error_count: rec.extracted.diagnostics.len(),
                line_count: rec.extracted.line_count,
            })
            .collect();
        out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        out
    }

    pub fn file_symbols(&self, path: &Path) -> Vec<OutlineNode> {
        let guard = self.inner.read().unwrap();
        let canonical = guard.resolve_path(path);
        let Some(rec) = guard.files.get(canonical.as_ref()) else {
            return Vec::new();
        };
        build_outline(&rec.extracted.symbols)
    }

    pub fn enclosing_chain(&self, path: &Path, offset: usize) -> Vec<OutlineNode> {
        let guard = self.inner.read().unwrap();
        let canonical = guard.resolve_path(path);
        let Some(rec) = guard.files.get(canonical.as_ref()) else {
            return Vec::new();
        };
        enclosing_chain_from_symbols(&rec.extracted.symbols, offset)
    }

    pub fn identifier_at(text: &str, offset: usize) -> Option<(String, Range<usize>)> {
        if text.is_empty() {
            return None;
        }
        let len = text.len();
        let mut probe = offset.min(len);
        if probe == len && probe > 0 {
            probe -= 1;
        }
        // Adjust to valid char boundary
        while probe > 0 && !text.is_char_boundary(probe) {
            probe -= 1;
        }
        let ch = text[probe..].chars().next()?;
        if !is_ident_char(ch) {
            if probe > 0 {
                let prev_idx = text[..probe]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                let prev_ch = text[prev_idx..].chars().next()?;
                if is_ident_char(prev_ch) {
                    probe = prev_idx;
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }

        let mut start = probe;
        for (idx, c) in text[..probe].char_indices().rev() {
            if is_ident_char(c) {
                start = idx;
            } else {
                break;
            }
        }
        let mut end = probe;
        for (idx, c) in text[probe..].char_indices() {
            if is_ident_char(c) {
                end = probe + idx + c.len_utf8();
            } else {
                break;
            }
        }
        let ident = &text[start..end];
        if ident.is_empty() || ident.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        Some((ident.to_string(), start..end))
    }

    pub fn find_definitions(&self, path: &Path, offset: usize, text: &str) -> Vec<NavLocation> {
        let Some((ident, ident_range)) = Self::identifier_at(text, offset) else {
            return Vec::new();
        };
        self.find_definitions_for_name(path, &ident, Some(ident_range))
    }

    pub fn find_definitions_for_name(
        &self,
        origin_path: &Path,
        ident: &str,
        origin_range: Option<Range<usize>>,
    ) -> Vec<NavLocation> {
        let guard = self.inner.read().unwrap();
        let canonical_origin = guard.resolve_path(origin_path);
        find_definitions_locked(&guard, canonical_origin.as_ref(), ident, origin_range)
    }

    pub fn find_references(
        &self,
        symbol_name: &str,
        preferred_file: Option<&Path>,
    ) -> Vec<ReferenceHit> {
        let guard = self.inner.read().unwrap();
        let canonical_pref = preferred_file.map(|p| guard.resolve_path(p).into_owned());
        find_references_locked(&guard, symbol_name, canonical_pref.as_deref())
    }

    pub fn hover_info(&self, path: &Path, offset: usize, text: &str) -> Option<HoverInfo> {
        let (ident, range) = Self::identifier_at(text, offset)?;
        let guard = self.inner.read().unwrap();
        let canonical = guard.resolve_path(path);
        let defs = find_definitions_locked(&guard, canonical.as_ref(), &ident, Some(range.clone()));
        let best = defs.first()?;

        let members = guard
            .files
            .get(&best.path)
            .and_then(|rec| {
                rec.extracted
                    .symbols
                    .iter()
                    .find(|s| s.selection_range == best.selection_range)
                    .map(|s| s.members.clone())
            })
            .unwrap_or_default();

        let ref_count: usize = guard
            .by_ref_files
            .get(&ident)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|p| guard.files.get(p))
                    .map(|rec| {
                        rec.extracted
                            .references
                            .iter()
                            .filter(|r| r.name == ident)
                            .count()
                    })
                    .sum()
            })
            .unwrap_or(0);
        Some(HoverInfo {
            symbol_name: best.symbol_name.clone(),
            kind: best.kind,
            signature: best.signature.clone(),
            doc_comment: best.doc_comment.clone(),
            defined_rel_path: best.rel_path.clone(),
            defined_line: best.start_line + 1,
            reference_count: ref_count,
            members,
            range,
        })
    }

    pub fn code_map(&self, path: &Path, offset: usize, text: &str) -> CodeMapSnapshot {
        let guard = self.inner.read().unwrap();
        let canonical = guard.resolve_path(path);
        let chain = guard
            .files
            .get(canonical.as_ref())
            .map(|rec| enclosing_chain_from_symbols(&rec.extracted.symbols, offset))
            .unwrap_or_default();
        let enclosing = chain
            .iter()
            .rev()
            .find(|s| s.kind != SymbolKind::Impl)
            .cloned()
            .or_else(|| chain.last().cloned());

        let cursor_ident = Self::identifier_at(text, offset).map(|(name, _)| name);
        let cursor_definitions = cursor_ident
            .as_deref()
            .map(|name| find_definitions_locked(&guard, canonical.as_ref(), name, None))
            .unwrap_or_default();

        let mut outgoing_symbols = Vec::new();
        let file_imports;
        let outgoing_names: Vec<String> = {
            if let Some(rec) = guard.files.get(canonical.as_ref()) {
                file_imports = rec.extracted.imports.clone();
                if let Some(enc) = &enclosing {
                    let mut seen = HashSet::new();
                    rec.extracted
                        .references
                        .iter()
                        .filter(|r| {
                            !r.is_definition
                                && r.byte_range.start >= enc.byte_range.start
                                && r.byte_range.end <= enc.byte_range.end
                                && r.name != enc.name
                        })
                        .filter(|r| seen.insert(r.name.clone()))
                        .map(|r| r.name.clone())
                        .collect()
                } else {
                    Vec::new()
                }
            } else {
                file_imports = Vec::new();
                Vec::new()
            }
        };

        for name in outgoing_names.into_iter().take(24) {
            if let Some(loc) =
                find_definitions_locked(&guard, canonical.as_ref(), &name, None)
                    .into_iter()
                    .next()
            {
                outgoing_symbols.push(loc);
            }
        }

        let target_for_callers = cursor_ident
            .as_deref()
            .or_else(|| enclosing.as_ref().map(|e| e.name.as_str()));
        let incoming_references = target_for_callers
            .map(|name| {
                find_references_locked(&guard, name, Some(canonical.as_ref()))
                    .into_iter()
                    .filter(|r| !r.is_definition)
                    .take(40)
                    .collect()
            })
            .unwrap_or_default();

        CodeMapSnapshot {
            enclosing,
            cursor_symbol: cursor_ident,
            cursor_definitions,
            outgoing_symbols,
            incoming_references,
            file_imports,
        }
    }

    pub fn search_symbols(&self, query: &str) -> Vec<SymbolHit> {
        self.search_symbols_query(query, false).unwrap_or_default()
    }

    /// Search global workspace symbols using either fuzzy matching or regex matching.
    ///
    /// Regex mode activates automatically when `use_regex` is `true`, or when the query
    /// uses `re:<pattern>` or `/<pattern>/` syntax.
    pub fn search_symbols_query(&self, query: &str, use_regex: bool) -> Result<Vec<SymbolHit>, String> {
        let trimmed = query.trim();
        let explicit_prefix_pat = if let Some(rest) = trimmed.strip_prefix("re:") {
            Some(rest.trim())
        } else if trimmed.starts_with('/') && trimmed.len() >= 2 {
            let inner = trimmed.strip_prefix('/').unwrap_or(trimmed);
            let inner = inner.strip_suffix('/').unwrap_or(inner);
            Some(inner)
        } else {
            None
        };

        let effective_regex = if use_regex {
            (!trimmed.is_empty()).then_some(explicit_prefix_pat.unwrap_or(trimmed))
        } else {
            explicit_prefix_pat.filter(|p| !p.is_empty())
        };

        let compiled_regex = if let Some(pat) = effective_regex {
            let has_upper = pat.chars().any(char::is_uppercase);
            let re = RegexBuilder::new(pat)
                .case_insensitive(!has_upper)
                .build()
                .map_err(|e| format!("Invalid regex: {e}"))?;
            Some(re)
        } else {
            None
        };

        let guard = self.inner.read().unwrap();
        let mut hits = Vec::new();
        for rec in guard.files.values() {
            for sym in &rec.extracted.symbols {
                if matches!(sym.kind, SymbolKind::Field | SymbolKind::Variant | SymbolKind::Impl)
                    && trimmed.is_empty()
                {
                    continue;
                }
                let score = if let Some(re) = &compiled_regex {
                    if let Some(m) = re.find(&sym.name) {
                        let base = if m.start() == 0 && m.end() == sym.name.len() {
                            350
                        } else if m.start() == 0 {
                            260
                        } else {
                            200
                        };
                        Some(base - (sym.name.len() as i64 - (m.end() - m.start()) as i64).max(0))
                    } else if re.is_match(&sym.signature) {
                        Some(130)
                    } else if re.is_match(&format!("{}::{}", rec.rel_path, sym.name)) {
                        Some(95)
                    } else {
                        None
                    }
                } else {
                    fuzzy_match(query, &sym.name).map(|m| m.score)
                };

                if let Some(score) = score {
                    hits.push(SymbolHit {
                        score,
                        location: NavLocation {
                            path: rec.path.clone(),
                            rel_path: rec.rel_path.clone(),
                            symbol_name: sym.name.clone(),
                            kind: sym.kind,
                            signature: sym.signature.clone(),
                            doc_comment: sym.doc_comment.clone(),
                            start_line: sym.start_line,
                            start_col: sym.start_col,
                            end_line: sym.end_line,
                            end_col: sym.end_col,
                            byte_range: sym.byte_range.clone(),
                            selection_range: sym.selection_range.clone(),
                            score,
                        },
                    });
                }
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.location.symbol_name.cmp(&b.location.symbol_name))
                .then_with(|| a.location.rel_path.cmp(&b.location.rel_path))
        });
        hits.truncate(120);
        Ok(hits)
    }

    pub fn search_files(&self, query: &str) -> Vec<FileSearchHit> {
        let guard = self.inner.read().unwrap();
        let mut hits = Vec::new();
        for rec in guard.files.values() {
            if let Some(m) = fuzzy_match(query, &rec.rel_path) {
                hits.push(FileSearchHit {
                    path: rec.path.clone(),
                    rel_path: rec.rel_path.clone(),
                    language: rec.extracted.language.clone(),
                    symbol_count: rec.extracted.symbols.len(),
                    score: m.score,
                });
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.rel_path.cmp(&b.rel_path))
        });
        hits.truncate(120);
        hits
    }

    pub fn all_rel_paths(&self) -> HashSet<String> {
        let guard = self.inner.read().unwrap();
        guard
            .files
            .values()
            .map(|rec| rec.rel_path.clone())
            .collect()
    }

    pub fn file_snapshot_by_rel_or_path(&self, raw_path: &str) -> Option<(PathBuf, String, String)> {
        let norm = raw_path
            .trim()
            .trim_start_matches("./")
            .replace('\\', "/");
        if norm.is_empty() {
            return None;
        }
        let guard = self.inner.read().unwrap();
        if let Some(rec) = guard
            .files
            .values()
            .find(|r| r.rel_path == norm || r.rel_path.ends_with(&format!("/{norm}")))
        {
            return Some((rec.path.clone(), rec.rel_path.clone(), rec.content.clone()));
        }
        let resolved = guard.resolve_path(Path::new(&norm));
        guard
            .files
            .get(resolved.as_ref())
            .map(|rec| (rec.path.clone(), rec.rel_path.clone(), rec.content.clone()))
    }

    pub fn search_text(&self, query: &str) -> Vec<TextSearchHit> {
        self.search_text_regex(query, false, None, 200)
            .unwrap_or_default()
    }

    pub fn search_text_regex(
        &self,
        query: &str,
        use_regex: bool,
        path_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TextSearchHit>, String> {
        let q = query.trim();
        if q.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let compiled = if use_regex {
            let has_upper = q.chars().any(char::is_uppercase);
            Some(
                RegexBuilder::new(q)
                    .case_insensitive(!has_upper)
                    .build()
                    .map_err(|e| format!("Invalid regex: {e}"))?,
            )
        } else {
            None
        };
        let q_lower = q.to_lowercase();
        let filter_norm = path_filter
            .map(|f| f.trim().to_lowercase())
            .filter(|f| !f.is_empty());
        let guard = self.inner.read().unwrap();
        let mut hits = Vec::new();

        for rec in guard.files.values() {
            if let Some(f) = &filter_norm {
                if !rec.rel_path.to_lowercase().contains(f) {
                    continue;
                }
            }
            let mut offset = 0usize;
            for (line_idx, raw_line) in rec.content.split_inclusive('\n').enumerate() {
                let trimmed = raw_line.trim_end_matches(['\r', '\n']);
                let found_col = if let Some(re) = &compiled {
                    re.find(trimmed).map(|m| m.start())
                } else {
                    trimmed.to_lowercase().find(&q_lower)
                };
                if let Some(col) = found_col {
                    hits.push(TextSearchHit {
                        path: rec.path.clone(),
                        rel_path: rec.rel_path.clone(),
                        line: line_idx as u32,
                        col: col as u32,
                        line_text: trimmed.trim().to_string(),
                        byte_offset: offset + col,
                    });
                    if hits.len() >= limit {
                        return Ok(hits);
                    }
                }
                offset += raw_line.len();
            }
        }
        Ok(hits)
    }

    pub fn all_diagnostics(&self) -> Vec<WorkspaceDiagnostic> {
        let guard = self.inner.read().unwrap();
        let mut out = Vec::new();
        for rec in guard.files.values() {
            for d in &rec.extracted.diagnostics {
                out.push(WorkspaceDiagnostic {
                    path: rec.path.clone(),
                    rel_path: rec.rel_path.clone(),
                    diagnostic: d.clone(),
                });
            }
        }
        out
    }

    pub fn file_diagnostics(&self, path: &Path) -> Vec<IndexDiagnostic> {
        let guard = self.inner.read().unwrap();
        let canonical = guard.resolve_path(path);
        guard
            .files
            .get(canonical.as_ref())
            .map(|r| r.extracted.diagnostics.clone())
            .unwrap_or_default()
    }

    pub fn completion_candidates(
        &self,
        path: &Path,
        prefix: &str,
        text: &str,
    ) -> Vec<CompletionCandidate> {
        let guard = self.inner.read().unwrap();
        let canonical = guard.resolve_path(path);
        let mut seen = HashSet::new();
        let mut out = Vec::new();

        // 1. Symbols from current file first
        if let Some(rec) = guard.files.get(canonical.as_ref()) {
            for sym in &rec.extracted.symbols {
                if (prefix.is_empty() || fuzzy_match(prefix, &sym.name).is_some())
                    && seen.insert(sym.name.clone())
                {
                    out.push(CompletionCandidate {
                        label: sym.name.clone(),
                        kind: sym.kind.to_completion_kind(),
                        detail: sym.signature.clone(),
                        documentation: sym.doc_comment.clone(),
                        priority: 200,
                    });
                }
            }
        }

        // 2. Identifiers found in the current buffer
        let mut tok = String::new();
        for ch in text.chars() {
            if is_ident_char(ch) {
                tok.push(ch);
            } else if !tok.is_empty() {
                if tok.len() >= 3
                    && tok != prefix
                    && (prefix.is_empty() || fuzzy_match(prefix, &tok).is_some())
                    && seen.insert(tok.clone())
                {
                    out.push(CompletionCandidate {
                        label: tok.clone(),
                        kind: lsp_types::CompletionItemKind::TEXT,
                        detail: "buffer identifier".to_string(),
                        documentation: None,
                        priority: 120,
                    });
                }
                tok.clear();
            }
        }

        // 3. Cross-file workspace symbols
        for rec in guard.files.values() {
            if rec.path == canonical {
                continue;
            }
            for sym in &rec.extracted.symbols {
                if matches!(sym.kind, SymbolKind::Impl | SymbolKind::Field | SymbolKind::Variant) {
                    continue;
                }
                if let Some(m) = fuzzy_match(prefix, &sym.name) {
                    if seen.insert(sym.name.clone()) {
                        out.push(CompletionCandidate {
                            label: sym.name.clone(),
                            kind: sym.kind.to_completion_kind(),
                            detail: format!("{} ({})", sym.signature, rec.rel_path),
                            documentation: sym.doc_comment.clone(),
                            priority: 80 + m.score,
                        });
                    }
                }
            }
        }

        out.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.label.cmp(&b.label)));
        out.truncate(40);
        out
    }

    /// Collects definitions of types/functions/structs referenced near the cursor (especially in
    /// other workspace files) so AI FIM completions receive cross-file repository context.
    pub fn related_context_for_fim(
        &self,
        path: &Path,
        offset: usize,
        text: &str,
        max_snippets: usize,
    ) -> Vec<FimContextSnippet> {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let window_start = floor_char_boundary(text, offset.saturating_sub(1800));
        let window_end = ceil_char_boundary(text, (offset + 600).min(text.len()));
        let local_slice = &text[window_start..window_end];

        let mut tokens = Vec::new();
        let mut seen_tok = HashSet::new();
        let mut cur = String::new();
        for ch in local_slice.chars() {
            if is_ident_char(ch) {
                cur.push(ch);
            } else {
                if cur.len() >= 3 && seen_tok.insert(cur.clone()) {
                    tokens.push(cur.clone());
                }
                cur.clear();
            }
        }
        if cur.len() >= 3 && seen_tok.insert(cur.clone()) {
            tokens.push(cur);
        }

        let guard = self.inner.read().unwrap();
        let mut snippets = Vec::new();
        let mut seen_syms = HashSet::new();

        // Include imported identifiers too
        if let Some(rec) = guard.files.get(&canonical) {
            for imp in &rec.extracted.imports {
                for name in &imp.names {
                    if seen_tok.insert(name.clone()) {
                        tokens.push(name.clone());
                    }
                }
            }
        }

        for tok in tokens.into_iter().rev() {
            let Some(postings) = guard.by_symbol_name.get(&tok) else {
                continue;
            };
            for (target_path, sym_idx) in postings {
                if target_path == &canonical {
                    continue;
                }
                let Some(rec) = guard.files.get(target_path) else {
                    continue;
                };
                let Some(sym) = rec.extracted.symbols.get(*sym_idx) else {
                    continue;
                };
                if matches!(sym.kind, SymbolKind::Impl | SymbolKind::Field | SymbolKind::Variant) {
                    continue;
                }
                let key = format!("{}:{}", rec.rel_path, sym.name);
                if seen_syms.insert(key) {
                    snippets.push(FimContextSnippet {
                        rel_path: rec.rel_path.clone(),
                        symbol_name: sym.name.clone(),
                        kind: sym.kind,
                        signature: sym.signature.clone(),
                        doc_comment: sym.doc_comment.clone(),
                        members: sym.members.clone(),
                    });
                    if snippets.len() >= max_snippets {
                        return snippets;
                    }
                }
            }
        }
        snippets
    }

    /// Look up members (struct fields / enum variants) of a type symbol anywhere in the workspace.
    pub fn members_for_symbol(&self, symbol_name: &str) -> Vec<String> {
        let guard = self.inner.read().unwrap();
        let Some(postings) = guard.by_symbol_name.get(symbol_name) else {
            return Vec::new();
        };
        for (path, sym_idx) in postings {
            if let Some(sym) = guard
                .files
                .get(path)
                .and_then(|r| r.extracted.symbols.get(*sym_idx))
            {
                if !sym.members.is_empty() {
                    return sym.members.clone();
                }
            }
        }
        Vec::new()
    }

    pub fn symbol_by_name(&self, name: &str) -> Option<NavLocation> {
        let guard = self.inner.read().unwrap();
        let postings = guard.by_symbol_name.get(name)?;
        let (path, idx) = postings.first()?;
        let rec = guard.files.get(path)?;
        let sym = rec.extracted.symbols.get(*idx)?;
        Some(NavLocation {
            path: rec.path.clone(),
            rel_path: rec.rel_path.clone(),
            symbol_name: sym.name.clone(),
            kind: sym.kind,
            signature: sym.signature.clone(),
            doc_comment: sym.doc_comment.clone(),
            start_line: sym.start_line,
            start_col: sym.start_col,
            end_line: sym.end_line,
            end_col: sym.end_col,
            byte_range: sym.byte_range.clone(),
            selection_range: sym.selection_range.clone(),
            score: 100,
        })
    }

    /// Infer lightweight AST + WorkspaceIndex type & parameter inlay hints for a file when
    /// an LSP server does not provide `textDocument/inlayHint`.
    pub fn inlay_hints_for_file(
        &self,
        path: &Path,
        text: &str,
        max_hints: usize,
    ) -> Vec<InlayHintItem> {
        let guard = self.inner.read().unwrap();
        let canonical = guard.resolve_path(path);
        let mut hints = Vec::new();

        for (line_idx, raw_line) in text.lines().enumerate() {
            if hints.len() >= max_hints {
                break;
            }
            let trimmed = raw_line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("//")
                || trimmed.starts_with('#')
                || trimmed.starts_with("/*")
            {
                continue;
            }

            // 1. Untyped local variable bindings from known function return types or constructors.
            if let Some((var_end_col, rhs)) = extract_untyped_binding_rhs(raw_line) {
                if let Some(inferred) = infer_type_from_rhs(&guard, canonical.as_ref(), rhs) {
                    hints.push(InlayHintItem {
                        line: line_idx as u32,
                        character: var_end_col as u32,
                        label: format!(": {inferred}"),
                        kind: InlayHintKindTag::Type,
                    });
                }
            }

            // 2. Concise parameter hints for function / method calls on the line.
            if let Some((callee_name, args_col, args)) = extract_call_site_args(raw_line) {
                if !args.is_empty() {
                    if let Some(param_names) =
                        lookup_param_names_for_callee(&guard, canonical.as_ref(), &callee_name)
                    {
                        let mut parts = Vec::new();
                        for (arg, pname) in args.iter().zip(param_names.iter()) {
                            let arg_clean = arg.trim();
                            if !arg_clean.is_empty()
                                && arg_clean != *pname
                                && !arg_clean.ends_with(&format!("{pname}:"))
                                && !arg_clean.contains('=')
                            {
                                parts.push(format!("{pname}: {arg_clean}"));
                            }
                        }
                        if !parts.is_empty() {
                            hints.push(InlayHintItem {
                                line: line_idx as u32,
                                character: args_col as u32,
                                label: format!("({})", parts.join(", ")),
                                kind: InlayHintKindTag::Parameter,
                            });
                        }
                    }
                }
            }
        }
        hints
    }
}

fn extract_untyped_binding_rhs(line: &str) -> Option<(usize, &str)> {
    let eq = line.find('=')?;
    // Ignore comparisons `==`, `!=`, `<=`, `>=`, `=>`.
    if matches!(line.as_bytes().get(eq.wrapping_sub(1)), Some(b'=' | b'!' | b'<' | b'>'))
        || matches!(line.as_bytes().get(eq + 1), Some(b'=' | b'>'))
    {
        return None;
    }
    let lhs = line[..eq].trim_end();
    if lhs.contains(':') {
        return None;
    }
    let trimmed_lhs = lhs.trim_start();
    let is_binding = trimmed_lhs.starts_with("let ")
        || trimmed_lhs.starts_with("const ")
        || trimmed_lhs.starts_with("var ")
        || (!trimmed_lhs.contains(' ') && !trimmed_lhs.is_empty());
    if !is_binding {
        return None;
    }
    let ident_part = trimmed_lhs
        .trim_start_matches("let ")
        .trim_start_matches("mut ")
        .trim_start_matches("const ")
        .trim_start_matches("var ")
        .trim();
    if ident_part.is_empty() || ident_part.starts_with('_') || !ident_part.chars().all(is_ident_char)
    {
        return None;
    }
    let rhs = line[eq + 1..].trim().trim_end_matches(';').trim();
    Some((lhs.len(), rhs))
}

fn infer_type_from_rhs(guard: &IndexData, origin: &Path, rhs: &str) -> Option<String> {
    let head = rhs.split('(').next()?.trim().trim_start_matches("new ").trim();
    let parts: Vec<&str> = head.split("::").chain(head.split('.')).collect();
    if let Some((type_name, _)) = head.split_once("::") {
        let defs = find_definitions_locked(guard, origin, type_name.trim(), None);
        if let Some(d) = defs.first() {
            if matches!(d.kind, SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Class | SymbolKind::Interface) {
                return Some(d.symbol_name.clone());
            }
        }
    }
    let callee = parts.last().copied().unwrap_or(head).trim();
    if callee.is_empty() {
        return None;
    }
    let defs = find_definitions_locked(guard, origin, callee, None);
    for d in defs {
        if matches!(d.kind, SymbolKind::Struct | SymbolKind::Class) {
            return Some(d.symbol_name);
        }
        if matches!(d.kind, SymbolKind::Function | SymbolKind::Method) {
            if let Some((_, ret)) = d.signature.split_once("->") {
                let r = ret.trim().trim_end_matches('{').trim();
                if !r.is_empty() && r != "Self" && r != "None" && r != "()" {
                    return Some(r.to_string());
                }
            }
        }
    }
    None
}

fn extract_call_site_args(line: &str) -> Option<(String, usize, Vec<&str>)> {
    let open = line.find('(')?;
    let prefix = line[..open].trim_end();
    if prefix.ends_with("fn")
        || prefix.starts_with("fn ")
        || prefix.starts_with("pub fn ")
        || prefix.starts_with("def ")
        || prefix.starts_with("if ")
        || prefix.starts_with("while ")
        || prefix.starts_with("for ")
        || prefix.starts_with("match ")
    {
        return None;
    }
    let callee_start = prefix
        .rfind(|c: char| !is_ident_char(c))
        .map(|i| i + 1)
        .unwrap_or(0);
    let callee = prefix[callee_start..].trim();
    if callee.is_empty() {
        return None;
    }
    let after = &line[open + 1..];
    let close = after.rfind(')')?;
    let arg_body = after[..close].trim();
    if arg_body.is_empty() {
        return None;
    }
    let args: Vec<&str> = arg_body.split(',').collect();
    if args.is_empty() || args.len() > 6 {
        return None;
    }
    Some((callee.to_string(), open + 1, args))
}

fn lookup_param_names_for_callee(
    guard: &IndexData,
    origin: &Path,
    callee: &str,
) -> Option<Vec<String>> {
    let defs = find_definitions_locked(guard, origin, callee, None);
    let d = defs
        .into_iter()
        .find(|d| matches!(d.kind, SymbolKind::Function | SymbolKind::Method))?;
    let open = d.signature.find('(')?;
    let close = d.signature[open + 1..].find(')')? + open + 1;
    let params_str = &d.signature[open + 1..close];
    let mut names = Vec::new();
    for raw_p in params_str.split(',') {
        let p = raw_p.trim();
        if p.is_empty() || p == "self" || p == "&self" || p == "&mut self" || p == "cls" {
            continue;
        }
        let name = p
            .split(':')
            .next()
            .unwrap_or(p)
            .split('=')
            .next()
            .unwrap_or(p)
            .trim()
            .trim_start_matches("mut ");
        if !name.is_empty() && name.chars().all(is_ident_char) {
            names.push(name.to_string());
        }
    }
    (!names.is_empty()).then_some(names)
}

fn find_definitions_locked(
    guard: &IndexData,
    canonical_origin: &Path,
    ident: &str,
    origin_range: Option<Range<usize>>,
) -> Vec<NavLocation> {
    let Some(entries) = guard.by_symbol_name.get(ident) else {
        return Vec::new();
    };

    let origin_imports: Vec<&str> = guard
        .files
        .get(canonical_origin)
        .map(|rec| rec.extracted.imports.iter().map(|i| i.raw.as_str()).collect())
        .unwrap_or_default();

    let mut locations: Vec<NavLocation> = entries
        .iter()
        .filter_map(|(target_path, sym_idx)| {
            let rec = guard.files.get(target_path)?;
            let sym = rec.extracted.symbols.get(*sym_idx)?;
            if sym.kind == SymbolKind::Impl {
                return None;
            }
            let is_self_def = target_path == canonical_origin
                && origin_range
                    .as_ref()
                    .is_some_and(|r| r == &sym.selection_range);

            let mut score: i64 = match sym.kind {
                SymbolKind::Function | SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Trait
                | SymbolKind::Interface | SymbolKind::Class => 50,
                SymbolKind::Method | SymbolKind::TypeAlias | SymbolKind::Constant => 40,
                SymbolKind::Impl => 10,
                _ => 25,
            };
            if target_path == canonical_origin {
                score += 30;
            }
            if origin_imports.iter().any(|imp| {
                imp.contains(ident)
                    || rec
                        .path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .is_some_and(|stem| imp.contains(stem))
            }) {
                score += 25;
            }
            if is_self_def {
                score -= 35;
            }

            Some(NavLocation {
                path: rec.path.clone(),
                rel_path: rec.rel_path.clone(),
                symbol_name: sym.name.clone(),
                kind: sym.kind,
                signature: sym.signature.clone(),
                doc_comment: sym.doc_comment.clone(),
                start_line: sym.start_line,
                start_col: sym.start_col,
                end_line: sym.end_line,
                end_col: sym.end_col,
                byte_range: sym.byte_range.clone(),
                selection_range: sym.selection_range.clone(),
                score,
            })
        })
        .collect();

    locations.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.rel_path.cmp(&b.rel_path))
            .then_with(|| a.start_line.cmp(&b.start_line))
    });
    locations
}

fn find_references_locked(
    guard: &IndexData,
    symbol_name: &str,
    canonical_pref: Option<&Path>,
) -> Vec<ReferenceHit> {
    let Some(paths) = guard.by_ref_files.get(symbol_name) else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    for path in paths {
        let Some(rec) = guard.files.get(path) else {
            continue;
        };
        for r in &rec.extracted.references {
            if r.name == symbol_name {
                let enclosing_symbol = r
                    .enclosing_symbol_idx
                    .and_then(|idx| rec.extracted.symbols.get(idx))
                    .map(|s| s.name.clone());
                hits.push(ReferenceHit {
                    path: rec.path.clone(),
                    rel_path: rec.rel_path.clone(),
                    name: r.name.clone(),
                    line: r.line,
                    col: r.col,
                    byte_range: r.byte_range.clone(),
                    line_text: r.line_text.clone(),
                    enclosing_symbol,
                    is_definition: r.is_definition,
                    is_call: r.is_call,
                });
            }
        }
    }

    hits.sort_by(|a, b| {
        let a_pref = canonical_pref.is_some_and(|p| p == a.path.as_path());
        let b_pref = canonical_pref.is_some_and(|p| p == b.path.as_path());
        b.is_definition
            .cmp(&a.is_definition)
            .then_with(|| b_pref.cmp(&a_pref))
            .then_with(|| a.rel_path.cmp(&b.rel_path))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.col.cmp(&b.col))
    });
    hits
}

fn enclosing_chain_from_symbols(symbols: &[SymbolEntry], offset: usize) -> Vec<OutlineNode> {
    let mut chain = Vec::new();
    for (idx, sym) in symbols.iter().enumerate() {
        if sym.byte_range.start <= offset && offset <= sym.byte_range.end {
            let mut depth = 0usize;
            let mut cur = sym.parent_idx;
            while let Some(p) = cur {
                depth += 1;
                cur = symbols.get(p).and_then(|s| s.parent_idx);
            }
            chain.push(OutlineNode {
                idx,
                name: sym.name.clone(),
                kind: sym.kind,
                signature: sym.signature.clone(),
                doc_comment: sym.doc_comment.clone(),
                start_line: sym.start_line,
                start_col: sym.start_col,
                end_line: sym.end_line,
                end_col: sym.end_col,
                byte_range: sym.byte_range.clone(),
                selection_range: sym.selection_range.clone(),
                depth,
                members: sym.members.clone(),
            });
        }
    }
    chain.sort_by_key(|s| s.byte_range.start);
    chain
}

fn build_outline(symbols: &[SymbolEntry]) -> Vec<OutlineNode> {
    let mut depths = vec![0usize; symbols.len()];
    for i in 0..symbols.len() {
        let mut d = 0;
        let mut cur = symbols[i].parent_idx;
        while let Some(p) = cur {
            d += 1;
            cur = symbols.get(p).and_then(|s| s.parent_idx);
        }
        depths[i] = d;
    }

    symbols
        .iter()
        .enumerate()
        .map(|(idx, sym)| OutlineNode {
            idx,
            name: sym.name.clone(),
            kind: sym.kind,
            signature: sym.signature.clone(),
            doc_comment: sym.doc_comment.clone(),
            start_line: sym.start_line,
            start_col: sym.start_col,
            end_line: sym.end_line,
            end_col: sym.end_col,
            byte_range: sym.byte_range.clone(),
            selection_range: sym.selection_range.clone(),
            depth: depths[idx],
            members: sym.members.clone(),
        })
        .collect()
}

fn is_indexable_extension(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if matches!(name, "Makefile" | "Cargo.toml" | "package.json" | "README.md") {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "rs" | "ts"
            | "tsx"
            | "mts"
            | "cts"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "py"
            | "pyi"
            | "go"
            | "c"
            | "h"
            | "cpp"
            | "cc"
            | "cxx"
            | "hpp"
            | "hh"
            | "json"
            | "toml"
            | "md"
            | "markdown"
            | "yaml"
            | "yml"
            | "sh"
            | "bash"
            | "zsh"
            | "css"
            | "scss"
            | "html"
            | "txt"
    )
}

fn make_rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

pub fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

pub fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

pub fn ceil_char_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn indexes_workspace_cross_file_definitions_references_and_fim_context() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("models.rs");
        let b = dir.path().join("service.rs");
        fs::write(
            &a,
            r#"
/// Application account model.
pub struct Account {
    pub id: u64,
    pub balance: i64,
}

pub enum Status {
    Active,
    Suspended,
}
"#,
        )
        .unwrap();
        fs::write(
            &b,
            r#"
use crate::models::Account;

pub fn credit(acc: &mut Account, amount: i64) {
    acc.balance += amount;
}
"#,
        )
        .unwrap();

        let index = WorkspaceIndex::new();
        let (files, symbols) = index.index_workspace(dir.path());
        assert_eq!(files, 2);
        assert!(symbols >= 4);

        let b_text = fs::read_to_string(&b).unwrap();
        let account_offset = b_text.find("Account,").unwrap() + 2;
        let defs = index.find_definitions(&b, account_offset, &b_text);
        assert!(!defs.is_empty());
        assert_eq!(defs[0].symbol_name, "Account");
        assert!(defs[0].rel_path.ends_with("models.rs"));

        let refs = index.find_references("Account", Some(&b));
        assert!(refs.len() >= 2);

        let hover = index.hover_info(&b, account_offset, &b_text).unwrap();
        assert_eq!(hover.symbol_name, "Account");
        assert_eq!(
            hover.doc_comment.as_deref(),
            Some("Application account model.")
        );
        assert!(hover.members.contains(&"balance".to_string()));

        let fim_ctx = index.related_context_for_fim(&b, account_offset, &b_text, 4);
        assert!(fim_ctx.iter().any(|s| s.symbol_name == "Account"));

        // Global regex-based symbol search
        let re_hits = index.search_symbols_query("^cre.*it$", true).unwrap();
        assert_eq!(re_hits.len(), 1);
        assert_eq!(re_hits[0].location.symbol_name, "credit");

        let inline_re_hits = index.search_symbols("re:^Acc.*$");
        assert!(inline_re_hits.iter().any(|h| h.location.symbol_name == "Account"));
        assert!(index.search_symbols_query("(", true).is_err());

        // AST + WorkspaceIndex inlay hint inference
        let c = dir.path().join("use_acc.rs");
        let c_text = "fn run() {\n    let acct = Account::new(1);\n    credit(acct, 50);\n}\n";
        fs::write(&c, c_text).unwrap();
        index.reindex_file(&c, c_text);
        let hints = index.inlay_hints_for_file(&c, c_text, 16);
        assert!(hints.iter().any(|h| h.label.contains("Account")), "hints={hints:?}");
        assert!(hints.iter().any(|h| h.label.contains("acc:") && h.label.contains("amount:")), "hints={hints:?}");
    }
}
