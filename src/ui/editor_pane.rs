use crate::{
    config::{FimConfig, VimConfig},
    fim::{FimEngine, FimStatus},
    index::{InlayHintKindTag, WorkspaceIndex, extractor::detect_language, is_ident_char},
    lsp::{LspManager, location_link_from_nav},
    ui::ide::KestrelIde,
    vim::{
        ScrollAlign, VimMode, VimState,
        motions::{clamp_char_boundary, clamp_normal_cursor, next_char_end},
    },
};
use anyhow::Result;
use gpui_kit::{
    App, AppContext as _, ContentMask, Context, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Subscription, Task, TextAlign, TextRun, WeakEntity, Window, canvas,
    component::{
        ActiveTheme as _, ColorName, Icon,
        dock::{BasePanel, Panel, PanelControl, PanelEvent},
        h_flex,
        highlighter::{Diagnostic, DiagnosticSeverity},
        input::{
            CodeActionProvider, CompletionProvider, DefinitionProvider, Editor, EditorState,
            HoverProvider, InputEvent, RangeDecoration, RangeDecorationCollection,
            RangeDecorationStyle, Rope, RopeExt as _,
        },
    },
    div, fill, font, hsla, point, prelude::FluentBuilder as _, px,
};
use lsp_types::{
    CodeAction, CompletionContext, CompletionItem, CompletionResponse, CompletionTextEdit, Hover,
    HoverContents, InlayHintLabel, InlineCompletionContext, InlineCompletionItem,
    InlineCompletionResponse, LocationLink, MarkupContent, MarkupKind, Position, Range, TextEdit,
};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    ops::Range as ByteRange,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, RwLock},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedInlayHint {
    pub line: u32,
    pub character: u32,
    pub label: String,
    pub is_type: bool,
}

#[derive(Debug, Clone)]
pub struct RenderedInlineDoc {
    pub line: u32,
    pub ident_range: ByteRange<usize>,
    pub summary: String,
    pub hover: Hover,
}

pub struct CodeTabPanel {
    pub path: PathBuf,
    pub rel_path: String,
    pub language: String,
    pub saved_text: String,
    pub dirty: bool,
    pub editor: Entity<EditorState>,
    pub vim: VimState,
    vim_config: Arc<RwLock<VimConfig>>,
    vim_decorations: RangeDecorationCollection,
    vim_undo_stack: Vec<(String, usize)>,
    vim_redo_stack: Vec<(String, usize)>,
    vim_insert_entry: Option<(String, usize)>,
    pub last_insert_offset: Option<usize>,
    cached_text: Option<(usize, String, Arc<String>)>,
    last_vim_dec_key: Option<(Option<ByteRange<usize>>, VimMode, bool, u64, String)>,
    cached_search_hits: Option<(u64, String, Vec<ByteRange<usize>>)>,
    pub scroll_padding: usize,
    pub inlay_hints_enabled: bool,
    pub inline_hover_docs_enabled: bool,
    pending_surrounding_lines_sync: bool,
    inlay_hints: Vec<RenderedInlayHint>,
    inlay_lines: Vec<(u32, String)>,
    inlay_seq: u64,
    inlay_task: Option<Task<()>>,
    pub inline_hover_doc: Option<RenderedInlineDoc>,
    inline_doc_seq: u64,
    inline_doc_task: Option<Task<()>>,
    last_doc_ident_key: Option<(u64, ByteRange<usize>)>,
    edit_seq: u64,
    sync_pending: bool,
    sync_task: Option<Task<()>>,
    ide: WeakEntity<KestrelIde>,
    index: WorkspaceIndex,
    fim: FimEngine,
    lsp: LspManager,
    last_observed_cursor: usize,
    last_observed_selection: ByteRange<usize>,
    _subscriptions: Vec<Subscription>,
}

impl CodeTabPanel {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        path: PathBuf,
        rel_path: String,
        initial_text: String,
        index: WorkspaceIndex,
        fim: FimEngine,
        fim_config: Arc<RwLock<FimConfig>>,
        vim_config: Arc<RwLock<VimConfig>>,
        scroll_padding: usize,
        inlay_hints_enabled: bool,
        inline_hover_docs_enabled: bool,
        lsp: LspManager,
        ide: WeakEntity<KestrelIde>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let language = detect_language(&path).to_string();

        lsp.did_open(&path, &language, &initial_text);

        let completion_provider = Rc::new(KestrelCompletionProvider {
            path: path.clone(),
            language: language.clone(),
            index: index.clone(),
            fim: fim.clone(),
            fim_config,
            lsp: lsp.clone(),
            ide: ide.clone(),
        });
        let definition_provider = Rc::new(KestrelDefinitionProvider {
            path: path.clone(),
            language: language.clone(),
            index: index.clone(),
            lsp: lsp.clone(),
        });
        let hover_provider = Rc::new(KestrelHoverProvider {
            path: path.clone(),
            language: language.clone(),
            index: index.clone(),
            lsp: lsp.clone(),
        });
        let code_action_provider = Rc::new(KestrelCodeActionProvider {
            path: path.clone(),
            language: language.clone(),
            lsp: lsp.clone(),
        });
        let show_doc_ide = ide.clone();
        let show_document: gpui_kit::component::input::ShowDocumentHandler =
            Rc::new(move |params, window, cx| {
                let mut handled = false;
                _ = show_doc_ide.update(cx, |ide, cx| {
                    handled = ide.handle_show_document(params, window, cx);
                });
                handled
            });

        let vim_cfg = vim_config.read().map(|c| c.clone()).unwrap_or_default();
        let vim = VimState::new(!vim_cfg.enabled || vim_cfg.start_in_insert);
        let initial_readonly = vim_cfg.enabled && vim.mode.is_readonly();

