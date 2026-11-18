use crate::{
    ai::{AskAiPreset, CodeContextAttachment, parse_code_link_url},
    brand::{APP_DISPLAY_TITLE, APP_NAME, APP_TAGLINE, render_brand_logo},
    config::{AskAiConfig, FimConfig, FimProviderKind, IdeConfig, LspConfig, VimConfig},
    fim::{FimEngine, FimStatus},
    index::{NavLocation, OutlineNode, WorkspaceIndex, ceil_char_boundary, floor_char_boundary},
    lsp::{LspManager, LspServerStatus},
    ui::{
        AcceptFimLine, AcceptFimWord, AskAi, CloseModal, ConfirmModal, CycleFimNext, CycleFimPrev,
        ExplainSelection, FindReferences, GoToSymbol, ModalDown, ModalUp, NavigateBack,
        NavigateForward, NextTab, OpenFolder, OpenSettings, PrevTab, QuickOpen, SaveActiveFile,
        SwitchTab1, SwitchTab2, SwitchTab3, SwitchTab4, SwitchTab5, SwitchTab6, SwitchTab7,
        SwitchTab8, SwitchTab9, ToggleBottomDock, ToggleLeftDock, TogglePaletteRegex,
        ToggleRightDock, ToggleTheme, ToggleVimMode, TriggerFim,
        ask_ai_panel::AskAiPanel,
        editor_pane::{CodeTabPanel, build_local_hover},
        explorer_panel::WorkspaceExplorerPanel,
        inspector_panel::{CodeNavAndFimPanel, InspectorTab},
        modals::{
            ActiveModal, PaletteKind, PaletteModal, PaletteSelection, SettingsModal, SettingsTab,
            render_palette_overlay, render_settings_overlay, trigger_test_connection,
        },
        results_panel::{NavResultsPanel, ResultsTab},
        symbol_color,
    },
    vim::{
        CommandKind, VimEffect, VimKeyEvent, VimMode, VimOutcome, VimRegister,
        motions::{
            clamp_normal_cursor, extract_file_path_under_cursor, line_index_at, move_vertical,
        },
    },
};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeystrokeEvent, ParentElement as _, PathPromptOptions,
    Render, Styled as _, Subscription, Task, Window,
    component::{
        ActiveTheme as _, ColorName, Disableable as _, Icon, Sizable as _, Theme, ThemeMode,
        TitleBar,
        button::{Button, ButtonVariants as _},
        dock::{
            DockArea, DockEvent, DockLayout, DockPlacement, DockSkin, PaneNode, PaneRef, PanelId,
            PanelStyle, panel_handle,
        },
        h_flex,
        input::{GoToDefinition, RopeExt as _, ToggleCodeActions},
        status_bar::StatusBar,
        v_flex,
    },
    div, prelude::FluentBuilder as _, px,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, RwLock},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct NavHistoryEntry {
    path: PathBuf,
    line: u32,
    col: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LspGotoKind {
    Definition,
    Declaration,
    TypeDefinition,
    Implementation,
}

fn collect_center_panel_ids(node: &PaneNode, out: &mut Vec<PanelId>) {
    match node.kind() {
        PaneRef::Tabs { panels, .. } => out.extend_from_slice(panels),
        PaneRef::Split { children, .. } => {
            for child in children {
                collect_center_panel_ids(child, out);
            }
        }
    }
}

fn find_active_center_panel_id(node: &PaneNode) -> Option<PanelId> {
    match node.kind() {
        PaneRef::Tabs { panels, active_ix } => panels.get(active_ix).copied(),
        PaneRef::Split { children, .. } => children.iter().find_map(find_active_center_panel_id),
    }
}

pub struct KestrelIde {
    pub config: IdeConfig,
    pub fim_config: Arc<RwLock<FimConfig>>,
    pub ask_config: Arc<RwLock<AskAiConfig>>,
    pub lsp_config: Arc<RwLock<LspConfig>>,
    pub vim_config: Arc<RwLock<VimConfig>>,
    pub index: WorkspaceIndex,
    pub fim: FimEngine,
    pub lsp: LspManager,
    pub workspace_root: PathBuf,
    pub dock_area: Entity<DockArea>,
    pub dock_skin: Rc<DockSkin>,
    pub explorer: Entity<WorkspaceExplorerPanel>,
    pub inspector: Entity<CodeNavAndFimPanel>,
    pub ask_ai: Entity<AskAiPanel>,
    pub results: Entity<NavResultsPanel>,
    pub open_tabs: HashMap<PathBuf, Entity<CodeTabPanel>>,
    pub active_tab: Option<PathBuf>,
    pub active_modal: Option<ActiveModal>,
    pub persist_config: bool,
    pub vim_register: VimRegister,
    nav_history: Vec<NavHistoryEntry>,
    nav_cursor: usize,
    nav_last_was_dest: bool,
    status_notice: Option<String>,
    focus: FocusHandle,
    inspector_dirty: bool,
    inspector_refresh_seq: u64,
    inspector_task: Option<Task<()>>,
    breadcrumb_cache: RefCell<Option<(PathBuf, usize, u64, Vec<OutlineNode>)>>,
    _subscriptions: Vec<Subscription>,
}

impl KestrelIde {
    pub fn new(
        initial_workspace: Option<PathBuf>,
        mut config: IdeConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace_root = initial_workspace
            .or_else(|| config.last_workspace.clone())
            .filter(|p| p.exists() && p.is_dir())
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            });
        let workspace_root = fs::canonicalize(&workspace_root).unwrap_or(workspace_root);
        config.push_recent_workspace(workspace_root.clone());

        let theme_mode = if config.dark_mode {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
        Theme::change(theme_mode, Some(window), cx);

        let index = WorkspaceIndex::new();
        index.index_workspace(&workspace_root);

        let fim = FimEngine::new();
        let fim_config = Arc::new(RwLock::new(config.fim.clone()));
        let ask_config = Arc::new(RwLock::new(config.ask_ai.clone()));
        let lsp_config = Arc::new(RwLock::new(config.lsp.clone()));
        let vim_config = Arc::new(RwLock::new(config.vim.clone()));
        let lsp = LspManager::new(config.lsp.clone(), workspace_root.clone());

        let lsp_watch = lsp.clone();
        let mut last_lsp_seq = lsp.event_seq();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                let seq = lsp_watch.event_seq();
                if seq != last_lsp_seq {
                    last_lsp_seq = seq;
                    let Ok(_) = this.update(cx, |ide, cx| {
                        ide.on_lsp_updated(cx);
                    }) else {
                        break;
                    };
                } else if this.upgrade().is_none() {
                    break;
                }
            }
        })
        .detach();

        let weak_ide = cx.weak_entity();

        let (dock_area, dock_skin) = DockSkin::dock_area("kestrel-dock", None, window, cx);
        dock_skin.set_panel_style(PanelStyle::TabBar, cx);
        dock_skin.set_close_button_visible(true, cx);

        let explorer = cx.new(|cx| {
            WorkspaceExplorerPanel::new(index.clone(), weak_ide.clone(), window, cx)
        });
        let inspector = cx.new(|cx| {
            CodeNavAndFimPanel::new(
                index.clone(),
                fim.clone(),
                fim_config.clone(),
                weak_ide.clone(),
                window,
                cx,
            )
        });
        let ask_ai = cx.new(|cx| {
            AskAiPanel::new(
                index.clone(),
                lsp.clone(),
                fim_config.clone(),
                ask_config.clone(),
                weak_ide.clone(),
                window,
                cx,
            )
        });
        let results = cx.new(|cx| {
            NavResultsPanel::new(index.clone(), lsp.clone(), weak_ide.clone(), window, cx)
        });

        dock_area.update(cx, |area, cx| {
            area.set_center(DockLayout::tabs(), window, cx);
            area.set_dock(
                DockPlacement::Left,
                DockLayout::tabs().panel_view(panel_handle(explorer.clone()), cx),
                window,
                cx,
            );
            area.set_dock_size(DockPlacement::Left, px(260.), window, cx);
            area.set_dock(
                DockPlacement::Right,
                DockLayout::tabs()
                    .panel_view(panel_handle(inspector.clone()), cx)
                    .panel_view(panel_handle(ask_ai.clone()), cx),
                window,
                cx,
            );
            area.set_dock_size(DockPlacement::Right, px(345.), window, cx);
            area.set_dock(
                DockPlacement::Bottom,
                DockLayout::tabs().panel_view(panel_handle(results.clone()), cx),
                window,
                cx,
            );
            area.set_dock_size(DockPlacement::Bottom, px(215.), window, cx);
            if area.is_dock_open(DockPlacement::Bottom) {
                area.toggle_dock(DockPlacement::Bottom, window, cx);
            }
        });

        let vim_ide = cx.weak_entity();
        let key_sub = cx.intercept_keystrokes(move |event, window, cx| {
            let mut handled = false;
            let _ = vim_ide.update(cx, |ide, cx| {
                handled = ide.handle_intercepted_keystroke(event, window, cx);
            });
            if handled {
                cx.stop_propagation();
            }
        });

        let dock_sub = cx.subscribe_in(&dock_area, window, |this, _, ev: &DockEvent, window, cx| {
            if matches!(ev, DockEvent::LayoutChanged) {
                this.sync_tabs_with_dock(window, cx);
            }
        });

        let mut ide = Self {
            config,
            fim_config,
            ask_config,
            lsp_config,
            vim_config,
            index,
            fim,
            lsp,
            workspace_root,
            dock_area,
            dock_skin,
            explorer,
            inspector,
            ask_ai,
            results,
            open_tabs: HashMap::new(),
            active_tab: None,
            active_modal: None,
            persist_config: true,
            vim_register: VimRegister::default(),
            nav_history: Vec::new(),
            nav_cursor: 0,
            nav_last_was_dest: false,
            status_notice: None,
            focus: cx.focus_handle(),
            inspector_dirty: false,
            inspector_refresh_seq: 0,
            inspector_task: None,
            breadcrumb_cache: RefCell::new(None),
            _subscriptions: vec![key_sub, dock_sub],
        };

        let ws_title = ide
            .workspace_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| ide.workspace_root.display().to_string());
        window.set_window_title(&format!("{APP_NAME} — {ws_title}"));

        ide.open_default_workspace_file(window, cx);
        ide
    }

    fn open_default_workspace_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.index.file_entries();
        let preferred = [
            "src/main.rs",
            "src/lib.rs",
            "src/index.ts",
            "src/app.tsx",
            "main.py",
            "main.go",
            "README.md",
        ];
        for cand in preferred {
            if let Some(hit) = entries.iter().find(|f| f.rel_path == cand) {
                self.open_file(&hit.path.clone(), None, false, window, cx);
                return;
            }
        }
        if let Some(first) = entries.first() {
            self.open_file(&first.path.clone(), None, false, window, cx);
        }
    }

    pub fn set_workspace(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let canonical = fs::canonicalize(&dir).unwrap_or(dir);
        if !canonical.is_dir() {
            return;
        }
        self.workspace_root = canonical.clone();
        let ws_title = canonical
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| canonical.display().to_string());
        window.set_window_title(&format!("{APP_NAME} — {ws_title}"));
        self.config.push_recent_workspace(canonical.clone());
        if self.persist_config {
            let _ = self.config.save_global();
        }

        self.open_tabs.clear();
        self.active_tab = None;
        self.nav_history.clear();
        self.nav_cursor = 0;
        self.fim.clear_active();
        self.lsp.set_workspace(&canonical);

        self.dock_area.update(cx, |area, cx| {
            area.set_center(DockLayout::tabs(), window, cx);
        });

        let (files, syms) = self.index.index_workspace(&canonical);
        self.explorer.update(cx, |exp, cx| {
            exp.set_active_file(None, cx);
            exp.refresh(cx);
        });
        self.results.update(cx, |res, cx| {
            res.refresh_diagnostics(cx);
        });
        self.inspector.update(cx, |insp, cx| {
            insp.update_context(None, 0, "", cx);
        });
        self.status_notice = Some(format!("Indexed {files} files · {syms} symbols"));
        self.open_default_workspace_file(window, cx);
        cx.notify();
    }

    pub fn reindex_workspace(&mut self, cx: &mut Context<Self>) {
        let root = self.workspace_root.clone();
        let (files, syms) = self.index.index_workspace(&root);
        for (path, tab) in &self.open_tabs {
            let text = tab.read(cx).editor.read(cx).value().to_string();
            self.index.reindex_file(path, &text);
        }
        self.explorer.update(cx, |exp, cx| exp.refresh(cx));
        self.results
            .update(cx, |res, cx| res.refresh_diagnostics(cx));
        self.refresh_inspector_for_active(cx);
        self.status_notice = Some(format!("Re-indexed {files} files · {syms} symbols"));
        cx.notify();
    }

    pub fn open_file(
        &mut self,
        path: &Path,
        line_col: Option<(u32, u32)>,
        record_history: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file_selecting_inner(
            path,
            line_col.map(|p| p.0).unwrap_or(0),
            line_col.map(|p| p.1).unwrap_or(0),
            None,
            line_col.is_some(),
            record_history,
            window,
            cx,
        );
    }

    pub fn open_file_selecting(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
        select_len: Option<u32>,
        record_history: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file_selecting_inner(
            path,
            line,
            col,
            select_len,
            true,
            record_history,
            window,
            cx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn open_file_selecting_inner(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
        select_len: Option<u32>,
        explicit_cursor: bool,
        record_history: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if record_history {
            if let Some((cur_path, cur_line, cur_col)) = self.current_cursor_location(cx) {
                self.push_history_entry(cur_path, cur_line, cur_col);
            }
        }

        let tab = if let Some(existing) = self.open_tabs.get(&canonical).cloned() {
            let panel_id = PanelId::from(existing.entity_id());
            self.dock_area.update(cx, |area, cx| {
                area.select_panel(panel_id, window, cx);
            });
            existing
        } else {
            let Ok(text) = fs::read_to_string(&canonical) else {
                self.status_notice = Some(format!("Unable to open {}", canonical.display()));
                cx.notify();
                return;
            };
            self.index.reindex_file(&canonical, &text);
            let rel_path = self.index.rel_path_for(&canonical);
            let ide_weak = cx.weak_entity();
            let index = self.index.clone();
            let fim = self.fim.clone();
            let fim_cfg = self.fim_config.clone();
            let vim_cfg = self.vim_config.clone();
            let lsp = self.lsp.clone();
            let scroll_padding = self.config.scroll_padding;
            let inlay_hints = self.config.inlay_hints;
            let inline_hover_docs = self.config.inline_hover_docs;
            let tab = cx.new(|cx| {
                CodeTabPanel::new(
                    canonical.clone(),
                    rel_path,
                    text,
                    index,
                    fim,
                    fim_cfg,
                    vim_cfg,
                    scroll_padding,
                    inlay_hints,
                    inline_hover_docs,
                    lsp,
                    ide_weak,
                    window,
                    cx,
                )
            });
            self.open_tabs.insert(canonical.clone(), tab.clone());
            let panel_id = PanelId::from(tab.entity_id());
            let handle = panel_handle(tab.clone());
            self.dock_area.update(cx, |area, cx| {
                area.add_panel_view(handle, DockPlacement::Center, None, window, cx);
                area.select_panel(panel_id, window, cx);
            });
            tab
        };

        if explicit_cursor || line > 0 || col > 0 || select_len.is_some() {
            tab.update(cx, |t, cx| {
                t.jump_to_line_col(line, col, select_len, window, cx);
            });
        } else {
            tab.update(cx, |t, cx| {
                t.editor.update(cx, |ed, cx| ed.focus(window, cx));
            });
        }

        self.status_notice = None;
        self.active_tab = Some(canonical.clone());
        if record_history {
            if let Some((dest_path, dest_line, dest_col)) = self.current_cursor_location(cx) {
                self.push_history_entry(dest_path, dest_line, dest_col);
                self.nav_last_was_dest = true;
            }
        }
        self.explorer
            .update(cx, |exp, cx| exp.set_active_file(Some(canonical), cx));
        self.refresh_inspector_for_active(cx);
        cx.notify();
    }

    pub fn jump_to_nav_location(
        &mut self,
        loc: &NavLocation,
        record_history: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let len = (!loc.symbol_name.is_empty()).then(|| loc.symbol_name.chars().count() as u32);
        self.open_file_selecting(
            &loc.path,
            loc.start_line,
            loc.start_col,
            len,
            record_history,
            window,
            cx,
        );
    }

    pub fn handle_show_document(
        &mut self,
        params: &lsp_types::ShowDocumentParams,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Ok(parsed) = url::Url::parse(params.uri.as_str()) else {
            return false;
        };
        let Ok(target_path) = parsed.to_file_path() else {
            return false;
        };
        let (line, col, sel_len) = match params.selection {
            Some(r) => {
                let len = (r.end.line == r.start.line)
                    .then(|| r.end.character.saturating_sub(r.start.character));
                (r.start.line, r.start.character, len)
            }
            None => (0, 0, None),
        };
        self.open_file_selecting(&target_path, line, col, sel_len, true, window, cx);
        true
    }

    pub fn on_tab_focused(&mut self, path: &Path, cx: &mut Context<Self>) {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.active_tab = Some(canonical.clone());
        self.explorer
            .update(cx, |exp, cx| exp.set_active_file(Some(canonical), cx));
        self.sync_ask_ai_live_selection(cx);
        self.refresh_inspector_for_active(cx);
        cx.notify();
    }

    pub fn on_tab_closed(&mut self, path: &Path, cx: &mut Context<Self>) {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.open_tabs.remove(&canonical);
        if self.active_tab.as_ref() == Some(&canonical) {
            self.active_tab = self.open_tabs.keys().next().cloned();
            let active = self.active_tab.clone();
            self.explorer
                .update(cx, |exp, cx| exp.set_active_file(active, cx));
            self.refresh_inspector_for_active(cx);
        }
        cx.notify();
    }

    fn is_active_tab_path(&self, path: &Path) -> bool {
        if self.active_tab.as_deref() == Some(path) {
            return true;
        }
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.active_tab.as_ref() == Some(&canonical)
    }

    pub fn on_buffer_changed(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.is_active_tab_path(path) {
            self.schedule_inspector_refresh(cx);
        }
        if self.dock_area.read(cx).is_dock_open(DockPlacement::Bottom) {
            self.results
                .update(cx, |res, cx| res.refresh_diagnostics(cx));
        }
        cx.notify();
    }

    pub fn on_cursor_moved(&mut self, path: &Path, _cursor: usize, cx: &mut Context<Self>) {
        if self.is_active_tab_path(path) {
            self.sync_ask_ai_live_selection(cx);
            self.schedule_inspector_refresh(cx);
            cx.notify();
        }
    }

    fn is_inspector_context_visible(&self, cx: &App) -> bool {
        self.dock_area.read(cx).is_dock_open(DockPlacement::Right)
            && !matches!(self.inspector.read(cx).current_tab(), InspectorTab::FimWorkbench)
    }

    fn schedule_inspector_refresh(&mut self, cx: &mut Context<Self>) {
        if !self.is_inspector_context_visible(cx) {
            self.inspector_dirty = true;
            return;
        }
        self.inspector_refresh_seq = self.inspector_refresh_seq.wrapping_add(1);
        let expected_seq = self.inspector_refresh_seq;
        self.inspector_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(85)).await;
            _ = this.update(cx, |ide, cx| {
                if ide.inspector_refresh_seq == expected_seq {
                    ide.refresh_inspector_for_active(cx);
                }
            });
        }));
    }

    pub fn on_fim_updated(&mut self, cx: &mut Context<Self>) {
        self.inspector.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    pub fn on_lsp_updated(&mut self, cx: &mut Context<Self>) {
        for tab in self.open_tabs.values() {
            tab.update(cx, |t, cx| {
                t.sync_diagnostics(cx);
                t.schedule_inlay_refresh(10, cx);
            });
        }
        if self.dock_area.read(cx).is_dock_open(DockPlacement::Bottom) {
            self.results
                .update(cx, |res, cx| res.refresh_diagnostics(cx));
        }
        cx.notify();
    }

    pub fn refresh_inspector_for_active(&mut self, cx: &mut Context<Self>) {
        self.inspector_dirty = false;
        let Some(tab) = self.active_tab_entity() else {
            self.inspector
                .update(cx, |insp, cx| insp.update_context(None, 0, "", cx));
            return;
        };
        let (path, offset, text) = tab.update(cx, |t, cx| {
            let offset = t.editor.read(cx).cursor();
            let text = t.text_snapshot(cx);
            (t.path.clone(), offset, text)
        });
        self.inspector.update(cx, |insp, cx| {
            insp.update_context(Some(path), offset, &text, cx);
        });
    }

    fn active_tab_entity(&self) -> Option<Entity<CodeTabPanel>> {
        let path = self.active_tab.as_ref()?;
        self.open_tabs.get(path).cloned()
    }

    fn current_cursor_location(&self, cx: &App) -> Option<(PathBuf, u32, u32)> {
        let tab = self.active_tab_entity()?;
        let t = tab.read(cx);
        let pos = t.editor.read(cx).cursor_position();
        Some((t.path.clone(), pos.line, pos.character))
    }

    fn push_history_entry(&mut self, path: PathBuf, line: u32, col: u32) {
        let entry = NavHistoryEntry { path, line, col };
        if self.nav_history.get(self.nav_cursor.saturating_sub(1)) == Some(&entry) {
            return;
        }
        self.nav_history.truncate(self.nav_cursor);
        self.nav_history.push(entry);
        if self.nav_history.len() > 100 {
            self.nav_history.remove(0);
        }
        self.nav_cursor = self.nav_history.len();
    }

    pub fn push_current_jump_point(&mut self, cx: &App) {
        if let Some((path, line, col)) = self.current_cursor_location(cx) {
            self.push_history_entry(path, line, col);
            self.nav_last_was_dest = false;
        }
    }

    pub fn can_go_back(&self) -> bool {
        self.nav_cursor > 1
    }

    pub fn can_go_forward(&self) -> bool {
        self.nav_cursor < self.nav_history.len()
    }

    pub fn navigate_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.nav_cursor == self.nav_history.len() {
            if let Some((cur_path, cur_line, cur_col)) = self.current_cursor_location(cx) {
                let entry = NavHistoryEntry {
                    path: cur_path,
                    line: cur_line,
                    col: cur_col,
                };
                if self.nav_last_was_dest
                    && self
                        .nav_history
                        .last()
                        .is_some_and(|e| e.path == entry.path)
                {
                    if let Some(last) = self.nav_history.last_mut() {
                        *last = entry;
                    }
                } else if self.nav_history.last() != Some(&entry) {
                    self.push_history_entry(entry.path, entry.line, entry.col);
                }
            }
        }
        self.nav_last_was_dest = false;
        if !self.can_go_back() {
            self.status_notice = Some("At oldest jump position".into());
            cx.notify();
            return;
        }
        self.nav_cursor -= 1;
        if let Some(entry) = self.nav_history.get(self.nav_cursor - 1).cloned() {
            self.open_file(&entry.path, Some((entry.line, entry.col)), false, window, cx);
        }
    }

    pub fn navigate_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.nav_last_was_dest = false;
        if !self.can_go_forward() {
            self.status_notice = Some("At newest jump position".into());
            cx.notify();
            return;
        }
        if let Some(entry) = self.nav_history.get(self.nav_cursor).cloned() {
            self.nav_cursor += 1;
            self.open_file(&entry.path, Some((entry.line, entry.col)), false, window, cx);
        }
    }

    pub fn center_tabs_in_order(&self, cx: &App) -> Vec<(PathBuf, Entity<CodeTabPanel>)> {
        let mut by_panel_id: HashMap<PanelId, (PathBuf, Entity<CodeTabPanel>)> =
            HashMap::with_capacity(self.open_tabs.len());
        for (path, tab) in &self.open_tabs {
            by_panel_id.insert(PanelId::from(tab.entity_id()), (path.clone(), tab.clone()));
        }
        let mut ordered_ids = Vec::new();
        if let Some(tree) = self.dock_area.read(cx).layout(DockPlacement::Center) {
            collect_center_panel_ids(tree.root(), &mut ordered_ids);
        }
        ordered_ids
            .into_iter()
            .filter_map(|id| by_panel_id.remove(&id))
            .collect()
    }

    pub fn switch_to_tab_index(
        &mut self,
        index_1_based: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tabs = self.center_tabs_in_order(cx);
        if tabs.is_empty() || index_1_based == 0 {
            return;
        }
        let idx = index_1_based - 1;
        let Some((path, tab)) = tabs.get(idx).cloned() else {
            return;
        };
        let panel_id = PanelId::from(tab.entity_id());
        self.dock_area.update(cx, |area, cx| {
            area.select_panel(panel_id, window, cx);
        });
        self.activate_selected_tab(path, tab, window, cx);
    }

    pub fn switch_adjacent_tab(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tabs = self.center_tabs_in_order(cx);
        if tabs.len() <= 1 {
            return;
        }
        let cur_idx = self
            .active_tab
            .as_ref()
            .and_then(|active| tabs.iter().position(|(p, _)| p == active))
            .unwrap_or(0);
        let len = tabs.len() as isize;
        let next_idx = ((cur_idx as isize + delta).rem_euclid(len)) as usize;
        let (path, tab) = tabs[next_idx].clone();
        let panel_id = PanelId::from(tab.entity_id());
        self.dock_area.update(cx, |area, cx| {
            area.select_panel(panel_id, window, cx);
        });
        self.activate_selected_tab(path, tab, window, cx);
    }

    fn activate_selected_tab(
        &mut self,
        path: PathBuf,
        tab: Entity<CodeTabPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_tab = Some(path.clone());
        tab.update(cx, |t, cx| {
            t.editor.update(cx, |ed, cx| ed.focus(window, cx));
        });
        self.explorer
            .update(cx, |exp, cx| exp.set_active_file(Some(path), cx));
        self.refresh_inspector_for_active(cx);
        cx.notify();
    }

    fn sync_tabs_with_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut ordered_ids = Vec::new();
        let mut active_id = None;
        if let Some(tree) = self.dock_area.read(cx).layout(DockPlacement::Center) {
            collect_center_panel_ids(tree.root(), &mut ordered_ids);
            active_id = find_active_center_panel_id(tree.root());
        }

        // Remove any tabs closed via the tab bar `x` button.
        let valid_ids: std::collections::HashSet<PanelId> = ordered_ids.iter().copied().collect();
        let removed: Vec<PathBuf> = self
            .open_tabs
            .iter()
            .filter(|(_, tab)| !valid_ids.contains(&PanelId::from(tab.entity_id())))
            .map(|(p, _)| p.clone())
            .collect();
        for p in removed {
            self.open_tabs.remove(&p);
            self.lsp.did_close(&p);
        }

        if let Some(aid) = active_id {
            if let Some((next_path, next_tab)) = self
                .open_tabs
                .iter()
                .find(|(_, t)| PanelId::from(t.entity_id()) == aid)
                .map(|(p, t)| (p.clone(), t.clone()))
            {
                if self.active_tab.as_ref() != Some(&next_path) {
                    self.active_tab = Some(next_path.clone());
                    next_tab.update(cx, |t, cx| {
                        t.editor.update(cx, |ed, cx| ed.focus(window, cx));
                    });
                    self.explorer
                        .update(cx, |exp, cx| exp.set_active_file(Some(next_path), cx));
                    self.refresh_inspector_for_active(cx);
                    cx.notify();
                }
            }
        } else if self.open_tabs.is_empty() && self.active_tab.is_some() {
            self.active_tab = None;
            self.explorer
                .update(cx, |exp, cx| exp.set_active_file(None, cx));
            self.refresh_inspector_for_active(cx);
            cx.notify();
        }
    }

    pub fn go_to_definition_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_lsp_goto(LspGotoKind::Definition, window, cx);
    }

    pub fn go_to_declaration_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_lsp_goto(LspGotoKind::Declaration, window, cx);
    }

    pub fn go_to_type_definition_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_lsp_goto(LspGotoKind::TypeDefinition, window, cx);
    }

    pub fn go_to_implementation_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_lsp_goto(LspGotoKind::Implementation, window, cx);
    }

    fn run_lsp_goto(
        &mut self,
        kind: LspGotoKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab_entity() else {
            return;
        };
        let (path, language, offset, pos, text) = tab.update(cx, |t, cx| {
            t.flush_pending_sync(cx);
            let text = t.text_snapshot(cx).as_ref().clone();
            let ed = t.editor.read(cx);
            (
                t.path.clone(),
                t.language.clone(),
                ed.cursor(),
                ed.cursor_position(),
                text,
            )
        });
        let ident = WorkspaceIndex::identifier_at(&text, offset)
            .map(|(n, _)| n)
            .unwrap_or_else(|| "symbol".to_string());

        let lsp_status = self.lsp.active_status_for_language(&language);
        let lsp_ready = lsp_status
            .as_ref()
            .is_some_and(|(_, st)| matches!(st, LspServerStatus::Ready { .. }));
        let lsp_starting = lsp_status
            .as_ref()
            .is_some_and(|(_, st)| matches!(st, LspServerStatus::Starting));

        if !lsp_ready {
            let defs = self.index.find_definitions(&path, offset, &text);
            if defs.is_empty() && lsp_starting {
                self.status_notice = Some(format!(
                    "LSP indexing workspace… wait a moment for `{ident}`"
                ));
                cx.notify();
                return;
            }
            self.apply_definition_results(ident, defs, window, cx);
            return;
        }

        let lsp = self.lsp.clone();
        let index = self.index.clone();
        let bg = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let defs = bg
                .spawn(async move {
                    let lsp_res = match kind {
                        LspGotoKind::Definition => {
                            lsp.definition(&path, &language, pos.line, pos.character)
                        }
                        LspGotoKind::Declaration => {
                            lsp.declaration(&path, &language, pos.line, pos.character)
                        }
                        LspGotoKind::TypeDefinition => {
                            lsp.type_definition(&path, &language, pos.line, pos.character)
                        }
                        LspGotoKind::Implementation => {
                            lsp.implementation(&path, &language, pos.line, pos.character)
                        }
                    };
                    if let Ok(locs) = lsp_res {
                        if !locs.is_empty() {
                            return locs;
                        }
                    }
                    if matches!(kind, LspGotoKind::Definition | LspGotoKind::Declaration) {
                        if let Ok(fallback_type) =
                            lsp.type_definition(&path, &language, pos.line, pos.character)
                        {
                            if !fallback_type.is_empty() {
                                return fallback_type;
                            }
                        }
                    }
                    index.find_definitions(&path, offset, &text)
                })
                .await;

            _ = this.update_in(cx, |ide, window, cx| {
                ide.apply_definition_results(ident, defs, window, cx);
            });
        })
        .detach();
    }

    fn apply_definition_results(
        &mut self,
        ident: String,
        mut defs: Vec<NavLocation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for loc in &mut defs {
            if loc.symbol_name.is_empty() {
                loc.symbol_name = ident.clone();
            }
        }
        match defs.len() {
            0 => {
                self.status_notice = Some(format!("No definition found for `{ident}`"));
                cx.notify();
            }
            1 => {
                let target = &defs[0];
                if let Some((cur_path, cur_line, cur_col)) = self.current_cursor_location(cx) {
                    if target.path == cur_path
                        && target.start_line == cur_line
                        && cur_col >= target.start_col
                        && cur_col <= target.end_col.max(target.start_col + ident.len() as u32)
                    {
                        self.status_notice = Some(format!("Already at definition of `{ident}`"));
                        cx.notify();
                        return;
                    }
                }
                self.jump_to_nav_location(target, true, window, cx);
            }
            _ => {
                let index = self.index.clone();
                self.active_modal = Some(ActiveModal::Palette(PaletteModal::new(
                    PaletteKind::Definitions(ident, defs),
                    index,
                    window,
                    cx,
                )));
                cx.notify();
            }
        }
    }

    pub fn show_references_for(
        &mut self,
        symbol: &str,
        preferred_path: Option<&Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hits = self.index.find_references(symbol, preferred_path);
        let count = hits.len();
        self.results.update(cx, |res, cx| {
            res.set_references(symbol.to_string(), hits, cx);
        });
        self.ensure_dock_open(DockPlacement::Bottom, window, cx);
        self.status_notice = Some(format!("Found {count} reference(s) to `{symbol}`"));
        cx.notify();
    }

    pub fn find_references_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab_entity() else {
            return;
        };
        let (path, language, offset, pos, text) = tab.update(cx, |t, cx| {
            t.flush_pending_sync(cx);
            let text = t.text_snapshot(cx).as_ref().clone();
            let ed = t.editor.read(cx);
            (
                t.path.clone(),
                t.language.clone(),
                ed.cursor(),
                ed.cursor_position(),
                text,
            )
        });
        let Some((ident, _)) = WorkspaceIndex::identifier_at(&text, offset) else {
            self.status_notice = Some("Place cursor on a symbol to find references".into());
            cx.notify();
            return;
        };

        if self
            .lsp
            .active_status_for_language(&language)
            .is_none_or(|(_, st)| !matches!(st, LspServerStatus::Ready { .. }))
        {
            self.show_references_for(&ident, Some(&path), window, cx);
            return;
        }

        let lsp = self.lsp.clone();
        let index = self.index.clone();
        let bg = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let hits = bg
                .spawn(async move {
                    if let Ok(lsp_hits) =
                        lsp.references(&path, &language, pos.line, pos.character, &ident)
                    {
                        if !lsp_hits.is_empty() {
                            return (ident, lsp_hits);
                        }
                    }
                    let fallback = index.find_references(&ident, Some(&path));
                    (ident, fallback)
                })
                .await;

            _ = this.update_in(cx, |ide, window, cx| {
                let count = hits.1.len();
                ide.results.update(cx, |res, cx| {
                    res.set_references(hits.0.clone(), hits.1, cx);
                });
                ide.ensure_dock_open(DockPlacement::Bottom, window, cx);
                ide.status_notice = Some(format!("Found {count} reference(s) to `{}`", hits.0));
                cx.notify();
            });
        })
        .detach();
    }

    pub fn trigger_active_fim(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_tab_entity() {
            tab.update(cx, |t, cx| t.trigger_fim_now(window, cx));
        }
    }

    pub fn accept_active_fim_word(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(tab) = self.active_tab_entity() else {
            return false;
        };
        let accepted = tab.update(cx, |t, cx| t.accept_fim_word(window, cx));
        if accepted {
            self.refresh_inspector_for_active(cx);
            cx.notify();
        }
        accepted
    }

    pub fn accept_active_fim_line(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(tab) = self.active_tab_entity() else {
            return false;
        };
        let accepted = tab.update(cx, |t, cx| t.accept_fim_line(window, cx));
        if accepted {
            self.refresh_inspector_for_active(cx);
            cx.notify();
        }
        accepted
    }

    pub fn accept_active_fim_full(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(tab) = self.active_tab_entity() else {
            return false;
        };
        let accepted = tab.update(cx, |t, cx| t.accept_fim_full(window, cx));
        if accepted {
            self.refresh_inspector_for_active(cx);
            cx.notify();
        }
        accepted
    }

    pub fn cycle_active_fim(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_tab_entity() {
            tab.update(cx, |t, cx| {
                t.cycle_fim(delta, window, cx);
            });
            cx.notify();
        }
    }

    pub fn open_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.index.clone();
        self.active_modal = Some(ActiveModal::Palette(PaletteModal::new(
            PaletteKind::QuickOpen,
            index,
            window,
            cx,
        )));
        cx.notify();
    }

    pub fn open_go_to_symbol(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.index.clone();
        self.active_modal = Some(ActiveModal::Palette(PaletteModal::new(
            PaletteKind::WorkspaceSymbols,
            index,
            window,
            cx,
        )));
        cx.notify();
    }

    pub fn open_settings_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings_tab(SettingsTab::Fim, window, cx);
    }

    pub fn open_settings_tab(
        &mut self,
        tab: SettingsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fim = self.fim_config.read().unwrap().clone();
        let ask_ai = self.ask_config.read().unwrap().clone();
        let lsp = self.lsp_config.read().unwrap().clone();
        let vim = self.vim_config.read().unwrap().clone();
        let scroll_padding = self.config.scroll_padding;
        let inlay_hints = self.config.inlay_hints;
        let inline_hover_docs = self.config.inline_hover_docs;
        self.active_modal = Some(ActiveModal::Settings(SettingsModal::new(
            tab,
            &fim,
            &ask_ai,
            &lsp,
            &vim,
            scroll_padding,
            inlay_hints,
            inline_hover_docs,
            window,
            cx,
        )));
        cx.notify();
    }

    pub fn select_settings_provider(
        &mut self,
        kind: FimProviderKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ActiveModal::Settings(settings)) = &mut self.active_modal {
            settings.provider = kind;
            settings.test_status = None;
            settings.endpoint_input.update(cx, |inp, cx| {
                inp.set_value(kind.default_endpoint(), window, cx);
            });
            settings.model_input.update(cx, |inp, cx| {
                inp.set_value(kind.default_model(), window, cx);
            });
            cx.notify();
        }
    }

    pub fn run_settings_connection_test(&mut self, cx: &mut Context<Self>) {
        let Some(ActiveModal::Settings(settings)) = &mut self.active_modal else {
            return;
        };
        let base = self.fim_config.read().unwrap().clone();
        let candidate_cfg = settings.build_fim_config(&base, cx);
        settings.testing = true;
        settings.test_status = None;
        cx.notify();

        let bg = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let res = bg
                .spawn(async move { trigger_test_connection(candidate_cfg) })
                .await;
            _ = this.update(cx, |ide, cx| {
                if let Some(ActiveModal::Settings(s)) = &mut ide.active_modal {
                    s.testing = false;
                    s.test_status = Some(res);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn save_settings_modal(&mut self, cx: &mut Context<Self>) {
        let base_fim = self.fim_config.read().unwrap().clone();
        let base_ask_ai = self.ask_config.read().unwrap().clone();
        let base_lsp = self.lsp_config.read().unwrap().clone();

        let base_padding = self.config.scroll_padding;

        let (updated_fim, updated_ask_ai, updated_lsp, updated_vim, updated_display) = {
            let Some(ActiveModal::Settings(settings)) = &mut self.active_modal else {
                return;
            };
            let fim = settings.build_fim_config(&base_fim, cx);
            let ask_ai = settings.build_ask_ai_config(&base_ask_ai, cx);
            let lsp = match settings.build_lsp_config(&base_lsp, cx) {
                Ok(v) => v,
                Err(err) => {
                    settings.lsp_error = Some(err);
                    cx.notify();
                    return;
                }
            };
            let vim = settings.build_vim_config();
            let display = settings.build_display_settings(base_padding, cx);
            (fim, ask_ai, lsp, vim, display)
        };

        *self.fim_config.write().unwrap() = updated_fim.clone();
        *self.ask_config.write().unwrap() = updated_ask_ai.clone();
        *self.lsp_config.write().unwrap() = updated_lsp.clone();
        *self.vim_config.write().unwrap() = updated_vim.clone();
        self.config.fim = updated_fim;
        self.config.ask_ai = updated_ask_ai;
        self.config.lsp = updated_lsp.clone();
        self.config.vim = updated_vim;
        self.config.scroll_padding = updated_display.0;
        self.config.inlay_hints = updated_display.1;
        self.config.inline_hover_docs = updated_display.2;
        if self.persist_config {
            let _ = self.config.save_global();
        }
        self.sync_vim_config_to_tabs(cx);
        self.sync_display_settings_to_tabs(None, cx);
        self.fim.clear_active();
        self.lsp.reconcile_config(updated_lsp);
        self.active_modal = None;
        self.status_notice = Some("Saved AI FIM, LSP & Vim configuration".into());
        cx.notify();
    }

    pub fn sync_vim_config_to_tabs(&mut self, cx: &mut Context<Self>) {
        let tabs: Vec<Entity<CodeTabPanel>> = self.open_tabs.values().cloned().collect();
        for tab in tabs {
            tab.update(cx, |t, cx| t.apply_vim_config_change(cx));
        }
    }

    pub fn sync_display_settings_to_tabs(
        &mut self,
        mut window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let padding = self.config.scroll_padding;
        let inlay = self.config.inlay_hints;
        let inline_doc = self.config.inline_hover_docs;
        let tabs: Vec<Entity<CodeTabPanel>> = self.open_tabs.values().cloned().collect();
        for tab in tabs {
            tab.update(cx, |t, cx| {
                t.apply_display_settings(
                    padding,
                    inlay,
                    inline_doc,
                    window.as_deref_mut(),
                    cx,
                );
            });
        }
    }

    pub fn toggle_vim_mode(&mut self, cx: &mut Context<Self>) {
        let next = !self.config.vim.enabled;
        self.set_vim_enabled(next, cx);
    }

    pub fn set_vim_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.config.vim.enabled = enabled;
        self.vim_config.write().unwrap().enabled = enabled;
        if self.persist_config {
            let _ = self.config.save_global();
        }
        self.sync_vim_config_to_tabs(cx);
        self.status_notice = Some(if enabled {
            "Vim mode enabled".into()
        } else {
            "Vim mode disabled".into()
        });
        cx.notify();
    }

    pub fn close_active_tab(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(active_path) = self.active_tab.clone() else {
            return;
        };
        let Some(tab) = self.open_tabs.get(&active_path).cloned() else {
            return;
        };
        if !force && tab.read(cx).dirty {
            self.status_notice = Some("No write since last change (use :q! or :wq)".into());
            cx.notify();
            return;
        }
        self.open_tabs.remove(&active_path);
        self.dock_area.update(cx, |area, cx| {
            area.remove_panel(tab, window, cx);
        });
        self.active_tab = self.open_tabs.keys().next().cloned();
        if let Some(next_path) = self.active_tab.clone() {
            if let Some(next_tab) = self.open_tabs.get(&next_path) {
                next_tab.update(cx, |t, cx| {
                    t.editor.update(cx, |ed, cx| ed.focus(window, cx));
                });
            }
            self.explorer
                .update(cx, |exp, cx| exp.set_active_file(Some(next_path), cx));
        } else {
            self.explorer
                .update(cx, |exp, cx| exp.set_active_file(None, cx));
        }
        self.refresh_inspector_for_active(cx);
        cx.notify();
    }

    pub fn show_hover_at_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab_entity() else {
            return;
        };
        let (path, language, pos, offset, text, ident_range, lsp_range) = {
            let t = tab.read(cx);
            let ed = t.editor.read(cx);
            let text = ed.value().to_string();
            let offset = ed.cursor();
            let ident_range = WorkspaceIndex::identifier_at(&text, offset).map(|(_, r)| r);
            let rope = ed.text();
            let lsp_range = ident_range.as_ref().map(|r| {
                lsp_types::Range::new(
                    rope.offset_to_position(r.start),
                    rope.offset_to_position(r.end),
                )
            });
            (
                t.path.clone(),
                t.language.clone(),
                ed.cursor_position(),
                offset,
                text,
                ident_range,
                lsp_range,
            )
        };
        if let Some(info) = self.index.hover_info(&path, offset, &text) {
            self.status_notice = Some(format!(
                "{} `{}` · {} ({}:{})",
                info.kind.label(),
                info.symbol_name,
                info.signature,
                info.defined_rel_path,
                info.defined_line,
            ));
            let local_hover = build_local_hover(&language, &info, lsp_range);
            let byte_range = ident_range.clone().unwrap_or(offset..offset);
            tab.update(cx, |t, cx| {
                t.show_inline_hover_popover(byte_range, local_hover, cx);
            });
        } else if let Some((ident, _)) = WorkspaceIndex::identifier_at(&text, offset) {
            self.status_notice = Some(format!("Symbol `{ident}`"));
        }

        if self
            .lsp
            .active_status_for_language(&language)
            .is_some_and(|(_, st)| matches!(st, LspServerStatus::Ready { .. }))
        {
            let lsp = self.lsp.clone();
            let bg = cx.background_executor().clone();
            let tab_weak = tab.downgrade();
            let lsp_byte_range = ident_range.unwrap_or(offset..offset);
            cx.spawn(async move |this, cx| {
                let hover = bg
                    .spawn(async move { lsp.hover(&path, &language, pos.line, pos.character) })
                    .await;
                if let Ok(Some(h)) = hover {
                    let text = match &h.contents {
                        lsp_types::HoverContents::Scalar(ms) => match ms {
                            lsp_types::MarkedString::String(s) => s.clone(),
                            lsp_types::MarkedString::LanguageString(ls) => ls.value.clone(),
                        },
                        lsp_types::HoverContents::Markup(m) => m.value.clone(),
                        lsp_types::HoverContents::Array(arr) => arr
                            .iter()
                            .map(|ms| match ms {
                                lsp_types::MarkedString::String(s) => s.clone(),
                                lsp_types::MarkedString::LanguageString(ls) => ls.value.clone(),
                            })
                            .collect::<Vec<_>>()
                            .join(" · "),
                    };
                    let summary = text.lines().next().unwrap_or("").trim().to_string();
                    if !summary.is_empty() {
                        _ = this.update(cx, |ide, cx| {
                            ide.status_notice = Some(summary);
                            cx.notify();
                        });
                        _ = tab_weak.update(cx, |t, cx| {
                            t.show_inline_hover_popover(lsp_byte_range, h, cx);
                        });
                    }
                }
            })
            .detach();
        }
        cx.notify();
    }

    pub fn jump_to_diagnostic(
        &mut self,
        next: bool,
        errors_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab_entity() else {
            return;
        };
        let (path, cur_line, cur_col) = {
            let t = tab.read(cx);
            let pos = t.editor.read(cx).cursor_position();
            (t.path.clone(), pos.line, pos.character)
        };

        let mut diags: Vec<(u32, u32, String)> = self
            .lsp
            .diagnostics_for(&path)
            .into_iter()
            .filter(|d| {
                !errors_only
                    || d.severity == Some(lsp_types::DiagnosticSeverity::ERROR)
            })
            .map(|d| (d.range.start.line, d.range.start.character, d.message))
            .collect();
        for d in self.index.file_diagnostics(&path) {
            diags.push((d.line, d.col, d.message));
        }
        diags.sort_by_key(|d| (d.0, d.1));
        if diags.is_empty() {
            self.status_notice = Some(if errors_only {
                "No errors in active file".into()
            } else {
                "No diagnostics in active file".into()
            });
            cx.notify();
            return;
        }

        let target = if next {
            diags
                .iter()
                .find(|(l, c, _)| (*l, *c) > (cur_line, cur_col))
                .or_else(|| diags.first())
                .cloned()
        } else {
            diags
                .iter()
                .rev()
                .find(|(l, c, _)| (*l, *c) < (cur_line, cur_col))
                .or_else(|| diags.last())
                .cloned()
        };

        if let Some((line, col, msg)) = target {
            self.push_current_jump_point(cx);
            tab.update(cx, |t, cx| {
                t.jump_to_line_col(line, col, None, window, cx);
            });
            self.status_notice = Some(format!("L{}:{}: {msg}", line + 1, col + 1));
            cx.notify();
        }
    }

    pub fn jump_to_symbol_in_file(
        &mut self,
        next: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab_entity() else {
            return;
        };
        let (path, cur_line, cur_col) = tab.update(cx, |t, cx| {
            t.flush_pending_sync(cx);
            let pos = t.editor.read(cx).cursor_position();
            (t.path.clone(), pos.line, pos.character)
        });
        let syms = self.index.file_symbols(&path);
        if syms.is_empty() {
            self.status_notice = Some("No symbols in active file".into());
            cx.notify();
            return;
        }
        let target = if next {
            syms.iter()
                .find(|s| (s.start_line, s.start_col) > (cur_line, cur_col))
                .or_else(|| syms.first())
        } else {
            syms.iter()
                .rev()
                .find(|s| (s.start_line, s.start_col) < (cur_line, cur_col))
                .or_else(|| syms.last())
        };
        if let Some(sym) = target.cloned() {
            self.push_current_jump_point(cx);
            tab.update(cx, |t, cx| {
                t.jump_to_line_col(sym.start_line, sym.start_col, None, window, cx);
            });
            self.status_notice = Some(format!("{} {}", sym.kind.label(), sym.name));
            cx.notify();
        }
    }

    pub fn go_to_file_under_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab_entity() else {
            return;
        };
        let (active_path, text, offset) = tab.update(cx, |t, cx| {
            let text = t.text_snapshot(cx);
            let offset = t.editor.read(cx).cursor();
            (t.path.clone(), text, offset)
        });
        let Some(raw) = extract_file_path_under_cursor(&text, offset) else {
            self.status_notice = Some("No file or module path under cursor".into());
            cx.notify();
            return;
        };

        let parent_dir = active_path.parent().unwrap_or(&self.workspace_root);
        let mut candidates: Vec<PathBuf> = Vec::new();
        if raw.contains("::") {
            let parts: Vec<&str> = raw
                .split("::")
                .filter(|p| !p.is_empty() && *p != "self" && *p != "super")
                .collect();
            let stripped: Vec<&str> = parts
                .first()
                .is_some_and(|first| *first == "crate")
                .then(|| parts[1..].to_vec())
                .unwrap_or_else(|| parts.clone());
            for base in [parent_dir, self.workspace_root.as_path(), &self.workspace_root.join("src")] {
                for len in (1..=stripped.len()).rev() {
                    let sub = stripped[..len].join("/");
                    candidates.push(base.join(format!("{sub}.rs")));
                    candidates.push(base.join(&sub).join("mod.rs"));
                }
            }
        } else {
            candidates.push(parent_dir.join(&raw));
            candidates.push(parent_dir.join(format!("{raw}.rs")));
            candidates.push(parent_dir.join(&raw).join("mod.rs"));
            candidates.push(self.workspace_root.join(&raw));
            candidates.push(self.workspace_root.join("src").join(&raw));
            for ext in ["rs", "ts", "tsx", "js", "jsx", "py", "go"] {
                candidates.push(parent_dir.join(format!("{raw}.{ext}")));
            }
        }

        if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
            self.open_file(&found, Some((0, 0)), true, window, cx);
            return;
        }

        let needle = raw.rsplit("::").next().unwrap_or(&raw);
        let fuzzy_hits = self.index.search_files(needle);
        if let Some(hit) = fuzzy_hits.first() {
            self.open_file(&hit.path, Some((0, 0)), true, window, cx);
            return;
        }

        self.status_notice = Some(format!("Can't find file `{raw}`"));
        cx.notify();
    }

    pub fn jump_to_last_insert(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab_entity() else {
            return;
        };
        tab.update(cx, |t, cx| {
            let target = t
                .last_insert_offset
                .unwrap_or_else(|| t.editor.read(cx).cursor());
            t.set_vim_cursor(target, cx);
        });
    }

    pub fn open_outline_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inspector.update(cx, |insp, cx| {
            insp.set_tab(InspectorTab::Outline, cx);
        });
        self.ensure_dock_open(DockPlacement::Right, window, cx);
        self.refresh_inspector_for_active(cx);
    }

    pub fn show_jump_list(&mut self, cx: &mut Context<Self>) {
        if self.nav_history.is_empty() {
            self.status_notice = Some("Jump list is empty".into());
        } else {
            let summary: String = self
                .nav_history
                .iter()
                .rev()
                .take(4)
                .map(|e| {
                    let name = e
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("?");
                    format!("{}:{}", name, e.line + 1)
                })
                .collect::<Vec<_>>()
                .join(" <- ");
            self.status_notice = Some(format!(
                "Jumps ({}/{}): {summary}",
                self.nav_cursor,
                self.nav_history.len()
            ));
        }
        cx.notify();
    }

    pub fn show_tab_list(&mut self, cx: &mut Context<Self>) {
        let tabs = self.center_tabs_in_order(cx);
        if tabs.is_empty() {
            self.status_notice = Some("No open tabs".into());
        } else {
            let list: String = tabs
                .iter()
                .enumerate()
                .map(|(i, (p, _))| {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                    let marker = if self.active_tab.as_ref() == Some(p) {
                        "*"
                    } else {
                        ""
                    };
                    format!("{}:{marker}{name}", i + 1)
                })
                .collect::<Vec<_>>()
                .join("  ");
            self.status_notice = Some(list);
        }
        cx.notify();
    }

    pub fn capture_active_code_context(
        &self,
        allow_symbol_fallback: bool,
        cx: &mut Context<Self>,
    ) -> Option<CodeContextAttachment> {
        let tab = self.active_tab_entity()?.clone();
        let (path, rel_path, language, text, sel, cursor, vim_visual) = tab.update(cx, |t, cx| {
            let text = t.text_snapshot(cx);
            let ed = t.editor.read(cx);
            let cursor = ed.cursor();
            let sel = ed.selected_range();
            let vim_visual = if t.is_vim_enabled()
                && matches!(t.vim.mode, VimMode::Visual | VimMode::VisualLine)
            {
                t.vim.visual_range(&text, cursor).map(|(r, _)| r)
            } else {
                None
            };
            (
                t.path.clone(),
                t.rel_path.clone(),
                t.language.clone(),
                text,
                sel,
                cursor,
                vim_visual,
            )
        });

        let explicit_range = if sel.start != sel.end {
            Some(sel.start.min(sel.end)..sel.start.max(sel.end))
        } else {
            vim_visual.filter(|r| r.start != r.end)
        };

        let (start_line, end_line, code, anchor_offset, from_selection) =
            if let Some(range) = explicit_range {
                let lo = floor_char_boundary(&text, range.start.min(text.len()));
                let hi = ceil_char_boundary(&text, range.end.min(text.len()));
                if lo >= hi {
                    return None;
                }
                let start_line = line_index_at(&text, lo) as u32 + 1;
                let end_probe = if hi > lo { hi - 1 } else { lo };
                let end_line =
                    (line_index_at(&text, floor_char_boundary(&text, end_probe)) as u32 + 1)
                        .max(start_line);
                let mut snippet = text[lo..hi].to_string();
                if snippet.len() > 16_000 {
                    let cut = floor_char_boundary(&snippet, 16_000);
                    snippet.truncate(cut);
                    snippet.push_str("\n// ... truncated");
                }
                (start_line, end_line, snippet, lo, true)
            } else if allow_symbol_fallback {
                let chain = self.index.enclosing_chain(&path, cursor);
                let lines: Vec<&str> = text.lines().collect();
                if lines.is_empty() {
                    return None;
                }
                if let Some(sym) = chain.last() {
                    let s_idx = (sym.start_line as usize).min(lines.len().saturating_sub(1));
                    let e_idx = (sym.end_line as usize)
                        .min(lines.len().saturating_sub(1))
                        .max(s_idx);
                    let snippet = lines[s_idx..=e_idx].join("\n");
                    (s_idx as u32 + 1, e_idx as u32 + 1, snippet, cursor, false)
                } else {
                    let cur_line = line_index_at(&text, cursor);
                    let s_idx = cur_line.saturating_sub(12);
                    let e_idx = (cur_line + 12).min(lines.len().saturating_sub(1));
                    let snippet = lines[s_idx..=e_idx].join("\n");
                    (s_idx as u32 + 1, e_idx as u32 + 1, snippet, cursor, false)
                }
            } else {
                return None;
            };

        if code.trim().is_empty() {
            return None;
        }

        let enclosing_symbol = self
            .index
            .enclosing_chain(&path, anchor_offset)
            .last()
            .map(|s| s.name.clone());

        Some(CodeContextAttachment {
            id: 0,
            path,
            rel_path,
            language,
            start_line,
            end_line,
            code,
            enclosing_symbol,
            from_selection,
        })
    }

    fn has_active_editor_selection(&self, cx: &App) -> bool {
        let Some(tab) = self.active_tab_entity() else {
            return false;
        };
        let t = tab.read(cx);
        let sel = t.editor.read(cx).selected_range();
        sel.start != sel.end
            || (t.is_vim_enabled() && matches!(t.vim.mode, VimMode::Visual | VimMode::VisualLine))
    }

    pub fn sync_ask_ai_live_selection(&mut self, cx: &mut Context<Self>) {
        if !self.has_active_editor_selection(cx) {
            if self.ask_ai.read(cx).live_selection().is_some() {
                self.ask_ai.update(cx, |ai, cx| ai.set_live_selection(None, cx));
            }
            return;
        }
        let ctx = self.capture_active_code_context(false, cx);
        self.ask_ai.update(cx, |ai, cx| ai.set_live_selection(ctx, cx));
    }

    pub fn open_ask_ai_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_dock_open(DockPlacement::Right, window, cx);
        let panel_id = PanelId::from(self.ask_ai.entity_id());
        self.dock_area.update(cx, |area, cx| {
            area.select_panel(panel_id, window, cx);
        });
    }

    pub fn capture_selection_to_ask_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_ask_ai_panel(window, cx);
        if let Some(ctx) = self.capture_active_code_context(true, cx) {
            let label = ctx.chip_label();
            self.ask_ai.update(cx, |ai, cx| ai.attach_context(ctx, cx));
            self.status_notice = Some(format!("Attached `{label}` to Ask AI context"));
        } else {
            self.status_notice = Some("Open a file or select code to attach to Ask AI".into());
        }
        cx.notify();
    }

    pub fn open_ask_ai_with_selection(
        &mut self,
        explain_immediately: bool,
        question: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_ask_ai_panel(window, cx);
        self.sync_ask_ai_live_selection(cx);
        let ctx = self.capture_active_code_context(true, cx);
        if let Some(ctx) = ctx.clone() {
            if ctx.from_selection {
                self.ask_ai.update(cx, |ai, cx| ai.attach_context(ctx, cx));
            }
        }
        if explain_immediately {
            self.ask_ai.update(cx, |ai, cx| {
                ai.submit_preset(AskAiPreset::Explain, question, ctx, cx);
            });
        } else if let Some(q) = question {
            self.ask_ai.update(cx, |ai, cx| {
                ai.submit_preset(AskAiPreset::Custom, Some(q), ctx, cx);
            });
        } else {
            self.ask_ai.update(cx, |ai, cx| {
                ai.focus_prompt(window, cx);
            });
        }
        cx.notify();
    }

    pub fn run_ask_ai_preset(&mut self, preset: AskAiPreset, cx: &mut Context<Self>) {
        let fallback = self.capture_active_code_context(true, cx);
        self.ask_ai.update(cx, |ai, cx| {
            ai.submit_preset(preset, None, fallback, cx);
        });
    }

    pub fn submit_ask_ai_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_ask_ai_live_selection(cx);
        let fallback = self.capture_active_code_context(true, cx);
        self.ask_ai.update(cx, |ai, cx| {
            ai.submit_input(fallback, window, cx);
        });
    }

    pub fn open_code_link(
        &mut self,
        raw_url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let known = self.index.all_rel_paths();
        let Some(target) = parse_code_link_url(raw_url, &self.workspace_root, &known) else {
            return false;
        };
        if !target.path.is_file() {
            self.status_notice = Some(format!("File not found: {}", target.rel_path));
            cx.notify();
            return false;
        }
        let pos = target.zero_based_pos();
        self.open_file(&target.path, Some(pos), true, window, cx);
        self.status_notice = Some(format!("Opened {}", target.display_label()));
        cx.notify();
        true
    }

    pub fn handle_intercepted_keystroke(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.active_modal.is_some() || !self.config.vim.enabled {
            return false;
        }
        let ks = &event.keystroke;
        if ks.modifiers.platform || ks.modifiers.function || ks.modifiers.alt {
            return false;
        }
        if ks.key == "tab"
            || (ks.key.starts_with('f')
                && ks.key.len() > 1
                && ks.key[1..].bytes().all(|b| b.is_ascii_digit()))
        {
            return false;
        }

        let Some(tab) = self.active_tab_entity() else {
            return false;
        };
        let editor_focused = tab.read(cx).editor.read(cx).focus_handle(cx).is_focused(window);
        if !editor_focused {
            return false;
        }

        let vim_key = VimKeyEvent {
            key: ks.key.clone(),
            key_char: ks.key_char.clone(),
            ctrl: ks.modifiers.control,
            alt: ks.modifiers.alt,
            shift: ks.modifiers.shift,
        };
        if !tab.read(cx).vim.needs_text_for_key(&vim_key) {
            return false;
        }
        let (prev_mode, text, cursor) = tab.update(cx, |t, cx| {
            let text = t.text_snapshot(cx);
            let cursor = t.editor.read(cx).cursor();
            (t.vim.mode, text, cursor)
        });
        let outcome = tab.update(cx, |t, _| t.vim.handle_key(&text, cursor, vim_key));
        let new_mode = tab.read(cx).vim.mode;
        tab.update(cx, |t, cx| {
            t.record_vim_mode_transition(prev_mode, new_mode, &text, cursor, cx);
        });
        self.apply_vim_outcome(&tab, outcome, window, cx)
    }

    fn apply_vim_outcome(
        &mut self,
        tab: &Entity<CodeTabPanel>,
        outcome: VimOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut needs_refresh = outcome.consumed;
        for eff in outcome.effects {
            match eff {
                VimEffect::MoveCursor { pos } => {
                    tab.update(cx, |t, cx| t.set_vim_cursor(pos, cx));
                }
                VimEffect::SetSelection { anchor, head, .. } => {
                    tab.update(cx, |t, cx| t.set_vim_visual_selection(anchor, head, cx));
                }
                VimEffect::Replace {
                    range,
                    replacement,
                    new_cursor,
                } => {
                    tab.update(cx, |t, cx| {
                        t.apply_vim_replace(range, &replacement, new_cursor, window, cx);
                    });
                }
                VimEffect::ModeChanged(_) => {
                    tab.update(cx, |t, cx| {
                        t.sync_vim_readonly(cx);
                        t.sync_vim_decorations(cx);
                    });
                    needs_refresh = true;
                }
                VimEffect::WriteClipboard(reg) => {
                    self.vim_register = reg.clone();
                    if self.config.vim.use_system_clipboard {
                        cx.write_to_clipboard(ClipboardItem::new_string(reg.text));
                    }
                }
                VimEffect::RequestPaste { before, count } => {
                    let payload = self.resolve_vim_paste_payload(tab, cx);
                    let (text, cursor) = tab.update(cx, |t, cx| {
                        let text = t.text_snapshot(cx);
                        let cursor = t.editor.read(cx).cursor();
                        (text, cursor)
                    });
                    if let Some(paste_out) = tab.update(cx, |t, _| {
                        t.vim.apply_paste(&text, cursor, &payload, before, count)
                    }) {
                        self.apply_vim_outcome(tab, paste_out, window, cx);
                    }
                }
                VimEffect::RepeatLastChange => {
                    self.replay_vim_dot(tab, window, cx);
                }
                VimEffect::Undo => {
                    tab.update(cx, |t, cx| t.apply_vim_undo_redo(false, window, cx));
                }
                VimEffect::Redo => {
                    tab.update(cx, |t, cx| t.apply_vim_undo_redo(true, window, cx));
                }
                VimEffect::ScrollHalfPage { down, count } => {
                    let lines_delta = (16 * count.max(1)) as isize * if down { 1 } else { -1 };
                    let (text, cursor, desired) = tab.update(cx, |t, cx| {
                        let text = t.text_snapshot(cx);
                        let cursor = t.editor.read(cx).cursor();
                        (text, cursor, t.vim.desired_col)
                    });
                    let target = move_vertical(&text, cursor, lines_delta, desired, true);
                    tab.update(cx, |t, cx| t.set_vim_cursor(target, cx));
                }
                VimEffect::AlignCursor(align) => {
                    tab.update(cx, |t, cx| t.align_cursor_in_viewport(align, cx));
                }
                VimEffect::PushJumpPoint => self.push_current_jump_point(cx),
                VimEffect::JumpBack => self.navigate_back(window, cx),
                VimEffect::JumpForward => self.navigate_forward(window, cx),
                VimEffect::JumpLastInsert => self.jump_to_last_insert(cx),
                VimEffect::GoToDefinition => self.go_to_definition_at_cursor(window, cx),
                VimEffect::GoToDeclaration => self.go_to_declaration_at_cursor(window, cx),
                VimEffect::GoToTypeDefinition => self.go_to_type_definition_at_cursor(window, cx),
                VimEffect::GoToImplementation => self.go_to_implementation_at_cursor(window, cx),
                VimEffect::FindReferences => self.find_references_at_cursor(window, cx),
                VimEffect::GoToFile => self.go_to_file_under_cursor(window, cx),
                VimEffect::CodeActions => {
                    let ed_focus = tab.read(cx).editor.read(cx).focus_handle(cx);
                    ed_focus.dispatch_action(&ToggleCodeActions, window, cx);
                }
                VimEffect::OpenOutline => self.open_outline_panel(window, cx),
                VimEffect::ShowHover => self.show_hover_at_cursor(cx),
                VimEffect::NextDiagnostic { errors_only } => {
                    self.jump_to_diagnostic(true, errors_only, window, cx);
                }
                VimEffect::PrevDiagnostic { errors_only } => {
                    self.jump_to_diagnostic(false, errors_only, window, cx);
                }
                VimEffect::NextSymbol => self.jump_to_symbol_in_file(true, window, cx),
                VimEffect::PrevSymbol => self.jump_to_symbol_in_file(false, window, cx),
                VimEffect::NextTab => self.switch_adjacent_tab(1, window, cx),
                VimEffect::PrevTab => self.switch_adjacent_tab(-1, window, cx),
                VimEffect::GoToTab(idx) => self.switch_to_tab_index(idx, window, cx),
                VimEffect::ShowJumpList => self.show_jump_list(cx),
                VimEffect::ListTabs => self.show_tab_list(cx),
                VimEffect::SaveFile => self.save_active_file(cx),
                VimEffect::SaveAndClose => {
                    self.save_active_file(cx);
                    self.close_active_tab(false, window, cx);
                }
                VimEffect::CloseTab { force } => {
                    self.close_active_tab(force, window, cx);
                }
                VimEffect::OpenFile(None) => self.open_quick_open(window, cx),
                VimEffect::OpenFile(Some(rel_or_abs)) => {
                    let cand = PathBuf::from(&rel_or_abs);
                    let target = if cand.is_absolute() {
                        cand
                    } else {
                        self.workspace_root.join(cand)
                    };
                    self.open_file(&target, None, true, window, cx);
                }
                VimEffect::TriggerFim => self.trigger_active_fim(window, cx),
                VimEffect::AskAi { explain, question } => {
                    self.open_ask_ai_with_selection(explain, question, window, cx);
                }
                VimEffect::SetVimEnabled(enabled) => self.set_vim_enabled(enabled, cx),
                VimEffect::SetInlayHints(enabled) => {
                    self.config.inlay_hints = enabled;
                    if self.persist_config {
                        let _ = self.config.save_global();
                    }
                    self.sync_display_settings_to_tabs(Some(window), cx);
                    self.status_notice = Some(if enabled {
                        "Inlay hints enabled".into()
                    } else {
                        "Inlay hints disabled".into()
                    });
                    needs_refresh = true;
                }
                VimEffect::SetInlineHoverDocs(enabled) => {
                    self.config.inline_hover_docs = enabled;
                    if self.persist_config {
                        let _ = self.config.save_global();
                    }
                    self.sync_display_settings_to_tabs(Some(window), cx);
                    self.status_notice = Some(if enabled {
                        "Inline hover docstrings enabled".into()
                    } else {
                        "Inline hover docstrings disabled".into()
                    });
                    needs_refresh = true;
                }
                VimEffect::SetScrollPadding(lines) => {
                    let lines = lines.clamp(0, 20);
                    self.config.scroll_padding = lines;
                    if self.persist_config {
                        let _ = self.config.save_global();
                    }
                    self.sync_display_settings_to_tabs(Some(window), cx);
                    self.status_notice = Some(format!("Scroll padding set to {lines} lines"));
                    needs_refresh = true;
                }
                VimEffect::StatusMessage(msg) => {
                    self.status_notice = Some(msg);
                    needs_refresh = true;
                }
            }
        }
        if needs_refresh {
            cx.notify();
        }
        outcome.consumed
    }

    fn resolve_vim_paste_payload(
        &self,
        tab: &Entity<CodeTabPanel>,
        cx: &App,
    ) -> VimRegister {
        if self.config.vim.use_system_clipboard {
            if let Some(clip) = cx.read_from_clipboard().and_then(|item| item.text()) {
                if !clip.is_empty() {
                    if clip == self.vim_register.text {
                        return self.vim_register.clone();
                    }
                    let local = &tab.read(cx).vim.register;
                    if clip == local.text {
                        return local.clone();
                    }
                    return VimRegister {
                        linewise: clip.ends_with('\n'),
                        text: clip,
                    };
                }
            }
        }
        if !tab.read(cx).vim.register.text.is_empty() {
            tab.read(cx).vim.register.clone()
        } else {
            self.vim_register.clone()
        }
    }

    fn replay_vim_dot(
        &mut self,
        tab: &Entity<CodeTabPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(keys) = tab.read(cx).vim.last_change.clone() else {
            return;
        };
        if keys.is_empty() {
            return;
        }
        tab.update(cx, |t, _| t.vim.set_replaying_dot(true));
        for key in keys {
            let (mode, text, cursor) = tab.update(cx, |t, cx| {
                let text = t.text_snapshot(cx);
                let cursor = t.editor.read(cx).cursor();
                (t.vim.mode, text, cursor)
            });
            let out = if mode == VimMode::Insert {
                tab.update(cx, |t, _| t.vim.handle_synthetic_insert_key(&text, cursor, &key))
            } else {
                tab.update(cx, |t, _| t.vim.handle_key(&text, cursor, key))
            };
            self.apply_vim_outcome(tab, out, window, cx);
        }
        tab.update(cx, |t, cx| {
            t.vim.set_replaying_dot(false);
            let text = t.text_snapshot(cx);
            let cur = t.editor.read(cx).cursor();
            let _ = line_index_at(&text, cur);
            if t.is_vim_readonly() {
                let clamped = clamp_normal_cursor(&text, cur);
                t.set_vim_cursor(clamped, cx);
            }
        });
    }

    pub fn close_modal(&mut self, cx: &mut Context<Self>) {
        self.active_modal = None;
        cx.notify();
    }

    pub fn refresh_palette(&mut self, cx: &mut Context<Self>) {
        if let Some(ActiveModal::Palette(pal)) = &mut self.active_modal {
            let q = pal.query_input.read(cx).value().to_string();
            pal.recompute(&q);
            cx.notify();
        }
    }

    pub fn toggle_palette_regex(&mut self, cx: &mut Context<Self>) {
        if let Some(ActiveModal::Palette(pal)) = &mut self.active_modal {
            let q = pal.query_input.read(cx).value().to_string();
            pal.toggle_regex(&q);
            cx.notify();
        }
    }

    pub fn confirm_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = match &self.active_modal {
            Some(ActiveModal::Palette(pal)) => pal.selected_target(),
            _ => None,
        };
        if let Some(sel) = selection {
            self.active_modal = None;
            match sel {
                PaletteSelection::File(path) => {
                    self.open_file(&path, None, true, window, cx);
                }
                PaletteSelection::Location(loc) => {
                    self.jump_to_nav_location(&loc, true, window, cx);
                }
            }
        }
    }

    pub fn save_active_file(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_tab_entity() {
            let res = tab.update(cx, |t, cx| t.save_to_disk(cx));
            match res {
                Ok(()) => {
                    self.status_notice = Some("File saved".into());
                }
                Err(err) => {
                    self.status_notice = Some(format!("Save failed: {err}"));
                }
            }
            self.explorer.update(cx, |exp, cx| exp.refresh(cx));
            self.results
                .update(cx, |res, cx| res.refresh_diagnostics(cx));
            cx.notify();
        }
    }

    pub fn open_folder_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Project Workspace".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(dir) = paths.into_iter().next() {
                    _ = this.update_in(cx, |ide, window, cx| {
                        ide.set_workspace(dir, window, cx);
                    });
                }
            }
        })
        .detach();
    }

    pub fn toggle_dock(&mut self, placement: DockPlacement, window: &mut Window, cx: &mut Context<Self>) {
        self.dock_area.update(cx, |area, cx| {
            area.toggle_dock(placement, window, cx);
        });
        if placement == DockPlacement::Right && self.inspector_dirty {
            self.refresh_inspector_for_active(cx);
        } else if placement == DockPlacement::Bottom {
            self.results.update(cx, |res, cx| res.refresh_diagnostics(cx));
        }
        cx.notify();
    }

    pub fn ensure_dock_open(&mut self, placement: DockPlacement, window: &mut Window, cx: &mut Context<Self>) {
        self.dock_area.update(cx, |area, cx| {
            if !area.is_dock_open(placement) {
                area.toggle_dock(placement, window, cx);
            }
        });
        if placement == DockPlacement::Right && self.inspector_dirty {
            self.refresh_inspector_for_active(cx);
        }
        cx.notify();
    }

    pub fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = if cx.theme().is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        self.config.dark_mode = matches!(next, ThemeMode::Dark);
        if self.persist_config {
            let _ = self.config.save_global();
        }
        Theme::change(next, Some(window), cx);
        cx.notify();
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ws_name = self
            .workspace_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.workspace_root.display().to_string());
        let cfg = self.fim_config.read().unwrap().clone();
        let telemetry = self.fim.telemetry();
        let is_dark = cx.theme().is_dark();

        let left_open = self.dock_area.read(cx).is_dock_open(DockPlacement::Left);
        let bottom_open = self.dock_area.read(cx).is_dock_open(DockPlacement::Bottom);
        let right_open = self.dock_area.read(cx).is_dock_open(DockPlacement::Right);

        let active_lang = self
            .active_tab_entity()
            .map(|t| t.read(cx).language.clone())
            .unwrap_or_else(|| "rust".to_string());
        let lsp_pill = match self.lsp.active_status_for_language(&active_lang) {
            Some((name, LspServerStatus::Ready { .. })) => format!("LSP: {name}"),
            Some((name, LspServerStatus::Starting)) => format!("LSP: {name}…"),
            Some((name, LspServerStatus::Unavailable(_))) => format!("LSP: {name} (off)"),
            Some((name, LspServerStatus::Stopped)) => format!("LSP: {name}"),
            None => {
                if self.lsp_config.read().unwrap().enabled {
                    "LSP: Auto".to_string()
                } else {
                    "LSP: Off".to_string()
                }
            }
        };

        TitleBar::new()
            .child(
                h_flex()
                    .w_full()
                    .pr_3()
                    .items_center()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(render_brand_logo(20., cx))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().foreground)
                                    .child(APP_NAME),
                            ),
                    )
                    .child(
                        Button::new("tb-open-folder")
                            .label(ws_name)
                            .icon(gpui_kit::assets::IconName::FolderOpen)
                            .tooltip("Open Project Folder (⌘O)")
                            .xsmall()
                            .outline()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_folder_prompt(window, cx);
                            })),
                    )
                    .child(
                        Button::new("tb-quick-open")
                            .label("Files ⌘P")
                            .icon(gpui_kit::assets::IconName::Search)
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_quick_open(window, cx);
                            })),
                    )
                    .child(
                        Button::new("tb-symbols")
                            .label("Symbols ⌘⇧O")
                            .icon(gpui_kit::assets::IconName::Braces)
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_go_to_symbol(window, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("tb-lsp-pill")
                            .label(lsp_pill)
                            .icon(gpui_kit::assets::IconName::Cpu)
                            .tooltip("Configure LSP Servers (⌘,)")
                            .xsmall()
                            .outline()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings_tab(SettingsTab::Lsp, window, cx);
                            })),
                    )
                    .child(
                        Button::new("tb-ask-ai")
                            .label("Ask AI ⌘L")
                            .icon(gpui_kit::assets::IconName::MessageCircleCode)
                            .tooltip("Ask AI about selected code (⌘L) / Explain (⌘⇧E)")
                            .xsmall()
                            .outline()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_ask_ai_with_selection(false, None, window, cx);
                            })),
                    )
                    .child(
                        Button::new("tb-fim-pill")
                            .label(format!(
                                "FIM: {} · {}",
                                cfg.provider.short_label(),
                                telemetry.model_label
                            ))
                            .icon(gpui_kit::assets::IconName::Sparkles)
                            .tooltip("Configure AI FIM Backend (⌘,)")
                            .xsmall()
                            .outline()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings_tab(SettingsTab::Fim, window, cx);
                            })),
                    )
                    .child(
                        Button::new("tb-left-dock")
                            .icon(gpui_kit::assets::IconName::PanelLeft)
                            .tooltip("Toggle Explorer (⌘B)")
                            .xsmall()
                            .when(left_open, |b| b.primary())
                            .when(!left_open, |b| b.ghost())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_dock(DockPlacement::Left, window, cx);
                            })),
                    )
                    .child(
                        Button::new("tb-bottom-dock")
                            .icon(gpui_kit::assets::IconName::PanelBottom)
                            .tooltip("Toggle Navigation Results (⌘J)")
                            .xsmall()
                            .when(bottom_open, |b| b.primary())
                            .when(!bottom_open, |b| b.ghost())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_dock(DockPlacement::Bottom, window, cx);
                            })),
                    )
                    .child(
                        Button::new("tb-right-dock")
                            .icon(gpui_kit::assets::IconName::PanelRight)
                            .tooltip("Toggle Code Map & FIM Workbench (⌘⌥B)")
                            .xsmall()
                            .when(right_open, |b| b.primary())
                            .when(!right_open, |b| b.ghost())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_dock(DockPlacement::Right, window, cx);
                            })),
                    )
                    .child(
                        Button::new("tb-theme")
                            .icon(if is_dark {
                                gpui_kit::assets::IconName::Sun
                            } else {
                                gpui_kit::assets::IconName::Moon
                            })
                            .tooltip("Toggle Dark / Light Theme")
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_theme(window, cx);
                            })),
                    ),
            )
    }

    fn render_nav_breadcrumb_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let can_back = self.can_go_back();
        let can_fwd = self.can_go_forward();

        let (rel_path, active_path, cursor_offset) = match self.active_tab_entity() {
            Some(tab) => {
                let t = tab.read(cx);
                let ed = t.editor.read(cx);
                (Some(t.rel_path.clone()), Some(t.path.clone()), ed.cursor())
            }
            None => (None, None, 0),
        };
        let rev = self.index.revision();
        let chain = if let Some(p) = &active_path {
            let mut cache = self.breadcrumb_cache.borrow_mut();
            let hit = cache
                .as_ref()
                .filter(|(cp, coff, crev, _)| cp == p && *coff == cursor_offset && *crev == rev)
                .map(|(_, _, _, nodes)| nodes.clone());
            match hit {
                Some(nodes) => nodes,
                None => {
                    let nodes = self.index.enclosing_chain(p, cursor_offset);
                    *cache = Some((p.clone(), cursor_offset, rev, nodes.clone()));
                    nodes
                }
            }
        } else {
            Vec::new()
        };
        let has_fim = self.fim.active().is_some();
        let has_sel = self.has_active_editor_selection(cx);

        h_flex()
            .px_2p5()
            .py_1()
            .gap_1p5()
            .items_center()
            .bg(cx.theme().tab_bar)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("nav-back")
                    .icon(gpui_kit::assets::IconName::ArrowLeft)
                    .tooltip("Navigate Back (⌘⌥←)")
                    .xsmall()
                    .ghost()
                    .disabled(!can_back)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.navigate_back(window, cx);
                    })),
            )
            .child(
                Button::new("nav-fwd")
                    .icon(gpui_kit::assets::IconName::ArrowRight)
                    .tooltip("Navigate Forward (⌘⌥→)")
                    .xsmall()
                    .ghost()
                    .disabled(!can_fwd)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.navigate_forward(window, cx);
                    })),
            )
            .child(
                div()
                    .w(px(1.))
                    .h(px(14.))
                    .bg(cx.theme().border),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap_1()
                    .items_center()
                    .when_some(rel_path, |bar, rel| {
                        let path_copy = active_path.clone();
                        bar.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(rel),
                        )
                        .children(chain.into_iter().enumerate().map(|(ix, sym)| {
                            let color = symbol_color(sym.kind, cx);
                            let target_path = path_copy.clone();
                            let line = sym.start_line;
                            let col = sym.start_col;
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(
                                    Icon::from(gpui_kit::assets::IconName::ChevronRight)
                                        .size(px(12.))
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .child(
                                    Button::new(("crumb-sym", ix))
                                        .label(sym.name)
                                        .xsmall()
                                        .ghost()
                                        .text_color(color)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            if let Some(p) = &target_path {
                                                this.open_file(p, Some((line, col)), true, window, cx);
                                            }
                                        })),
                                )
                        }))
                    })
                    .when(active_path.is_none(), |bar| {
                        bar.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("No active file — open a file from Explorer or press ⌘P"),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("bar-def")
                            .label("Def (F12)")
                            .icon(gpui_kit::assets::IconName::CornerDownRight)
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.go_to_definition_at_cursor(window, cx);
                            })),
                    )
                    .child(
                        Button::new("bar-refs")
                            .label("Refs (⇧F12)")
                            .icon(gpui_kit::assets::IconName::Crosshair)
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.find_references_at_cursor(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .w(px(1.))
                            .h(px(14.))
                            .bg(cx.theme().border),
                    )
                    .when(has_sel, |bar| {
                        bar.child(
                            Button::new("bar-explain-sel")
                                .label("Explain Sel (⌘⇧E)")
                                .icon(gpui_kit::assets::IconName::WandSparkles)
                                .xsmall()
                                .primary()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_ask_ai_with_selection(true, None, window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("bar-ask-ai")
                            .label("Ask AI (⌘L)")
                            .icon(gpui_kit::assets::IconName::MessageCircleCode)
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_ask_ai_with_selection(false, None, window, cx);
                            })),
                    )
                    .child(
                        Button::new("bar-fim-trigger")
                            .label("FIM (⌥\\)")
                            .icon(gpui_kit::assets::IconName::Zap)
                            .xsmall()
                            .when(has_fim, |b| b.primary())
                            .when(!has_fim, |b| b.outline())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.trigger_active_fim(window, cx);
                            })),
                    )
                    .when(has_fim, |bar| {
                        bar.child(
                            Button::new("bar-fim-word")
                                .label("Word (⌥→)")
                                .xsmall()
                                .outline()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.accept_active_fim_word(window, cx);
                                })),
                        )
                        .child(
                            Button::new("bar-fim-tab")
                                .label("Accept (Tab)")
                                .xsmall()
                                .primary()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.accept_active_fim_full(window, cx);
                                })),
                        )
                    }),
            )
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (files, syms, ts_errs) = self.index.stats();
        let lsp_errs = self.lsp.diagnostic_count();
        let total_diags = ts_errs + lsp_errs;
        let telemetry = self.fim.telemetry();
        let (cursor_txt, lang_txt, dirty, vim_badge_info) = match self.active_tab_entity() {
            Some(tab) => {
                let t = tab.read(cx);
                let pos = t.editor.read(cx).cursor_position();
                let vim_info = if self.config.vim.enabled {
                    let prefix = match t.vim.mode {
                        VimMode::Command(CommandKind::Ex) => {
                            Some(format!(":{}", t.vim.command_buffer))
                        }
                        VimMode::Command(CommandKind::SearchForward) => {
                            Some(format!("/{}", t.vim.command_buffer))
                        }
                        VimMode::Command(CommandKind::SearchBackward) => {
                            Some(format!("?{}", t.vim.command_buffer))
                        }
                        _ => None,
                    };
                    Some((t.vim.mode, t.vim.pending_display(), prefix))
                } else {
                    None
                };
                (
                    format!("Ln {}, Col {}", pos.line + 1, pos.character + 1),
                    t.language.clone(),
                    t.dirty,
                    vim_info,
                )
            }
            None => ("Ln -, Col -".to_string(), "plain".to_string(), false, None),
        };

        let lsp_status_txt = match self.lsp.active_status_for_language(&lang_txt) {
            Some((name, st)) => format!("LSP {name}: {}", st.short_label()),
            None => "LSP Idle".to_string(),
        };

        let fim_badge = match &telemetry.status {
            FimStatus::Disabled => "FIM Off".to_string(),
            FimStatus::Idle => format!("FIM Ready · {}", telemetry.provider_label),
            FimStatus::Predicting => "FIM Predicting...".to_string(),
            FimStatus::Ready => format!(
                "FIM {} · {}ms",
                telemetry.provider_label, telemetry.last_latency_ms
            ),
            FimStatus::Cached => format!("FIM {} · cached", telemetry.provider_label),
            FimStatus::Error(_) => "FIM Fallback".to_string(),
        };

        StatusBar::new()
            .left(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{files} files · {syms} symbols")),
                    )
                    .child(
                        Button::new("sb-diags")
                            .label(format!("{total_diags} diagnostics"))
                            .icon(gpui_kit::assets::IconName::CircleDot)
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.results.update(cx, |res, cx| {
                                    res.set_tab(ResultsTab::Diagnostics, cx);
                                });
                                this.ensure_dock_open(DockPlacement::Bottom, window, cx);
                            })),
                    )
                    .child(
                        Button::new("sb-lsp")
                            .label(lsp_status_txt)
                            .icon(gpui_kit::assets::IconName::Cpu)
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings_tab(SettingsTab::Lsp, window, cx);
                            })),
                    )
                    .child(
                        Button::new("sb-vim")
                            .label(match &vim_badge_info {
                                Some((mode, _, _)) => format!("-- {} --", mode.label()),
                                None if self.config.vim.enabled => "-- NORMAL --".to_string(),
                                None => "Vim Off".to_string(),
                            })
                            .icon(gpui_kit::assets::IconName::Terminal)
                            .xsmall()
                            .when(self.config.vim.enabled, |b| b.primary())
                            .when(!self.config.vim.enabled, |b| b.ghost())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings_tab(SettingsTab::Vim, window, cx);
                            })),
                    )
                    .when_some(
                        vim_badge_info
                            .as_ref()
                            .and_then(|(_, _, cmd)| cmd.clone()),
                        |row, cmd| {
                            row.child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .bg(cx.theme().secondary)
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(format!("{cmd}_")),
                            )
                        },
                    )
                    .when_some(
                        vim_badge_info
                            .as_ref()
                            .and_then(|(_, pending, _)| {
                                if pending.is_empty() {
                                    None
                                } else {
                                    Some(pending.clone())
                                }
                            }),
                        |row, pending| {
                            row.child(
                                div()
                                    .text_xs()
                                    .text_color(ColorName::Amber.scale(400))
                                    .child(pending),
                            )
                        },
                    )
                    .when_some(self.status_notice.clone(), |row, notice| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(ColorName::Cyan.scale(400))
                                .child(notice),
                        )
                    }),
            )
            .right(
                h_flex()
                    .gap_2p5()
                    .items_center()
                    .when(dirty, |row| {
                        row.child(
                            Button::new("sb-save")
                                .label("Save (⌘S)")
                                .xsmall()
                                .outline()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.save_active_file(cx);
                                })),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().foreground)
                            .child(cursor_txt),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(lang_txt),
                    )
                    .child(
                        Button::new("sb-fim")
                            .label(fim_badge)
                            .icon(gpui_kit::assets::IconName::Sparkles)
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.inspector.update(cx, |insp, cx| {
                                    insp.set_tab(InspectorTab::FimWorkbench, cx);
                                });
                                this.ensure_dock_open(DockPlacement::Right, window, cx);
                            })),
                    ),
            )
    }
}