        let scroll_padding = scroll_padding.clamp(0, 20);
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language(language.clone())
                .searchable(true)
                .folding(true)
                .line_number(true)
                .auto_close(true)
                .smart_indent(true)
                .cursor_surrounding_lines(Some(scroll_padding))
                .default_value(initial_text.clone());
            state.set_readonly(initial_readonly, cx);
            {
                let lsp_cfg = state.lsp_mut();
                lsp_cfg.completion_provider = Some(completion_provider);
                lsp_cfg.definition_provider = Some(definition_provider);
                lsp_cfg.hover_provider = Some(hover_provider);
                lsp_cfg.code_action_providers = vec![code_action_provider];
                lsp_cfg.show_document = Some(show_document);
            }
            sync_diagnostics_into_state(&mut state, &path, &index, &lsp);
            state
        });
        let vim_decorations =
            editor.update(cx, |state, cx| state.create_range_decorations_collection(vec![], cx));

        let mut subscriptions = Vec::new();
        let change_sub = cx.subscribe_in(&editor, window, |this, _, ev: &InputEvent, window, cx| {
            match ev {
                InputEvent::Change => {
                    this.edit_seq = this.edit_seq.wrapping_add(1);
                    this.cached_text = None;
                    let dirty_now = !rope_equals_str(this.editor.read(cx).text(), &this.saved_text);
                    if dirty_now != this.dirty {
                        this.dirty = dirty_now;
                        cx.notify();
                    }
                    this.schedule_background_sync(cx);
                }
                InputEvent::Focus => {
                    let ide = this.ide.clone();
                    let path = this.path.clone();
                    window.defer(cx, move |_, cx| {
                        _ = ide.update(cx, |ide, cx| {
                            ide.on_tab_focused(&path, cx);
                        });
                    });
                }
                _ => {}
            }
        });
        subscriptions.push(change_sub);

        let observe_sub = cx.observe(&editor, |this, editor, cx| {
            let (cursor, sel) = {
                let ed = editor.read(cx);
                (ed.cursor(), ed.selected_range())
            };
            if cursor != this.last_observed_cursor || sel != this.last_observed_selection {
                let cursor_moved = cursor != this.last_observed_cursor;
                this.last_observed_cursor = cursor;
                this.last_observed_selection = sel;
                this.sync_vim_decorations(cx);
                if cursor_moved && this.inline_hover_docs_enabled {
                    this.schedule_inline_doc_refresh(cx);
                }
                let ide = this.ide.clone();
                let path = this.path.clone();
                cx.defer(move |cx| {
                    _ = ide.update(cx, |ide, cx| {
                        ide.on_cursor_moved(&path, cursor, cx);
                    });
                });
            }
        });
        subscriptions.push(observe_sub);

        let mut panel = Self {
            path,
            rel_path,
            language,
            saved_text: initial_text,
            dirty: false,
            editor,
            vim,
            vim_config,
            vim_decorations,
            vim_undo_stack: Vec::new(),
            vim_redo_stack: Vec::new(),
            vim_insert_entry: None,
            last_insert_offset: None,
            cached_text: None,
            last_vim_dec_key: None,
            cached_search_hits: None,
            scroll_padding,
            inlay_hints_enabled,
            inline_hover_docs_enabled,
            pending_surrounding_lines_sync: false,
            inlay_hints: Vec::new(),
            inlay_lines: Vec::new(),
            inlay_seq: 0,
            inlay_task: None,
            inline_hover_doc: None,
            inline_doc_seq: 0,
            inline_doc_task: None,
            last_doc_ident_key: None,
            edit_seq: 0,
            sync_pending: false,
            sync_task: None,
            ide,
            index,
            fim,
            lsp,
            last_observed_cursor: 0,
            last_observed_selection: 0..0,
            _subscriptions: subscriptions,
        };
        panel.sync_vim_decorations(cx);
        panel.schedule_inlay_refresh(10, cx);
        panel
    }

    /// Return a cached `Arc<String>` snapshot of the buffer, rebuilding only when buffer contents changed.
    pub fn text_snapshot(&mut self, cx: &App) -> Arc<String> {
        let rope = self.editor.read(cx).text();
        let len = rope.len();
        let first_chunk = rope.chunks().next().unwrap_or("");
        if let Some((cached_len, cached_head, cached_arc)) = &self.cached_text {
            if *cached_len == len && cached_head == first_chunk {
                return Arc::clone(cached_arc);
            }
        }
        let s = Arc::new(rope.to_string());
        self.cached_text = Some((len, first_chunk.to_string(), Arc::clone(&s)));
        s
    }

    fn schedule_background_sync(&mut self, cx: &mut Context<Self>) {
        self.sync_pending = true;
        let expected_seq = self.edit_seq;
        let path = self.path.clone();
        let language = self.language.clone();
        let lsp = self.lsp.clone();
        let ide = self.ide.clone();

        self.sync_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(85)).await;

            let text_opt = this
                .update(cx, |tab, cx| {
                    if tab.edit_seq != expected_seq {
                        return None;
                    }
                    Some(tab.text_snapshot(cx).as_ref().clone())
                })
                .ok()
                .flatten();

            let Some(text) = text_opt else {
                return;
            };

            let bg = cx.background_executor().clone();
            let path_bg = path.clone();
            let lang_bg = language.clone();
            let text_bg = text.clone();
            let lsp_bg = lsp.clone();
            let (canonical, extracted) = bg
                .spawn(async move {
                    lsp_bg.did_change(&path_bg, &lang_bg, &text_bg);
                    WorkspaceIndex::prepare_reindex_file(&path_bg, &text_bg)
                })
                .await;

            _ = this.update(cx, |tab, cx| {
                if tab.edit_seq != expected_seq {
                    return;
                }
                tab.sync_pending = false;
                tab.index.commit_reindex_file(canonical, extracted, text);
                tab.editor.update(cx, |state, _| {
                    sync_diagnostics_into_state(state, &tab.path, &tab.index, &tab.lsp);
                });
                tab.schedule_inlay_refresh(40, cx);
                cx.notify();
            });

            _ = ide.update(cx, |ide, cx| {
                ide.on_buffer_changed(&path, cx);
            });
        }));
    }

    pub fn flush_pending_sync(&mut self, cx: &mut Context<Self>) {
        if !self.sync_pending {
            return;
        }
        self.sync_pending = false;
        self.sync_task = None;
        let text = self.text_snapshot(cx);
        self.index.reindex_file(&self.path, &text);
        self.lsp.did_change(&self.path, &self.language, &text);
        self.editor.update(cx, |state, _| {
            sync_diagnostics_into_state(state, &self.path, &self.index, &self.lsp);
        });
    }

    pub fn is_vim_enabled(&self) -> bool {
        self.vim_config.read().map(|c| c.enabled).unwrap_or(true)
    }

    pub fn is_vim_readonly(&self) -> bool {
        self.is_vim_enabled() && self.vim.mode.is_readonly()
    }

    pub fn sync_vim_decorations(&mut self, cx: &mut App) {
        let vim_enabled = self.is_vim_enabled();
        let highlight_search = vim_enabled && self.vim.highlight_search && !self.vim.search_query.is_empty();
        let (cursor, cursor_range) = {
            let state_ref = self.editor.read(cx);
            let rope = state_ref.text();
            let len = rope.len();
            let cursor = state_ref.cursor().min(len);
            let block_range = if vim_enabled
                && !matches!(
                    self.vim.mode,
                    VimMode::Insert | VimMode::Visual | VimMode::VisualLine
                )
                && cursor < len
            {
                rope.slice(cursor..).chars().next().and_then(|ch| {
                    if ch != '\n' && ch != '\r' {
                        Some(cursor..cursor + ch.len_utf8())
                    } else {
                        None
                    }
                })
            } else {
                None
            };
            (cursor, block_range)
        };
        let _ = cursor;

        let search_query = if highlight_search {
            self.vim.search_query.clone()
        } else {
            String::new()
        };
        let dec_key = (
            cursor_range.clone(),
            self.vim.mode,
            vim_enabled,
            self.edit_seq,
            search_query.clone(),
        );
        if self.last_vim_dec_key.as_ref() == Some(&dec_key) {
            return;
        }
        self.last_vim_dec_key = Some(dec_key);

        let mut decs = Vec::new();
        if highlight_search {
            let hit_ranges = match &self.cached_search_hits {
                Some((seq, q, hits)) if *seq == self.edit_seq && *q == search_query => hits.clone(),
                _ => {
                    let text = self.text_snapshot(cx);
                    let hits: Vec<ByteRange<usize>> = self
                        .vim
                        .search_matches(&text)
                        .into_iter()
                        .take(500)
                        .collect();
                    self.cached_search_hits = Some((self.edit_seq, search_query, hits.clone()));
                    hits
                }
            };
            for r in hit_ranges {
                decs.push(
                    RangeDecoration::new(r)
                        .with_style(RangeDecorationStyle::Fill)
                        .with_color(hsla(0.12, 0.85, 0.55, 0.28)),
                );
            }
        } else {
            self.cached_search_hits = None;
        }

        if let Some(range) = cursor_range {
            let color = if self.vim.mode == VimMode::Replace {
                hsla(0.05, 0.90, 0.60, 0.95)
            } else {
                hsla(0.55, 0.85, 0.60, 0.95)
            };
            decs.push(
                RangeDecoration::new(range)
                    .with_style(RangeDecorationStyle::Frame)
                    .with_color(color),
            );
        }

        self.vim_decorations.set(decs, cx);
    }

    pub fn sync_vim_readonly(&self, cx: &mut App) {
        let readonly = self.is_vim_readonly();
        self.editor.update(cx, |state, cx| {
            state.set_readonly(readonly, cx);
        });
    }

    pub fn apply_vim_config_change(&mut self, cx: &mut Context<Self>) {
        let cfg = self.vim_config.read().map(|c| c.clone()).unwrap_or_default();
        if !cfg.enabled {
            self.vim.clear_pending();
            self.vim.clear_visual();
            self.vim.command_buffer.clear();
            self.vim.highlight_search = false;
            self.vim.mode = VimMode::Insert;
        } else if self.vim.mode == VimMode::Insert && !cfg.start_in_insert {
            let text = self.text_snapshot(cx);
            let cursor = self.editor.read(cx).cursor();
            let _ = self.vim.enter_normal_mode(&text, cursor, false);
            let clamped = clamp_normal_cursor(&text, cursor);
            self.editor.update(cx, |state, cx| {
                state.set_selected_range(clamped..clamped, cx);
            });
        }
        self.sync_vim_readonly(cx);
        self.sync_vim_decorations(cx);
        cx.notify();
    }

    pub fn set_vim_cursor(&mut self, pos: usize, cx: &mut Context<Self>) {
        let padding = self.scroll_padding;
        self.editor.update(cx, |state, cx| {
            let clamped = pos.min(state.text().len());
            state.set_selected_range(clamped..clamped, cx);
            ensure_scroll_padding_for(state, clamped, padding, cx);
        });
        self.sync_vim_decorations(cx);
        cx.notify();
    }

    pub fn set_vim_visual_selection(
        &mut self,
        anchor: usize,
        head: usize,
        cx: &mut Context<Self>,
    ) {
        let padding = self.scroll_padding;
        let text = self.text_snapshot(cx);
        let reversed = head < anchor;
        let range = self
            .vim
            .visual_range(&text, head)
            .map(|(r, _)| r)
            .unwrap_or_else(|| {
                let lo = anchor.min(head);
                let hi = next_char_end(&text, anchor.max(head));
                lo..hi
            });
        self.editor.update(cx, |state, cx| {
            state.set_selected_range_with_direction(range, reversed, cx);
            ensure_scroll_padding_for(state, head.min(state.text().len()), padding, cx);
        });
        self.sync_vim_decorations(cx);
        cx.notify();
    }

    pub fn record_vim_mode_transition(
        &mut self,
        prev_mode: VimMode,
        new_mode: VimMode,
        pre_text: &str,
        pre_cursor: usize,
        cx: &mut Context<Self>,
    ) {
        let was_typing = matches!(prev_mode, VimMode::Insert | VimMode::Replace);
        let now_typing = matches!(new_mode, VimMode::Insert | VimMode::Replace);
        if !was_typing && now_typing && self.vim_insert_entry.is_none() {
            self.vim_insert_entry = Some((pre_text.to_string(), pre_cursor));
        } else if was_typing && !now_typing {
            self.last_insert_offset = Some(pre_cursor);
            if let Some((entry_text, entry_cursor)) = self.vim_insert_entry.take() {
                let current_text = self.text_snapshot(cx);
                if current_text.as_str() != entry_text {
                    self.push_vim_undo_entry(entry_text, entry_cursor);
                }
            }
        }
    }

    fn push_vim_undo_entry(&mut self, text: String, cursor: usize) {
        self.vim_undo_stack.push((text, cursor));
        if self.vim_undo_stack.len() > 200 {
            self.vim_undo_stack.remove(0);
        }
        self.vim_redo_stack.clear();
    }

    pub fn apply_vim_replace(
        &mut self,
        range: ByteRange<usize>,
        replacement: &str,
        new_cursor: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vim_insert_entry.is_none() {
            let pre_text = self.text_snapshot(cx).as_ref().clone();
            let cur = self.editor.read(cx).cursor();
            self.push_vim_undo_entry(pre_text, cur);
        }
        self.cached_text = None;
        let padding = self.scroll_padding;
        self.editor.update(cx, |state, cx| {
            let len = state.text().len();
            let clamped_range = range.start.min(len)..range.end.min(len);
            state.set_selected_range(clamped_range, cx);
            state.replace(replacement.to_string(), window, cx);
            let next_cur = new_cursor.min(state.text().len());
            state.set_selected_range(next_cur..next_cur, cx);
            ensure_scroll_padding_for(state, next_cur, padding, cx);
        });
        self.sync_vim_readonly(cx);
        self.sync_vim_decorations(cx);
        cx.notify();
    }

    pub fn apply_vim_undo_redo(&mut self, redo: bool, window: &mut Window, cx: &mut Context<Self>) {
        let cur_text = self.text_snapshot(cx).as_ref().clone();
        let cur_cursor = self.editor.read(cx).cursor();
        let entry = if redo {
            self.vim_redo_stack.pop()
        } else {
            self.vim_undo_stack.pop()
        };
        let Some((target_text, target_cursor)) = entry else {
            return;
        };
        if redo {
            self.vim_undo_stack.push((cur_text.clone(), cur_cursor));
        } else {
            self.vim_redo_stack.push((cur_text.clone(), cur_cursor));
        }
        self.cached_text = None;
        self.replace_minimal_diff(&cur_text, &target_text, target_cursor, window, cx);
        self.sync_vim_readonly(cx);
        self.sync_vim_decorations(cx);
        cx.notify();
    }

    fn replace_minimal_diff(
        &mut self,
        current: &str,
        target: &str,
        target_cursor: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut prefix = 0usize;
        let max_prefix = current.len().min(target.len());
        let cb = current.as_bytes();
        let tb = target.as_bytes();
        while prefix < max_prefix && cb[prefix] == tb[prefix] {
            prefix += 1;
        }
        prefix = clamp_char_boundary(current, prefix).min(clamp_char_boundary(target, prefix));

        let mut cur_end = current.len();
        let mut tgt_end = target.len();
        while cur_end > prefix && tgt_end > prefix && cb[cur_end - 1] == tb[tgt_end - 1] {
            cur_end -= 1;
            tgt_end -= 1;
        }
        while (cur_end < current.len() && !current.is_char_boundary(cur_end))
            || (tgt_end < target.len() && !target.is_char_boundary(tgt_end))
        {
            cur_end = (cur_end + 1).min(current.len());
            tgt_end = (tgt_end + 1).min(target.len());
        }

        let stay_ro = self.is_vim_readonly();
        let slice = &target[prefix..tgt_end];
        self.editor.update(cx, |state, cx| {
            state.set_selected_range(prefix..cur_end, cx);
            state.replace(slice.to_string(), window, cx);
            let final_cur = if stay_ro {
                clamp_normal_cursor(target, target_cursor)
            } else {
                target_cursor.min(state.text().len())
            };
            state.set_selected_range(final_cur..final_cur, cx);
        });
    }

    pub fn sync_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            sync_diagnostics_into_state(state, &self.path, &self.index, &self.lsp);
            cx.notify();
        });
    }

    pub fn jump_to_line_col(
        &mut self,
        line: u32,
        col: u32,
        select_len: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_vim_enabled() && matches!(self.vim.mode, VimMode::Visual | VimMode::VisualLine) {
            self.vim.clear_visual();
            self.vim.mode = VimMode::Normal;
            self.sync_vim_readonly(cx);
        }
        let select_when_non_vim = !self.is_vim_enabled();
        let padding = self.scroll_padding;
        self.editor.update(cx, |state, cx| {
            let pos = Position::new(line, col);
            state.set_cursor_position(pos, window, cx);
            if select_when_non_vim {
                if let Some(len) = select_len {
                    if len > 0 {
                        let start = state.text().position_to_offset(&pos);
                        let end = state
                            .text()
                            .position_to_offset(&Position::new(line, col + len));
                        state.set_selected_range(start..end, cx);
                    }
                }
            }
            let target_off = state.cursor();
            ensure_scroll_padding_for(state, target_off, padding, cx);
            state.focus(window, cx);
        });
        if self.is_vim_enabled() {
            self.vim.desired_col = col as usize;
        }
        self.sync_vim_decorations(cx);
        cx.notify();
    }

    pub fn align_cursor_in_viewport(&mut self, align: ScrollAlign, cx: &mut Context<Self>) {
        let padding = self.scroll_padding;
        self.editor.update(cx, |state, cx| {
            let Some(bounds) = state.text_bounds() else {
                return;
            };
            let Some(lh) = state.line_height() else {
                return;
            };
            if lh <= px(0.) || bounds.size.height <= px(0.) {
                return;
            }
            let line_idx = state.text().offset_to_position(state.cursor()).line as f32;
            let row_top = lh * line_idx;
            let vp_h = bounds.size.height;
            let pad = (lh * (padding as f32)).min((vp_h - lh).max(px(0.)) * 0.45);
            let mut scroll = state.scroll_offset();
            scroll.y = match align {
                ScrollAlign::Center => ((vp_h - lh) * 0.5 - row_top).min(px(0.)),
                ScrollAlign::Top => (pad - row_top).min(px(0.)),
                ScrollAlign::Bottom => (vp_h - pad - lh - row_top).min(px(0.)),
            };
            state.set_scroll_offset(scroll, cx);
        });
    }

    pub fn show_inline_hover_popover(
        &mut self,
        symbol_range: ByteRange<usize>,
        hover: Hover,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |state, cx| {
            state.present_hover(symbol_range, hover, cx);
        });
    }

    pub fn inlay_hints(&self) -> &[RenderedInlayHint] {
        &self.inlay_hints
    }

    pub fn apply_display_settings(
        &mut self,
        scroll_padding: usize,
        inlay_hints_enabled: bool,
        inline_hover_docs_enabled: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let padding = scroll_padding.clamp(0, 20);
        self.scroll_padding = padding;
        if let Some(window) = window {
            self.pending_surrounding_lines_sync = false;
            self.editor.update(cx, |state, cx| {
                state.set_cursor_surrounding_lines(Some(padding), window, cx);
            });
        } else {
            self.pending_surrounding_lines_sync = true;
        }
        if self.inlay_hints_enabled != inlay_hints_enabled {
            self.inlay_hints_enabled = inlay_hints_enabled;
            if inlay_hints_enabled {
                self.schedule_inlay_refresh(10, cx);
            } else {
                self.inlay_hints.clear();
                self.inlay_lines.clear();
            }
        }
        if self.inline_hover_docs_enabled != inline_hover_docs_enabled {
            self.inline_hover_docs_enabled = inline_hover_docs_enabled;
            if inline_hover_docs_enabled {
                self.last_doc_ident_key = None;
                self.schedule_inline_doc_refresh(cx);
            } else {
                self.inline_hover_doc = None;
                self.last_doc_ident_key = None;
            }
        }
        cx.notify();
    }

    pub fn schedule_inlay_refresh(&mut self, delay_ms: u64, cx: &mut Context<Self>) {
        if !self.inlay_hints_enabled {
            self.inlay_hints.clear();
            self.inlay_lines.clear();
            return;
        }
        self.inlay_seq = self.inlay_seq.wrapping_add(1);
        let seq = self.inlay_seq;
        let path = self.path.clone();
        let language = self.language.clone();
        let lsp = self.lsp.clone();
        let index = self.index.clone();
        let editor = self.editor.downgrade();

        self.inlay_task = Some(cx.spawn(async move |this, cx| {
            if delay_ms > 0 {
                cx.background_executor()
                    .timer(Duration::from_millis(delay_ms))
                    .await;
            }
            let text_opt = editor
                .read_with(cx, |ed, _| ed.value().to_string())
                .ok();
            let Some(text) = text_opt else {
                return;
            };
            let line_count = text.lines().count().max(1) as u32;
            let bg = cx.background_executor().clone();
            let hints = bg
                .spawn(async move {
                    if let Ok(lsp_hints) = lsp.inlay_hints(&path, &language, 0, line_count) {
                        if !lsp_hints.is_empty() {
                            return lsp_hints
                                .into_iter()
                                .filter_map(|h| {
                                    let label = inlay_label_to_string(&h.label);
                                    if label.is_empty() {
                                        return None;
                                    }
                                    Some(RenderedInlayHint {
                                        line: h.position.line,
                                        character: h.position.character,
                                        label,
                                        is_type: matches!(
                                            h.kind,
                                            Some(lsp_types::InlayHintKind::TYPE)
                                        ),
                                    })
                                })
                                .take(100)
                                .collect::<Vec<_>>();
                        }
                    }
                    index
                        .inlay_hints_for_file(&path, &text, 80)
                        .into_iter()
                        .map(|h| RenderedInlayHint {
                            line: h.line,
                            character: h.character,
                            label: h.label,
                            is_type: h.kind == InlayHintKindTag::Type,
                        })
                        .collect::<Vec<_>>()
                })
                .await;

            _ = this.update(cx, |tab, cx| {
                if tab.inlay_seq != seq || !tab.inlay_hints_enabled {
                    return;
                }
                tab.inlay_lines = group_inlay_hints_by_line(&hints);
                tab.inlay_hints = hints;
                cx.notify();
            });
        }));
    }

    pub fn schedule_inline_doc_refresh(&mut self, cx: &mut Context<Self>) {
        if !self.inline_hover_docs_enabled {
            self.inline_hover_doc = None;
            return;
        }
        let cursor = self.editor.read(cx).cursor();
        let text = self.text_snapshot(cx);
        let Some((_, ident_range)) = WorkspaceIndex::identifier_at(&text, cursor) else {
            if self.inline_hover_doc.is_some() || self.last_doc_ident_key.is_some() {
                self.inline_hover_doc = None;
                self.last_doc_ident_key = None;
                cx.notify();
            }
            return;
        };
        let key = (self.edit_seq, ident_range.clone());
        if self.last_doc_ident_key.as_ref() == Some(&key) {
            return;
        }
        self.last_doc_ident_key = Some(key.clone());

        let pos = self.editor.read(cx).text().offset_to_position(cursor);
        let lsp_range = {
            let rope = self.editor.read(cx).text();
            Range::new(
                rope.offset_to_position(ident_range.start),
                rope.offset_to_position(ident_range.end),
            )
        };

        // Instant synchronous local AST docstring check so cursor hover renders immediately.
        if let Some(info) = self.index.hover_info(&self.path, cursor, &text) {
            let hover = build_local_hover(&self.language, &info, Some(lsp_range));
            if let Some(doc) = info.doc_comment.as_deref().and_then(|d| summarize_inline_doc(d, 76)) {
                self.inline_hover_doc = Some(RenderedInlineDoc {
                    line: pos.line,
                    ident_range: ident_range.clone(),
                    summary: doc,
                    hover,
                });
                cx.notify();
            } else {
                self.inline_hover_doc = None;
            }
        } else {
            self.inline_hover_doc = None;
        }

        // Async LSP hover check for cross-file / external crate docstrings when LSP is Ready.
        self.inline_doc_seq = self.inline_doc_seq.wrapping_add(1);
        let seq = self.inline_doc_seq;
        let path = self.path.clone();
        let language = self.language.clone();
        let lsp = self.lsp.clone();
        let has_ready_lsp = lsp
            .active_status_for_language(&language)
            .is_some_and(|(_, s)| matches!(s, crate::lsp::LspServerStatus::Ready { .. }));
        if !has_ready_lsp {
            return;
        }

        self.inline_doc_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let bg = cx.background_executor().clone();
            let lsp_hover = bg
                .spawn(async move { lsp.hover(&path, &language, pos.line, pos.character).ok().flatten() })
                .await;
            let Some(hover) = lsp_hover else {
                return;
            };
            let Some(summary) = extract_hover_docstring_summary(&hover, 76) else {
                return;
            };
            _ = this.update(cx, |tab, cx| {
                if tab.inline_doc_seq != seq
                    || tab.last_doc_ident_key.as_ref() != Some(&key)
                    || !tab.inline_hover_docs_enabled
                {
                    return;
                }
                tab.inline_hover_doc = Some(RenderedInlineDoc {
                    line: pos.line,
                    ident_range,
                    summary,
                    hover,
                });
                cx.notify();
            });
        }));
    }

    pub fn save_to_disk(&mut self, cx: &mut Context<Self>) -> Result<()> {
        self.sync_pending = false;
        self.sync_task = None;
        let content = self.text_snapshot(cx).as_ref().clone();
        fs::write(&self.path, &content)?;
        self.saved_text = content.clone();
        self.dirty = false;
        self.index.reindex_file(&self.path, &content);
        self.lsp.did_save(&self.path, &self.language, &content);
        cx.notify();
        Ok(())
    }

    fn ensure_insert_for_fim(&mut self, cx: &mut Context<Self>) {
        if self.is_vim_enabled() && self.vim.mode != VimMode::Insert {
            let old_mode = self.vim.mode;
            let (text, cursor) = {
                let ed = self.editor.read(cx);
                (ed.value().to_string(), ed.cursor())
            };
            self.vim.clear_pending();
            self.vim.clear_visual();
            self.vim.mode = VimMode::Insert;
            self.record_vim_mode_transition(old_mode, VimMode::Insert, &text, cursor, cx);
            self.sync_vim_readonly(cx);
            self.sync_vim_decorations(cx);
        }
    }

    pub fn trigger_fim_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_insert_for_fim(cx);
        let cursor = self.editor.read(cx).cursor();
        if self
            .fim
            .active()
            .is_some_and(|a| a.file_path != self.path || a.offset != cursor)
        {
            self.fim.clear_active();
        }
        let utf16 = self.editor.read(cx).text().offset_to_offset_utf16(cursor);
        self.editor.update(cx, |state, cx| {
            state.focus(window, cx);
            EntityInputHandler::replace_text_in_range(
                state,
                Some(utf16..utf16),
                "",
                window,
                cx,
            );
        });
    }

    pub fn accept_fim_word(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let cursor = self.editor.read(cx).cursor();
        let Some(chunk) = self.fim.take_next_word(&self.path, cursor) else {
            return false;
        };
        self.insert_chunk_and_refresh_fim(&chunk, cursor, window, cx);
        true
    }

    pub fn accept_fim_line(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let cursor = self.editor.read(cx).cursor();
        let Some(chunk) = self.fim.take_next_line(&self.path, cursor) else {
            return false;
        };
        self.insert_chunk_and_refresh_fim(&chunk, cursor, window, cx);
        true
    }

    pub fn accept_fim_full(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.ensure_insert_for_fim(cx);
        let cursor = self.editor.read(cx).cursor();
        let Some(active) = self.fim.active() else {
            return false;
        };
        if active.file_path != self.path || active.offset != cursor {
            return false;
        }
        let Some(full) = active.current_text().map(str::to_string) else {
            return false;
        };
        self.fim.mark_accepted();
        let utf16 = self.editor.read(cx).text().offset_to_offset_utf16(cursor);
        self.editor.update(cx, |state, cx| {
            state.focus(window, cx);
            EntityInputHandler::replace_text_in_range(
                state,
                Some(utf16..utf16),
                &full,
                window,
                cx,
            );
        });
        true
    }

    pub fn cycle_fim(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.fim.cycle_candidate(delta).is_some() {
            self.trigger_fim_now(window, cx);
            true
        } else {
            false
        }
    }

    fn insert_chunk_and_refresh_fim(
        &mut self,
        chunk: &str,
        cursor: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_insert_for_fim(cx);
        let utf16 = self.editor.read(cx).text().offset_to_offset_utf16(cursor);
        self.editor.update(cx, |state, cx| {
            state.focus(window, cx);
            EntityInputHandler::replace_text_in_range(
                state,
                Some(utf16..utf16),
                chunk,
                window,
                cx,
            );
        });
    }
}

fn rope_equals_str(rope: &Rope, expected: &str) -> bool {
    if rope.len() != expected.len() {
        return false;
    }
    let mut offset = 0usize;
    let bytes = expected.as_bytes();
    for chunk in rope.chunks() {
        let end = offset + chunk.len();
        if bytes.get(offset..end) != Some(chunk.as_bytes()) {
            return false;
        }
        offset = end;
    }
    offset == bytes.len()
}

fn sync_diagnostics_into_state(
    state: &mut EditorState,
    path: &PathBuf,
    index: &WorkspaceIndex,
    lsp: &LspManager,
) {
    let lsp_diags = lsp.diagnostics_for(path);
    let ts_diags = index.file_diagnostics(path);
    if let Some(set) = state.diagnostics_mut() {
        set.clear();
        for d in lsp_diags {
            set.push(Diagnostic::from(d));
        }
        for d in ts_diags {
            let start = Position::new(d.line, d.col);
            let end = Position::new(d.line, d.col + 1);
            set.push(
                Diagnostic::new(start..end, d.message)
                    .with_severity(DiagnosticSeverity::Error)
                    .with_source("tree-sitter"),
            );
        }
    }
}

fn ensure_scroll_padding_for(
    state: &mut EditorState,
    target_offset: usize,
    padding_lines: usize,
    cx: &mut Context<EditorState>,
) {
    if padding_lines == 0 {
        return;
    }
    let Some(bounds) = state.text_bounds() else {
        return;
    };
    let Some(lh) = state.line_height() else {
        return;
    };
    if lh <= px(0.) || bounds.size.height <= px(0.) {
        return;
    }
    let line_idx = state.text().offset_to_position(target_offset).line as f32;
    let row_top = lh * line_idx;
    let vp_h = bounds.size.height;
    let pad = (lh * (padding_lines as f32)).min((vp_h - lh).max(px(0.)) * 0.45);
    let mut scroll = state.scroll_offset();
    let vis_top = row_top + scroll.y;
    let vis_bottom = vis_top + lh;
    if vis_top < pad {
        scroll.y = (pad - row_top).min(px(0.));
        state.set_scroll_offset(scroll, cx);
    } else if vis_bottom > vp_h - pad {
        scroll.y = (vp_h - pad - lh - row_top).min(px(0.));
        state.set_scroll_offset(scroll, cx);
    }
}