impl Focusable for KestrelIde {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for KestrelIde {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ide_weak = cx.weak_entity();
        let has_open_tabs = !self.open_tabs.is_empty();

        v_flex()
            .id("kestrel-ide-root")
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| {
                this.open_quick_open(window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToSymbol, window, cx| {
                this.open_go_to_symbol(window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToDefinition, window, cx| {
                this.go_to_definition_at_cursor(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FindReferences, window, cx| {
                this.find_references_at_cursor(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NavigateBack, window, cx| {
                this.navigate_back(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NavigateForward, window, cx| {
                this.navigate_forward(window, cx);
            }))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| {
                this.switch_adjacent_tab(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                this.switch_adjacent_tab(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTab1, window, cx| {
                this.switch_to_tab_index(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTab2, window, cx| {
                this.switch_to_tab_index(2, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTab3, window, cx| {
                this.switch_to_tab_index(3, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTab4, window, cx| {
                this.switch_to_tab_index(4, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTab5, window, cx| {
                this.switch_to_tab_index(5, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTab6, window, cx| {
                this.switch_to_tab_index(6, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTab7, window, cx| {
                this.switch_to_tab_index(7, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTab8, window, cx| {
                this.switch_to_tab_index(8, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTab9, window, cx| {
                this.switch_to_tab_index(9, window, cx);
            }))
            .on_action(cx.listener(|this, _: &TriggerFim, window, cx| {
                this.trigger_active_fim(window, cx);
            }))
            .on_action(cx.listener(|this, _: &AcceptFimWord, window, cx| {
                if !this.accept_active_fim_word(window, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &AcceptFimLine, window, cx| {
                if !this.accept_active_fim_line(window, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &CycleFimNext, window, cx| {
                this.cycle_active_fim(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CycleFimPrev, window, cx| {
                this.cycle_active_fim(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &AskAi, window, cx| {
                this.open_ask_ai_with_selection(false, None, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ExplainSelection, window, cx| {
                this.open_ask_ai_with_selection(true, None, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SaveActiveFile, _, cx| {
                this.save_active_file(cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings_modal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenFolder, window, cx| {
                this.open_folder_prompt(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleLeftDock, window, cx| {
                this.toggle_dock(DockPlacement::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRightDock, window, cx| {
                this.toggle_dock(DockPlacement::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleBottomDock, window, cx| {
                this.toggle_dock(DockPlacement::Bottom, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleVimMode, _, cx| {
                this.toggle_vim_mode(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleTheme, window, cx| {
                this.toggle_theme(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseModal, _, cx| {
                this.close_modal(cx);
            }))
            .on_action(cx.listener(|this, _: &ConfirmModal, window, cx| {
                this.confirm_modal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ModalUp, _, cx| {
                if let Some(ActiveModal::Palette(pal)) = &mut this.active_modal {
                    pal.move_selection(-1);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ModalDown, _, cx| {
                if let Some(ActiveModal::Palette(pal)) = &mut this.active_modal {
                    pal.move_selection(1);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &TogglePaletteRegex, _, cx| {
                this.toggle_palette_regex(cx);
            }))
            .child(self.render_title_bar(cx))
            .child(self.render_nav_breadcrumb_bar(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.dock_area.clone())
                    .when(!has_open_tabs, |container| {
                        container.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    v_flex()
                                        .p_6()
                                        .gap_2()
                                        .rounded_lg()
                                        .bg(cx.theme().secondary.opacity(0.45))
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .items_center()
                                        .child(render_brand_logo(52., cx))
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().foreground)
                                                .child(APP_DISPLAY_TITLE),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(APP_TAGLINE),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("⌘P Quick Open · ⌘⇧O Symbols · F12 Definition · ⇧F12 References · ⌥\\ Trigger FIM"),
                                        ),
                                ),
                        )
                    }),
            )
            .child(self.render_status_bar(cx))
            .when_some(self.active_modal.as_ref(), |root, modal| match modal {
                ActiveModal::Palette(pal) => {
                    root.child(render_palette_overlay(pal, ide_weak.clone(), cx))
                }
                ActiveModal::Settings(settings) => {
                    root.child(render_settings_overlay(settings, &self.lsp, ide_weak.clone(), cx))
                }
            })
    }
}