fn inlay_label_to_string(label: &InlayHintLabel) -> String {
    match label {
        InlayHintLabel::String(s) => s.trim().to_string(),
        InlayHintLabel::LabelParts(parts) => parts
            .iter()
            .map(|p| p.value.as_str())
            .collect::<String>()
            .trim()
            .to_string(),
    }
}

fn group_inlay_hints_by_line(hints: &[RenderedInlayHint]) -> Vec<(u32, String)> {
    let mut by_line: BTreeMap<u32, Vec<&str>> = BTreeMap::new();
    for h in hints {
        by_line.entry(h.line).or_default().push(h.label.as_str());
    }
    by_line
        .into_iter()
        .map(|(line, labels)| (line, labels.join("  ·  ")))
        .collect()
}

fn summarize_inline_doc(raw: &str, max_chars: usize) -> Option<String> {
    let first_line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("```") && !l.starts_with("---"))?;
    let clean = first_line
        .trim_start_matches("# ")
        .trim_start_matches("## ")
        .trim_start_matches("### ")
        .trim();
    if clean.is_empty() {
        return None;
    }
    let count = clean.chars().count();
    if count <= max_chars {
        Some(clean.to_string())
    } else {
        let mut s: String = clean.chars().take(max_chars.saturating_sub(1)).collect();
        s.push('…');
        Some(s)
    }
}

fn extract_hover_docstring_summary(hover: &Hover, max_chars: usize) -> Option<String> {
    let raw = match &hover.contents {
        HoverContents::Scalar(s) => match s {
            lsp_types::MarkedString::String(t) => t.clone(),
            lsp_types::MarkedString::LanguageString(ls) => ls.value.clone(),
        },
        HoverContents::Array(arr) => arr
            .iter()
            .map(|s| match s {
                lsp_types::MarkedString::String(t) => t.as_str(),
                lsp_types::MarkedString::LanguageString(ls) => ls.value.as_str(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        HoverContents::Markup(m) => m.value.clone(),
    };
    let mut in_fence = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || trimmed.is_empty() || trimmed.starts_with("---") {
            continue;
        }
        if let Some(s) = summarize_inline_doc(trimmed, max_chars) {
            return Some(s);
        }
    }
    None
}

pub fn build_local_hover(
    language: &str,
    info: &crate::index::HoverInfo,
    ident_range: Option<Range>,
) -> Hover {
    let mut md = format!(
        "```{}\n{}\n```\n\n**{}** `{}` · defined in `{}:{}` · **{}** references",
        language,
        info.signature,
        info.kind.label(),
        info.symbol_name,
        info.defined_rel_path,
        info.defined_line,
        info.reference_count,
    );
    if !info.members.is_empty() {
        md.push_str(&format!("\n\nMembers: `{}`", info.members.join("`, `")));
    }
    if let Some(doc) = &info.doc_comment {
        md.push_str(&format!("\n\n---\n\n{doc}"));
    }
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: md,
        }),
        range: ident_range,
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_inline_badge(
    x: gpui_kit::Pixels,
    y: gpui_kit::Pixels,
    line_height: gpui_kit::Pixels,
    font_size: gpui_kit::Pixels,
    text: String,
    text_color: gpui_kit::Hsla,
    bg_color: gpui_kit::Hsla,
    mono: &gpui_kit::Font,
    window: &mut Window,
    cx: &mut App,
) -> gpui_kit::Pixels {
    let shared: SharedString = text.into();
    let run = TextRun {
        len: shared.len(),
        font: mono.clone(),
        color: text_color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped = window
        .text_system()
        .shape_line(shared, font_size, &[run], None);
    let pad_x = px(6.);
    let badge_h = (font_size + px(4.)).min(line_height - px(2.)).max(px(14.));
    let badge_y = y + ((line_height - badge_h) * 0.5).max(px(0.));
    let badge_w = shaped.width + pad_x * 2.0;
    let mut quad = fill(
        gpui_kit::Bounds::new(point(x, badge_y), gpui_kit::size(badge_w, badge_h)),
        bg_color,
    );
    quad.corner_radii = gpui_kit::Corners::all(px(3.));
    window.paint_quad(quad);
    let text_y = y + ((line_height - font_size) * 0.5).max(px(0.));
    let _ = shaped.paint(
        point(x + pad_x, text_y),
        line_height,
        TextAlign::Left,
        None,
        window,
        cx,
    );
    x + badge_w
}

impl BasePanel for CodeTabPanel {
    fn panel_name(&self) -> &'static str {
        "CodeTabPanel"
    }

    fn closable(&self, _: &App) -> bool {
        true
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            let ide = self.ide.clone();
            let path = self.path.clone();
            window.defer(cx, move |_, cx| {
                _ = ide.update(cx, |ide, cx| {
                    ide.on_tab_focused(&path, cx);
                });
            });
        }
    }

    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.lsp.did_close(&self.path);
        let ide = self.ide.clone();
        let path = self.path.clone();
        window.defer(cx, move |_, cx| {
            _ = ide.update(cx, |ide, cx| {
                ide.on_tab_closed(&path, cx);
            });
        });
    }
}

impl Panel for CodeTabPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.rel_path.clone());
        Some(if self.dirty {
            format!("• {name}").into()
        } else {
            name.into()
        })
    }

    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.rel_path.clone());
        h_flex()
            .gap_1p5()
            .items_center()
            .child(
                Icon::from(gpui_kit::assets::IconName::FileCode)
                    .size(px(13.))
                    .text_color(cx.theme().muted_foreground),
            )
            .child(name)
            .when(self.dirty, |this| {
                this.child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .bg(ColorName::Amber.scale(400)),
                )
            })
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        Some(PanelControl::Toolbar)
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }

    fn title_bar(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for CodeTabPanel {}

impl Focusable for CodeTabPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl gpui_kit::Render for CodeTabPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_surrounding_lines_sync {
            self.pending_surrounding_lines_sync = false;
            let padding = self.scroll_padding;
            self.editor.update(cx, |state, cx| {
                state.set_cursor_surrounding_lines(Some(padding), window, cx);
            });
        }
        let editor_for_paint = self.editor.clone();
        let inlay_lines = if self.inlay_hints_enabled {
            self.inlay_lines.clone()
        } else {
            Vec::new()
        };
        let inline_doc = if self.inline_hover_docs_enabled {
            self.inline_hover_doc.clone()
        } else {
            None
        };
        let has_overlays = !inlay_lines.is_empty() || inline_doc.is_some();

        div()
            .id(("code-tab", cx.entity_id()))
            .relative()
            .size_full()
            .child(
                Editor::new(&self.editor)
                    .appearance(false)
                    .bordered(false)
                    .readonly(self.is_vim_readonly())
                    .size_full(),
            )
            .when(has_overlays, |root| {
                root.child(
                    div()
                        .absolute()
                        .inset_0()
                        .child(
                            canvas(
                                |_, _, _| {},
                                move |_, (), window, cx| {
                                    let (text_bounds, lh, inlay_anchors, doc_anchor) = {
                                        let ed = editor_for_paint.read(cx);
                                        let Some(text_bounds) = ed.text_bounds() else {
                                            return;
                                        };
                                        let Some(lh) = ed.line_height() else {
                                            return;
                                        };
                                        let rope = ed.text().clone();
                                        let total_lines = rope.lines_len();
                                        let mut inlay_anchors =
                                            Vec::with_capacity(inlay_lines.len());
                                        for (line_idx, label) in inlay_lines {
                                            let row = line_idx as usize;
                                            if row >= total_lines {
                                                continue;
                                            }
                                            let line_end = rope.line_end_offset(row);
                                            let Some(anchor) =
                                                ed.range_to_bounds(&(line_end..line_end))
                                            else {
                                                continue;
                                            };
                                            if anchor.bottom() < text_bounds.top()
                                                || anchor.top() > text_bounds.bottom()
                                            {
                                                continue;
                                            }
                                            let x = anchor.origin.x + px(14.);
                                            if x < text_bounds.right() - px(24.) {
                                                inlay_anchors.push((
                                                    line_idx,
                                                    x,
                                                    anchor.origin.y,
                                                    label,
                                                ));
                                            }
                                        }
                                        let doc_anchor = inline_doc.as_ref().and_then(|doc| {
                                            let row = doc.line as usize;
                                            if row >= total_lines {
                                                return None;
                                            }
                                            let line_end = rope.line_end_offset(row);
                                            let anchor =
                                                ed.range_to_bounds(&(line_end..line_end))?;
                                            if anchor.bottom() < text_bounds.top()
                                                || anchor.top() > text_bounds.bottom()
                                            {
                                                return None;
                                            }
                                            Some((
                                                doc.line,
                                                anchor.origin.x,
                                                anchor.origin.y,
                                                doc.summary.clone(),
                                            ))
                                        });
                                        (text_bounds, lh, inlay_anchors, doc_anchor)
                                    };
                                    let mono = font(cx.theme().mono_font_family.clone());
                                    let hint_size =
                                        (cx.theme().mono_font_size * 0.86).max(px(10.5));
                                    let inlay_bg = cx.theme().secondary.opacity(0.65);
                                    let inlay_fg = cx.theme().muted_foreground;
                                    let doc_bg = ColorName::Cyan.scale(900).opacity(0.45);
                                    let doc_fg = ColorName::Cyan.scale(300);
                                    let doc_line = doc_anchor.as_ref().map(|d| d.0);
                                    let mut doc_line_end_x = None;

                                    window.with_content_mask(
                                        Some(ContentMask {
                                            bounds: text_bounds,
                                        }),
                                        |window| {
                                            for (line_idx, x, y, label) in inlay_anchors {
                                                let end_x = paint_inline_badge(
                                                    x,
                                                    y,
                                                    lh,
                                                    hint_size,
                                                    label,
                                                    inlay_fg,
                                                    inlay_bg,
                                                    &mono,
                                                    window,
                                                    cx,
                                                );
                                                if doc_line == Some(line_idx) {
                                                    doc_line_end_x = Some(end_x);
                                                }
                                            }

                                            if let Some((_, base_x, y, summary)) = doc_anchor {
                                                let x = doc_line_end_x
                                                    .map(|ex| ex + px(8.))
                                                    .unwrap_or(base_x + px(14.));
                                                if x < text_bounds.right() - px(28.) {
                                                    paint_inline_badge(
                                                        x,
                                                        y,
                                                        lh,
                                                        hint_size,
                                                        format!("// {summary}"),
                                                        doc_fg,
                                                        doc_bg,
                                                        &mono,
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            }
                                        },
                                    );
                                },
                            )
                            .size_full(),
                        ),
                )
            })
    }
}

struct KestrelCompletionProvider {
    path: PathBuf,
    language: String,
    index: WorkspaceIndex,
    fim: FimEngine,
    fim_config: Arc<RwLock<FimConfig>>,
    lsp: LspManager,
    ide: WeakEntity<KestrelIde>,
}

impl CompletionProvider for KestrelCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let full = text.to_string();
        let safe_offset = offset.min(full.len());
        let before = &full[..safe_offset];
        let start = before
            .rfind(|c: char| !is_ident_char(c))
            .map(|i| i + 1)
            .unwrap_or(0);
        let prefix = before[start..].to_string();

        let pos = text.offset_to_position(safe_offset);
        let start_pos = text.offset_to_position(start);
        let end_pos = pos;
        let path = self.path.clone();
        let language = self.language.clone();
        let index = self.index.clone();
        let lsp = self.lsp.clone();
        let bg = cx.background_executor().clone();

        cx.spawn(async move |_| {
            let items = bg
                .spawn(async move {
                    let mut merged: Vec<CompletionItem> = Vec::new();
                    let mut seen = HashSet::new();

                    if let Ok(Some(resp)) =
                        lsp.completions(&path, &language, pos.line, pos.character)
                    {
                        let lsp_items = match resp {
                            CompletionResponse::Array(arr) => arr,
                            CompletionResponse::List(list) => list.items,
                        };
                        for item in lsp_items {
                            if seen.insert(item.label.clone()) {
                                merged.push(item);
                            }
                        }
                    }

                    for c in index.completion_candidates(&path, &prefix, &full) {
                        if seen.insert(c.label.clone()) {
                            merged.push(CompletionItem {
                                label: c.label.clone(),
                                kind: Some(c.kind),
                                detail: Some(c.detail),
                                documentation: c.documentation.map(|d| {
                                    lsp_types::Documentation::MarkupContent(MarkupContent {
                                        kind: MarkupKind::Markdown,
                                        value: d,
                                    })
                                }),
                                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                                    range: Range::new(start_pos, end_pos),
                                    new_text: c.label,
                                })),
                                ..Default::default()
                            });
                        }
                    }
                    merged.truncate(60);
                    merged
                })
                .await;

            Ok(CompletionResponse::Array(items))
        })
    }

    fn inline_completion(
        &self,
        rope: &Rope,
        offset: usize,
        _trigger: InlineCompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<InlineCompletionResponse>> {
        let cfg = self.fim_config.read().unwrap().clone();
        if !cfg.enabled {
            return Task::ready(Ok(InlineCompletionResponse::Array(vec![])));
        }

        let path = self.path.clone();
        let language = self.language.clone();
        let index = self.index.clone();
        let fim = self.fim.clone();
        let full_text = rope.to_string();
        let ide = self.ide.clone();

        fim.set_status(FimStatus::Predicting);
        let _ = ide.update(cx, |_, cx| cx.notify());

        let bg = cx.background_executor().clone();
        cx.spawn(async move |cx| {
            let suggestion = bg
                .spawn(async move {
                    fim.complete(&path, &language, offset, &full_text, &index, &cfg)
                })
                .await;

            let _ = cx.update(|cx| {
                _ = ide.update(cx, |ide, cx| ide.on_fim_updated(cx));
            });

            let items = match suggestion.and_then(|s| s.current_text().map(str::to_string)) {
                Some(text) if !text.is_empty() => vec![InlineCompletionItem {
                    insert_text: text,
                    filter_text: None,
                    range: None,
                    command: None,
                    insert_text_format: None,
                }],
                _ => Vec::new(),
            };
            Ok(InlineCompletionResponse::Array(items))
        })
    }

    fn inline_completion_debounce(&self) -> Duration {
        Duration::from_millis(self.fim_config.read().unwrap().debounce_ms)
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        !new_text.is_empty()
            && new_text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == ':')
    }
}

struct KestrelDefinitionProvider {
    path: PathBuf,
    language: String,
    index: WorkspaceIndex,
    lsp: LspManager,
}

impl DefinitionProvider for KestrelDefinitionProvider {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<LocationLink>>> {
        let full = text.to_string();
        let origin_range = WorkspaceIndex::identifier_at(&full, offset).map(|(_, r)| {
            Range::new(
                text.offset_to_position(r.start),
                text.offset_to_position(r.end),
            )
        });
        let pos = text.offset_to_position(offset);
        let path = self.path.clone();
        let language = self.language.clone();
        let index = self.index.clone();
        let lsp = self.lsp.clone();
        let bg = cx.background_executor().clone();

        cx.spawn(async move |_| {
            let links = bg
                .spawn(async move {
                    if let Ok(locs) = lsp.definition(&path, &language, pos.line, pos.character) {
                        if !locs.is_empty() {
                            return locs
                                .into_iter()
                                .filter_map(|loc| location_link_from_nav(&loc, origin_range))
                                .collect::<Vec<_>>();
                        }
                    }
                    index
                        .find_definitions(&path, offset, &full)
                        .into_iter()
                        .filter_map(|loc| loc.to_lsp_location_link(origin_range))
                        .collect::<Vec<_>>()
                })
                .await;
            Ok(links)
        })
    }
}

struct KestrelHoverProvider {
    path: PathBuf,
    language: String,
    index: WorkspaceIndex,
    lsp: LspManager,
}

impl HoverProvider for KestrelHoverProvider {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<Hover>>> {
        let full = text.to_string();
        let pos = text.offset_to_position(offset);
        let ident_range = WorkspaceIndex::identifier_at(&full, offset).map(|(_, r)| {
            Range::new(
                text.offset_to_position(r.start),
                text.offset_to_position(r.end),
            )
        });
        let path = self.path.clone();
        let language = self.language.clone();
        let index = self.index.clone();
        let lsp = self.lsp.clone();
        let bg = cx.background_executor().clone();

        cx.spawn(async move |_| {
            let result = bg
                .spawn(async move {
                    if let Ok(Some(hover)) = lsp.hover(&path, &language, pos.line, pos.character) {
                        return Some(hover);
                    }
                    let info = index.hover_info(&path, offset, &full)?;
                    Some(build_local_hover(&language, &info, ident_range))
                })
                .await;
            Ok(result)
        })
    }
}

struct KestrelCodeActionProvider {
    path: PathBuf,
    language: String,
    lsp: LspManager,
}

impl CodeActionProvider for KestrelCodeActionProvider {
    fn id(&self) -> SharedString {
        "kestrel-lsp-code-actions".into()
    }

    fn code_actions(
        &self,
        state: Entity<EditorState>,
        range: ByteRange<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeAction>>> {
        let (start, end) = {
            let st = state.read(cx);
            let rope = st.text();
            (
                rope.offset_to_position(range.start),
                rope.offset_to_position(range.end),
            )
        };
        let path = self.path.clone();
        let language = self.language.clone();
        let lsp = self.lsp.clone();
        let bg = cx.background_executor().clone();

        cx.spawn(async move |_| {
            let actions = bg
                .spawn(async move {
                    lsp.code_actions(&path, &language, start, end)
                        .unwrap_or_default()
                })
                .await;
            Ok(actions)
        })
    }

    fn perform_code_action(
        &self,
        state: Entity<EditorState>,
        action: CodeAction,
        _push_to_history: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        if let Some(edit) = action.edit {
            if let Some(changes) = edit.changes {
                if let Some(doc_uri) = crate::lsp::client::path_to_uri(&self.path) {
                    if let Some(edits) = changes.get(&doc_uri) {
                        let edits = edits.clone();
                        state.update(cx, |st, cx| {
                            st.apply_lsp_edits(&edits, window, cx);
                        });
                    }
                }
            }
        }
        Task::ready(Ok(()))
    }
}
