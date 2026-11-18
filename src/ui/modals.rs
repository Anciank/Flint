use crate::{
    config::{AskAiConfig, FimConfig, FimProviderKind, LspConfig, LspServerConfig, VimConfig},
    fim::client::test_fim_connection,
    index::{FileSearchHit, NavLocation, SymbolHit, WorkspaceIndex},
    lsp::{LspManager, LspServerStatus},
    ui::{MODAL_CONTEXT, ide::KestrelIde, symbol_color},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, KeyContext,
    ParentElement as _, ScrollHandle, StatefulInteractiveElement as _, Styled as _, Subscription,
    WeakEntity, Window, point,
    component::{
        ActiveTheme as _, ColorName, Icon, Sizable as _,
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputEvent, InputState},
        list::ListItem,
        scroll::ScrollableElement as _,
        switch::Switch,
        v_flex,
    },
    div, prelude::FluentBuilder as _, px,
};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum PaletteKind {
    QuickOpen,
    WorkspaceSymbols,
    Definitions(String, Vec<NavLocation>),
}

pub struct PaletteModal {
    pub kind: PaletteKind,
    pub query_input: Entity<InputState>,
    pub selected_idx: usize,
    pub regex_mode: bool,
    pub regex_error: Option<String>,
    pub scroll_handle: ScrollHandle,
    pub scroll_padding_items: usize,
    pub file_hits: Vec<FileSearchHit>,
    pub symbol_hits: Vec<SymbolHit>,
    pub def_hits: Vec<NavLocation>,
    index: WorkspaceIndex,
    _sub: Subscription,
}

impl PaletteModal {
    pub fn new(
        kind: PaletteKind,
        index: WorkspaceIndex,
        window: &mut Window,
        cx: &mut Context<KestrelIde>,
    ) -> Self {
        let placeholder = match &kind {
            PaletteKind::QuickOpen => "Quick Open file by path...",
            PaletteKind::WorkspaceSymbols => {
                "Go to Symbol across workspace (regex: re:^fn_ or toggle .*)..."
            }
            PaletteKind::Definitions(_, _) => "Filter definitions...",
        };
        let query_input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        query_input.update(cx, |state, cx| state.focus(window, cx));

        let sub = cx.subscribe_in(&query_input, window, |ide, _, ev: &InputEvent, window, cx| {
            match ev {
                InputEvent::Change => {
                    ide.refresh_palette(cx);
                }
                InputEvent::PressEnter { .. } => {
                    ide.confirm_modal(window, cx);
                }
                _ => {}
            }
        });

        let mut modal = Self {
            kind,
            query_input,
            selected_idx: 0,
            regex_mode: false,
            regex_error: None,
            scroll_handle: ScrollHandle::new(),
            scroll_padding_items: 2,
            file_hits: Vec::new(),
            symbol_hits: Vec::new(),
            def_hits: Vec::new(),
            index,
            _sub: sub,
        };
        modal.recompute("");
        modal
    }

    pub fn recompute(&mut self, query: &str) {
        self.regex_error = None;
        match &self.kind {
            PaletteKind::QuickOpen => {
                self.file_hits = self.index.search_files(query);
                self.selected_idx = 0;
            }
            PaletteKind::WorkspaceSymbols => {
                match self.index.search_symbols_query(query, self.regex_mode) {
                    Ok(hits) => {
                        self.symbol_hits = hits;
                        self.selected_idx = 0;
                    }
                    Err(err) => {
                        self.regex_error = Some(err);
                    }
                }
            }
            PaletteKind::Definitions(_, defs) => {
                let q = query.trim().to_lowercase();
                self.def_hits = defs
                    .iter()
                    .filter(|d| {
                        q.is_empty()
                            || d.rel_path.to_lowercase().contains(&q)
                            || d.signature.to_lowercase().contains(&q)
                    })
                    .cloned()
                    .collect();
                self.selected_idx = 0;
            }
        }
        self.scroll_handle.set_offset(point(px(0.), px(0.)));
    }

    pub fn toggle_regex(&mut self, query: &str) {
        if matches!(self.kind, PaletteKind::WorkspaceSymbols) {
            self.regex_mode = !self.regex_mode;
            self.recompute(query);
        }
    }

    pub fn len(&self) -> usize {
        match &self.kind {
            PaletteKind::QuickOpen => self.file_hits.len(),
            PaletteKind::WorkspaceSymbols => self.symbol_hits.len(),
            PaletteKind::Definitions(_, _) => self.def_hits.len(),
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let count = self.len();
        if count == 0 {
            self.selected_idx = 0;
            return;
        }
        self.selected_idx = (self.selected_idx as isize + delta).rem_euclid(count as isize) as usize;
        self.scroll_selection_into_view();
    }

    pub fn scroll_selection_into_view(&self) {
        let vp = self.scroll_handle.bounds();
        let max_off = self.scroll_handle.max_offset().y;
        if vp.size.height <= px(0.) {
            self.scroll_handle.scroll_to_item(self.selected_idx);
            return;
        }
        let Some(row) = self.scroll_handle.bounds_for_item(self.selected_idx) else {
            self.scroll_handle.scroll_to_item(self.selected_idx);
            return;
        };
        let pad_items = self.scroll_padding_items as f32;
        let max_pad = ((vp.size.height - row.size.height) * 0.5).max(px(0.));
        let pad = (row.size.height * pad_items).min(max_pad);

        let mut offset = self.scroll_handle.offset();
        let vis_top = row.top() + offset.y;
        let vis_bottom = row.bottom() + offset.y;
        if vis_top < vp.top() + pad {
            offset.y = vp.top() + pad - row.top();
        } else if vis_bottom > vp.bottom() - pad {
            offset.y = vp.bottom() - pad - row.bottom();
        }
        offset.y = offset.y.clamp(-max_off, px(0.));
        self.scroll_handle.set_offset(offset);
    }

    pub fn selected_target(&self) -> Option<PaletteSelection> {
        match &self.kind {
            PaletteKind::QuickOpen => self
                .file_hits
                .get(self.selected_idx)
                .map(|f| PaletteSelection::File(f.path.clone())),
            PaletteKind::WorkspaceSymbols => self
                .symbol_hits
                .get(self.selected_idx)
                .map(|s| PaletteSelection::Location(s.location.clone())),
            PaletteKind::Definitions(_, _) => self
                .def_hits
                .get(self.selected_idx)
                .cloned()
                .map(PaletteSelection::Location),
        }
    }
}

pub enum PaletteSelection {
    File(PathBuf),
    Location(NavLocation),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Fim,
    Lsp,
    Vim,
}

pub struct SettingsModal {
    pub tab: SettingsTab,
    // FIM fields
    pub provider: FimProviderKind,
    pub enabled: bool,
    pub include_repo_context: bool,
    pub endpoint_input: Entity<InputState>,
    pub model_input: Entity<InputState>,
    pub api_key_input: Entity<InputState>,
    pub debounce_input: Entity<InputState>,
    pub max_tokens_input: Entity<InputState>,
    pub ask_ai_model_input: Entity<InputState>,
    pub test_status: Option<Result<String, String>>,
    pub testing: bool,
    // LSP fields
    pub lsp_enabled: bool,
    pub lsp_timeout_input: Entity<InputState>,
    pub lsp_servers: Vec<LspServerConfig>,
    pub selected_server_idx: usize,
    pub srv_name_input: Entity<InputState>,
    pub srv_languages_input: Entity<InputState>,
    pub srv_command_input: Entity<InputState>,
    pub srv_args_input: Entity<InputState>,
    pub srv_settings_input: Entity<InputState>,
    pub lsp_error: Option<String>,
    // Vim & Editor Display fields
    pub vim_enabled: bool,
    pub vim_system_clipboard: bool,
    pub vim_start_in_insert: bool,
    pub scroll_padding_input: Entity<InputState>,
    pub inlay_hints_enabled: bool,
    pub inline_hover_docs_enabled: bool,
}

impl SettingsModal {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tab: SettingsTab,
        fim: &FimConfig,
        ask_ai: &AskAiConfig,
        lsp: &LspConfig,
        vim: &VimConfig,
        scroll_padding: usize,
        inlay_hints: bool,
        inline_hover_docs: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let endpoint_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("http://127.0.0.1:11434")
                .default_value(fim.endpoint.clone())
        });
        let model_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("qwen2.5-coder:7b")
                .default_value(fim.model.clone())
        });
        let api_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Optional (or set DEEPSEEK_API_KEY / CODESTRAL_API_KEY / OPENAI_API_KEY)")
                .default_value(fim.api_key.clone())
        });
        let debounce_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("240")
                .default_value(fim.debounce_ms.to_string())
        });
        let max_tokens_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("128")
                .default_value(fim.max_tokens.to_string())
        });
        let ask_ai_model_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Auto (e.g. deepseek-chat / gpt-4o-mini)")
                .default_value(ask_ai.model.clone())
        });

        let lsp_timeout_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("4000")
                .default_value(lsp.request_timeout_ms.to_string())
        });

        let servers = if lsp.servers.is_empty() {
            vec![LspServerConfig::default()]
        } else {
            lsp.servers.clone()
        };
        let first = &servers[0];
        let srv_name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("rust-analyzer")
                .default_value(first.name.clone())
        });
        let srv_languages_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("rust, toml")
                .default_value(first.languages.join(", "))
        });
        let srv_command_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("rust-analyzer")
                .default_value(first.command.clone())
        });
        let srv_args_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("--stdio")
                .default_value(first.args.join(" "))
        });
        let srv_settings_json = first
            .settings
            .as_ref()
            .and_then(|v| serde_json::to_string(v).ok())
            .unwrap_or_default();
        let srv_settings_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(r#"Optional JSON settings, e.g. {"check":{"command":"clippy"}}"#)
                .default_value(srv_settings_json)
        });
        let scroll_padding_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("5")
                .default_value(scroll_padding.to_string())
        });

        Self {
            tab,
            provider: fim.provider,
            enabled: fim.enabled,
            include_repo_context: fim.include_repo_context,
            endpoint_input,
            model_input,
            api_key_input,
            debounce_input,
            max_tokens_input,
            ask_ai_model_input,
            test_status: None,
            testing: false,
            lsp_enabled: lsp.enabled,
            lsp_timeout_input,
            lsp_servers: servers,
            selected_server_idx: 0,
            srv_name_input,
            srv_languages_input,
            srv_command_input,
            srv_args_input,
            srv_settings_input,
            lsp_error: None,
            vim_enabled: vim.enabled,
            vim_system_clipboard: vim.use_system_clipboard,
            vim_start_in_insert: vim.start_in_insert,
            scroll_padding_input,
            inlay_hints_enabled: inlay_hints,
            inline_hover_docs_enabled: inline_hover_docs,
        }
    }

    pub fn flush_selected_server(&mut self, cx: &App) -> Result<(), String> {
        let Some(srv) = self.lsp_servers.get_mut(self.selected_server_idx) else {
            return Ok(());
        };
        let name = self.srv_name_input.read(cx).value().trim().to_string();
        if !name.is_empty() {
            srv.name = name;
        }
        srv.languages = self
            .srv_languages_input
            .read(cx)
            .value()
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        srv.command = self.srv_command_input.read(cx).value().trim().to_string();
        srv.args = self
            .srv_args_input
            .read(cx)
            .value()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let raw_settings = self.srv_settings_input.read(cx).value().trim().to_string();
        if raw_settings.is_empty() {
            srv.settings = None;
        } else {
            let parsed = serde_json::from_str::<serde_json::Value>(&raw_settings)
                .map_err(|e| format!("Invalid settings JSON: {e}"))?;
            srv.settings = Some(parsed);
        }
        Ok(())
    }

    pub fn select_server(&mut self, idx: usize, window: &mut Window, cx: &mut App) {
        if self.flush_selected_server(cx).is_err() {
            // Still allow switching if invalid intermediate JSON
        }
        if idx >= self.lsp_servers.len() {
            return;
        }
        self.selected_server_idx = idx;
        self.load_server_fields(window, cx);
    }

    pub fn add_custom_server(&mut self, window: &mut Window, cx: &mut App) {
        let _ = self.flush_selected_server(cx);
        let id = format!("custom_{}", self.lsp_servers.len() + 1);
        self.lsp_servers.push(LspServerConfig {
            id,
            name: "custom-lsp".to_string(),
            enabled: true,
            languages: vec!["python".to_string()],
            command: "".to_string(),
            args: vec!["--stdio".to_string()],
            ..Default::default()
        });
        self.selected_server_idx = self.lsp_servers.len() - 1;
        self.load_server_fields(window, cx);
    }

    pub fn remove_selected_server(&mut self, window: &mut Window, cx: &mut App) {
        if self.lsp_servers.len() <= 1 {
            return;
        }
        self.lsp_servers.remove(self.selected_server_idx);
        if self.selected_server_idx >= self.lsp_servers.len() {
            self.selected_server_idx = self.lsp_servers.len() - 1;
        }
        self.load_server_fields(window, cx);
    }

    fn load_server_fields(&mut self, window: &mut Window, cx: &mut App) {
        let Some(srv) = self.lsp_servers.get(self.selected_server_idx).cloned() else {
            return;
        };
        self.srv_name_input
            .update(cx, |i, cx| i.set_value(srv.name, window, cx));
        self.srv_languages_input
            .update(cx, |i, cx| i.set_value(srv.languages.join(", "), window, cx));
        self.srv_command_input
            .update(cx, |i, cx| i.set_value(srv.command, window, cx));
        self.srv_args_input
            .update(cx, |i, cx| i.set_value(srv.args.join(" "), window, cx));
        let json_str = srv
            .settings
            .as_ref()
            .and_then(|v| serde_json::to_string(v).ok())
            .unwrap_or_default();
        self.srv_settings_input
            .update(cx, |i, cx| i.set_value(json_str, window, cx));
        self.lsp_error = None;
    }

    pub fn build_fim_config(&self, base: &FimConfig, cx: &App) -> FimConfig {
        let debounce_ms = self
            .debounce_input
            .read(cx)
            .value()
            .trim()
            .parse::<u64>()
            .unwrap_or(base.debounce_ms)
            .clamp(40, 2000);
        let max_tokens = self
            .max_tokens_input
            .read(cx)
            .value()
            .trim()
            .parse::<u32>()
            .unwrap_or(base.max_tokens)
            .clamp(16, 4000);

        FimConfig {
            enabled: self.enabled,
            provider: self.provider,
            endpoint: self.endpoint_input.read(cx).value().trim().to_string(),
            model: self.model_input.read(cx).value().trim().to_string(),
            api_key: self.api_key_input.read(cx).value().to_string(),
            debounce_ms,
            max_tokens,
            include_repo_context: self.include_repo_context,
            ..base.clone()
        }
    }

    pub fn build_ask_ai_config(&self, base: &AskAiConfig, cx: &App) -> AskAiConfig {
        AskAiConfig {
            model: self.ask_ai_model_input.read(cx).value().trim().to_string(),
            ..base.clone()
        }
    }

    pub fn build_lsp_config(&mut self, base: &LspConfig, cx: &App) -> Result<LspConfig, String> {
        self.flush_selected_server(cx)?;
        let timeout_ms = self
            .lsp_timeout_input
            .read(cx)
            .value()
            .trim()
            .parse::<u64>()
            .unwrap_or(base.request_timeout_ms)
            .clamp(500, 30000);
        Ok(LspConfig {
            enabled: self.lsp_enabled,
            request_timeout_ms: timeout_ms,
            servers: self.lsp_servers.clone(),
        })
    }

    pub fn build_vim_config(&self) -> VimConfig {
        VimConfig {
            enabled: self.vim_enabled,
            use_system_clipboard: self.vim_system_clipboard,
            start_in_insert: self.vim_start_in_insert,
        }
    }

    pub fn build_display_settings(&self, fallback_padding: usize, cx: &App) -> (usize, bool, bool) {
        let padding = self
            .scroll_padding_input
            .read(cx)
            .value()
            .trim()
            .parse::<usize>()
            .unwrap_or(fallback_padding)
            .clamp(0, 20);
        (
            padding,
            self.inlay_hints_enabled,
            self.inline_hover_docs_enabled,
        )
    }
}

pub enum ActiveModal {
    Palette(PaletteModal),
    Settings(SettingsModal),
}

pub fn render_palette_overlay(
    modal: &PaletteModal,
    ide: WeakEntity<KestrelIde>,
    cx: &App,
) -> impl IntoElement {
    let title = match &modal.kind {
        PaletteKind::QuickOpen => "Quick Open File (⌘P)".to_string(),
        PaletteKind::WorkspaceSymbols => "Go to Workspace Symbol (⌘⇧O)".to_string(),
        PaletteKind::Definitions(sym, _) => format!("Definitions for `{sym}`"),
    };
    let selected_idx = modal.selected_idx;

    let mut key_ctx = KeyContext::default();
    key_ctx.add(MODAL_CONTEXT);

    div()
        .id("palette-backdrop")
        .key_context(key_ctx)
        .absolute()
        .inset_0()
        .bg(gpui_kit::black().opacity(0.45))
        .flex()
        .justify_center()
        .items_start()
        .pt(px(72.))
        .on_mouse_down(gpui_kit::MouseButton::Left, {
            let ide = ide.clone();
            move |_, _, cx| {
                let ide = ide.clone();
                cx.defer(move |cx| {
                    _ = ide.update(cx, |ide, cx| ide.close_modal(cx));
                });
            }
        })
        .child(
            v_flex()
                .id("palette-card")
                .w(px(640.))
                .max_h(px(460.))
                .rounded_lg()
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .shadow_xl()
                .overflow_hidden()
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .child(
                    v_flex()
                        .p_3()
                        .gap_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .justify_between()
                                .items_center()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("↑↓ select · ↵ open · Esc close"),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(Input::new(&modal.query_input).small().cleanable(true)),
                                )
                                .when(matches!(modal.kind, PaletteKind::WorkspaceSymbols), |row| {
                                    let ide = ide.clone();
                                    let active = modal.regex_mode;
                                    row.child(
                                        Button::new("pal-regex-toggle")
                                            .label(".*")
                                            .tooltip("Toggle Global Regex Symbol Search (⌥R / ⌘R)")
                                            .xsmall()
                                            .when(active, |b| b.primary())
                                            .when(!active, |b| b.outline())
                                            .on_click(move |_, _, cx| {
                                                let ide = ide.clone();
                                                cx.defer(move |cx| {
                                                    _ = ide.update(cx, |ide, cx| {
                                                        ide.toggle_palette_regex(cx);
                                                    });
                                                });
                                            }),
                                    )
                                }),
                        )
                        .when_some(modal.regex_error.as_ref(), |hdr, err| {
                            hdr.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().danger)
                                    .child(err.clone()),
                            )
                        }),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h(px(120.))
                        .max_h(px(340.))
                        .overflow_hidden()
                        .child(
                            v_flex()
                                .id("palette-results")
                                .w_full()
                                .max_h(px(340.))
                                .overflow_y_scroll()
                                .track_scroll(&modal.scroll_handle)
                                .p_1p5()
                                .gap_0p5()
                                .when(modal.len() == 0, |l| {
                            l.child(
                                div()
                                    .p_4()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("No matching items found."),
                            )
                        })
                        .when(matches!(modal.kind, PaletteKind::QuickOpen), |list| {
                            list.children(modal.file_hits.iter().enumerate().map(|(ix, hit)| {
                                let is_sel = ix == selected_idx;
                                let path = hit.path.clone();
                                let ide = ide.clone();
                                ListItem::new(("pal-file", ix))
                                    .selected(is_sel)
                                    .py_1p5()
                                    .px_2p5()
                                    .on_click(move |_, window, cx| {
                                        let path = path.clone();
                                        let ide = ide.clone();
                                        window.defer(cx, move |window, cx| {
                                            _ = ide.update(cx, |ide, cx| {
                                                ide.close_modal(cx);
                                                ide.open_file(&path, None, true, window, cx);
                                            });
                                        });
                                    })
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                Icon::from(gpui_kit::assets::IconName::FileCode)
                                                    .size(px(13.))
                                                    .text_color(cx.theme().muted_foreground),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .truncate()
                                                    .text_xs()
                                                    .text_color(cx.theme().foreground)
                                                    .child(hit.rel_path.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!(
                                                        "{} · {} sym",
                                                        hit.language, hit.symbol_count
                                                    )),
                                            ),
                                    )
                            }))
                        })
                        .when(matches!(modal.kind, PaletteKind::WorkspaceSymbols), |list| {
                            list.children(modal.symbol_hits.iter().enumerate().map(|(ix, hit)| {
                                let is_sel = ix == selected_idx;
                                let loc = hit.location.clone();
                                let ide = ide.clone();
                                let color = symbol_color(loc.kind, cx);
                                let click_loc = loc.clone();
                                ListItem::new(("pal-sym", ix))
                                    .selected(is_sel)
                                    .py_1p5()
                                    .px_2p5()
                                    .on_click(move |_, window, cx| {
                                        let loc = click_loc.clone();
                                        let ide = ide.clone();
                                        window.defer(cx, move |window, cx| {
                                            _ = ide.update(cx, |ide, cx| {
                                                ide.close_modal(cx);
                                                ide.jump_to_nav_location(&loc, true, window, cx);
                                            });
                                        });
                                    })
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .w(px(26.))
                                                    .text_xs()
                                                    .text_color(color)
                                                    .child(loc.kind.badge()),
                                            )
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_xs()
                                                            .text_color(cx.theme().foreground)
                                                            .child(loc.symbol_name),
                                                    )
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_xs()
                                                            .text_color(cx.theme().muted_foreground)
                                                            .child(loc.signature),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!(
                                                        "{}:{}",
                                                        loc.rel_path,
                                                        loc.start_line + 1
                                                    )),
                                            ),
                                    )
                            }))
                        })
                        .when(matches!(modal.kind, PaletteKind::Definitions(_, _)), |list| {
                            list.children(modal.def_hits.iter().enumerate().map(|(ix, loc)| {
                                let is_sel = ix == selected_idx;
                                let ide = ide.clone();
                                let color = symbol_color(loc.kind, cx);
                                let click_loc = loc.clone();
                                ListItem::new(("pal-def", ix))
                                    .selected(is_sel)
                                    .py_1p5()
                                    .px_2p5()
                                    .on_click(move |_, window, cx| {
                                        let loc = click_loc.clone();
                                        let ide = ide.clone();
                                        window.defer(cx, move |window, cx| {
                                            _ = ide.update(cx, |ide, cx| {
                                                ide.close_modal(cx);
                                                ide.jump_to_nav_location(&loc, true, window, cx);
                                            });
                                        });
                                    })
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .w(px(26.))
                                                    .text_xs()
                                                    .text_color(color)
                                                    .child(loc.kind.badge()),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .truncate()
                                                    .text_xs()
                                                    .text_color(cx.theme().foreground)
                                                    .child(loc.signature.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!(
                                                        "{}:{}",
                                                        loc.rel_path,
                                                        loc.start_line + 1
                                                    )),
                                            ),
                                    )
                            }))
                        }),
                        )
                        .vertical_scrollbar(&modal.scroll_handle),
                ),
        )
}

pub fn render_settings_overlay(
    modal: &SettingsModal,
    lsp: &LspManager,
    ide: WeakEntity<KestrelIde>,
    cx: &App,
) -> impl IntoElement {
    let mut key_ctx = KeyContext::default();
    key_ctx.add(MODAL_CONTEXT);
    let statuses = lsp.server_statuses();
    let selected_idx = modal.selected_server_idx;
    let selected_srv = modal.lsp_servers.get(selected_idx).cloned();

    div()
        .id("settings-backdrop")
        .key_context(key_ctx)
        .absolute()
        .inset_0()
        .bg(gpui_kit::black().opacity(0.45))
        .flex()
        .justify_center()
        .items_center()
        .on_mouse_down(gpui_kit::MouseButton::Left, {
            let ide = ide.clone();
            move |_, _, cx| {
                let ide = ide.clone();
                cx.defer(move |cx| {
                    _ = ide.update(cx, |ide, cx| ide.close_modal(cx));
                });
            }
        })
        .child(
            v_flex()
                .id("settings-card")
                .w(px(700.))
                .rounded_lg()
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .shadow_xl()
                .p_4()
                .gap_3()
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(crate::brand::render_brand_logo(20., cx))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().foreground)
                                        .child(format!("{} Configuration", crate::brand::APP_NAME)),
                                )
                                .child(
                                    h_flex()
                                        .ml_3()
                                        .gap_1()
                                        .child(
                                            Button::new("cfg-tab-fim")
                                                .label("AI FIM")
                                                .icon(gpui_kit::assets::IconName::Sparkles)
                                                .xsmall()
                                                .when(modal.tab == SettingsTab::Fim, |b| b.primary())
                                                .when(modal.tab != SettingsTab::Fim, |b| b.ghost())
                                                .on_click({
                                                    let ide = ide.clone();
                                                    move |_, _, cx| {
                                                        let ide = ide.clone();
                                                        cx.defer(move |cx| {
                                                            _ = ide.update(cx, |ide, cx| {
                                                                if let Some(ActiveModal::Settings(s)) =
                                                                    &mut ide.active_modal
                                                                {
                                                                    s.tab = SettingsTab::Fim;
                                                                    cx.notify();
                                                                }
                                                            });
                                                        });
                                                    }
                                                }),
                                        )
                                        .child(
                                            Button::new("cfg-tab-lsp")
                                                .label("LSP Servers")
                                                .icon(gpui_kit::assets::IconName::Cpu)
                                                .xsmall()
                                                .when(modal.tab == SettingsTab::Lsp, |b| b.primary())
                                                .when(modal.tab != SettingsTab::Lsp, |b| b.ghost())
                                                .on_click({
                                                    let ide = ide.clone();
                                                    move |_, _, cx| {
                                                        let ide = ide.clone();
                                                        cx.defer(move |cx| {
                                                            _ = ide.update(cx, |ide, cx| {
                                                                if let Some(ActiveModal::Settings(s)) =
                                                                    &mut ide.active_modal
                                                                {
                                                                    s.tab = SettingsTab::Lsp;
                                                                    cx.notify();
                                                                }
                                                            });
                                                        });
                                                    }
                                                }),
                                        )
                                        .child(
                                            Button::new("cfg-tab-vim")
                                                .label("Vim Mode")
                                                .icon(gpui_kit::assets::IconName::Terminal)
                                                .xsmall()
                                                .when(modal.tab == SettingsTab::Vim, |b| b.primary())
                                                .when(modal.tab != SettingsTab::Vim, |b| b.ghost())
                                                .on_click({
                                                    let ide = ide.clone();
                                                    move |_, _, cx| {
                                                        let ide = ide.clone();
                                                        cx.defer(move |cx| {
                                                            _ = ide.update(cx, |ide, cx| {
                                                                if let Some(ActiveModal::Settings(s)) =
                                                                    &mut ide.active_modal
                                                                {
                                                                    s.tab = SettingsTab::Vim;
                                                                    cx.notify();
                                                                }
                                                            });
                                                        });
                                                    }
                                                }),
                                        ),
                                ),
                        )
                        .child(
                            Button::new("settings-close")
                                .icon(gpui_kit::assets::IconName::X)
                                .xsmall()
                                .ghost()
                                .on_click({
                                    let ide = ide.clone();
                                    move |_, _, cx| {
                                        let ide = ide.clone();
                                        cx.defer(move |cx| {
                                            _ = ide.update(cx, |ide, cx| ide.close_modal(cx));
                                        });
                                    }
                                }),
                        ),
                )
                .when(modal.tab == SettingsTab::Fim, |card| {
                    card.child(
                        h_flex()
                            .gap_4()
                            .child(
                                Switch::new("cfg-fim-enabled")
                                    .checked(modal.enabled)
                                    .label("Enable AI FIM Inline Completion")
                                    .on_click({
                                        let ide = ide.clone();
                                        move |checked, _, cx| {
                                            let checked = *checked;
                                            let ide = ide.clone();
                                            cx.defer(move |cx| {
                                                _ = ide.update(cx, |ide, cx| {
                                                    if let Some(ActiveModal::Settings(s)) =
                                                        &mut ide.active_modal
                                                    {
                                                        s.enabled = checked;
                                                        cx.notify();
                                                    }
                                                });
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Switch::new("cfg-fim-repo-ctx")
                                    .checked(modal.include_repo_context)
                                    .label("Inject Cross-File Code Nav Context")
                                    .on_click({
                                        let ide = ide.clone();
                                        move |checked, _, cx| {
                                            let checked = *checked;
                                            let ide = ide.clone();
                                            cx.defer(move |cx| {
                                                _ = ide.update(cx, |ide, cx| {
                                                    if let Some(ActiveModal::Settings(s)) =
                                                        &mut ide.active_modal
                                                    {
                                                        s.include_repo_context = checked;
                                                        cx.notify();
                                                    }
                                                });
                                            });
                                        }
                                    }),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("FIM Provider Backend"),
                            )
                            .child(
                                h_flex().gap_1().flex_wrap().children(
                                    FimProviderKind::ALL.iter().enumerate().map(|(ix, kind)| {
                                        let kind = *kind;
                                        let selected = modal.provider == kind;
                                        let ide = ide.clone();
                                        Button::new(("cfg-prov", ix))
                                            .label(kind.short_label())
                                            .xsmall()
                                            .when(selected, |b| b.primary())
                                            .when(!selected, |b| b.outline())
                                            .on_click(move |_, window, cx| {
                                                let ide = ide.clone();
                                                window.defer(cx, move |window, cx| {
                                                    _ = ide.update(cx, |ide, cx| {
                                                        ide.select_settings_provider(
                                                            kind, window, cx,
                                                        );
                                                    });
                                                });
                                            })
                                    }),
                                ),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Endpoint URL"),
                            )
                            .child(Input::new(&modal.endpoint_input).small()),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Model"),
                                    )
                                    .child(Input::new(&modal.model_input).small()),
                            )
                            .child(
                                v_flex()
                                    .w(px(110.))
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Debounce (ms)"),
                                    )
                                    .child(Input::new(&modal.debounce_input).small()),
                            )
                            .child(
                                v_flex()
                                    .w(px(110.))
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Max Tokens"),
                                    )
                                    .child(Input::new(&modal.max_tokens_input).small()),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("API Key (optional; falls back to env vars)"),
                            )
                            .child(Input::new(&modal.api_key_input).small()),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Ask AI Chat Model (optional; reuses backend/key above)"),
                            )
                            .child(Input::new(&modal.ask_ai_model_input).small()),
                    )
                    .when_some(modal.test_status.as_ref(), |c, res| {
                        let (ok, msg) = match res {
                            Ok(s) => (true, s.clone()),
                            Err(e) => (false, e.clone()),
                        };
                        c.child(
                            div()
                                .p_2()
                                .rounded_sm()
                                .bg(cx.theme().secondary)
                                .text_xs()
                                .text_color(if ok {
                                    cx.theme().success
                                } else {
                                    cx.theme().danger
                                })
                                .child(msg),
                        )
                    })
                })
                .when(modal.tab == SettingsTab::Lsp, |card| {
                    card.child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                Switch::new("cfg-lsp-enabled")
                                    .checked(modal.lsp_enabled)
                                    .label("Enable Language Server Protocol (LSP)")
                                    .on_click({
                                        let ide = ide.clone();
                                        move |checked, _, cx| {
                                            let checked = *checked;
                                            let ide = ide.clone();
                                            cx.defer(move |cx| {
                                                _ = ide.update(cx, |ide, cx| {
                                                    if let Some(ActiveModal::Settings(s)) =
                                                        &mut ide.active_modal
                                                    {
                                                        s.lsp_enabled = checked;
                                                        cx.notify();
                                                    }
                                                });
                                            });
                                        }
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Timeout (ms)"),
                                    )
                                    .child(div().w(px(90.)).child(Input::new(&modal.lsp_timeout_input).xsmall())),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .h(px(280.))
                            // Server selector list
                            .child(
                                v_flex()
                                    .w(px(210.))
                                    .h_full()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().background)
                                    .child(
                                        h_flex()
                                            .p_1p5()
                                            .justify_between()
                                            .items_center()
                                            .border_b_1()
                                            .border_color(cx.theme().border)
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child("Servers"),
                                            )
                                            .child(
                                                Button::new("lsp-add-srv")
                                                    .label("+ Add")
                                                    .xsmall()
                                                    .ghost()
                                                    .on_click({
                                                        let ide = ide.clone();
                                                        move |_, window, cx| {
                                                            let ide = ide.clone();
                                                            window.defer(cx, move |window, cx| {
                                                                _ = ide.update(cx, |ide, cx| {
                                                                    if let Some(
                                                                        ActiveModal::Settings(s),
                                                                    ) = &mut ide.active_modal
                                                                    {
                                                                        s.add_custom_server(
                                                                            window, cx,
                                                                        );
                                                                        cx.notify();
                                                                    }
                                                                });
                                                            });
                                                        }
                                                    }),
                                            ),
                                    )
                                    .child(
                                        v_flex()
                                            .id("lsp-srv-list")
                                            .flex_1()
                                            .min_h_0()
                                            .overflow_y_scrollbar()
                                            .p_1()
                                            .gap_0p5()
                                            .children(modal.lsp_servers.iter().enumerate().map(
                                                |(ix, srv)| {
                                                    let is_sel = ix == selected_idx;
                                                    let status = statuses
                                                        .iter()
                                                        .find(|(id, _, _)| id == &srv.id)
                                                        .map(|(_, _, st)| st.clone())
                                                        .unwrap_or(LspServerStatus::Stopped);
                                                    let dot_color = match status {
                                                        LspServerStatus::Ready { .. } => {
                                                            cx.theme().success
                                                        }
                                                        LspServerStatus::Starting => {
                                                            ColorName::Amber.scale(400)
                                                        }
                                                        LspServerStatus::Unavailable(_) => {
                                                            cx.theme().danger
                                                        }
                                                        LspServerStatus::Stopped => {
                                                            cx.theme().muted_foreground
                                                        }
                                                    };
                                                    let ide = ide.clone();
                                                    ListItem::new(("lsp-srv-row", ix))
                                                        .selected(is_sel)
                                                        .py_1()
                                                        .px_2()
                                                        .on_click(move |_, window, cx| {
                                                            let ide = ide.clone();
                                                            window.defer(cx, move |window, cx| {
                                                                _ = ide.update(cx, |ide, cx| {
                                                                    if let Some(
                                                                        ActiveModal::Settings(s),
                                                                    ) = &mut ide.active_modal
                                                                    {
                                                                        s.select_server(
                                                                            ix, window, cx,
                                                                        );
                                                                        cx.notify();
                                                                    }
                                                                });
                                                            });
                                                        })
                                                        .child(
                                                            h_flex()
                                                                .w_full()
                                                                .gap_1p5()
                                                                .items_center()
                                                                .child(
                                                                    div()
                                                                        .size(px(7.))
                                                                        .rounded_full()
                                                                        .bg(dot_color),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .flex_1()
                                                                        .truncate()
                                                                        .text_xs()
                                                                        .text_color(
                                                                            if srv.enabled {
                                                                                cx.theme().foreground
                                                                            } else {
                                                                                cx.theme().muted_foreground
                                                                            },
                                                                        )
                                                                        .child(srv.name.clone()),
                                                                ),
                                                        )
                                                },
                                            )),
                                    ),
                            )
                            // Selected server editor
                            .when_some(selected_srv, |row, srv| {
                                let status_obj = statuses
                                    .iter()
                                    .find(|(id, _, _)| id == &srv.id)
                                    .map(|(_, _, st)| st.clone())
                                    .unwrap_or(LspServerStatus::Stopped);
                                let status_desc = match &status_obj {
                                    LspServerStatus::Unavailable(reason) => {
                                        format!("Unavailable: {reason}")
                                    }
                                    other => other.short_label(),
                                };
                                let srv_id = srv.id.clone();
                                row.child(
                                    v_flex()
                                        .flex_1()
                                        .h_full()
                                        .gap_2()
                                        .child(
                                            h_flex()
                                                .justify_between()
                                                .items_center()
                                                .child(
                                                    Switch::new("lsp-srv-enabled")
                                                        .checked(srv.enabled)
                                                        .label("Enabled")
                                                        .on_click({
                                                            let ide = ide.clone();
                                                            move |checked, _, cx| {
                                                                let checked = *checked;
                                                                let ide = ide.clone();
                                                                cx.defer(move |cx| {
                                                                    _ = ide.update(cx, |ide, cx| {
                                                                        if let Some(
                                                                            ActiveModal::Settings(
                                                                                s,
                                                                            ),
                                                                        ) = &mut ide.active_modal
                                                                        {
                                                                            if let Some(target) = s
                                                                                .lsp_servers
                                                                                .get_mut(s.selected_server_idx)
                                                                            {
                                                                                target.enabled =
                                                                                    checked;
                                                                                cx.notify();
                                                                            }
                                                                        }
                                                                    });
                                                                });
                                                            }
                                                        }),
                                                )
                                                .child(
                                                    h_flex()
                                                        .gap_1()
                                                        .child(
                                                            Button::new("lsp-srv-restart")
                                                                .label("Restart")
                                                                .icon(gpui_kit::assets::IconName::RefreshCw)
                                                                .xsmall()
                                                                .outline()
                                                                .on_click({
                                                                    let ide = ide.clone();
                                                                    let srv_id = srv_id.clone();
                                                                    move |_, _, cx| {
                                                                        let ide = ide.clone();
                                                                        let srv_id = srv_id.clone();
                                                                        cx.defer(move |cx| {
                                                                            _ = ide.update(cx, |ide, cx| {
                                                                                ide.lsp.restart_server(&srv_id);
                                                                                cx.notify();
                                                                            });
                                                                        });
                                                                    }
                                                                }),
                                                        )
                                                        .when(modal.lsp_servers.len() > 1, |b| {
                                                            b.child(
                                                                Button::new("lsp-srv-remove")
                                                                    .label("Remove")
                                                                    .xsmall()
                                                                    .ghost()
                                                                    .on_click({
                                                                        let ide = ide.clone();
                                                                        move |_, window, cx| {
                                                                            let ide = ide.clone();
                                                                            window.defer(cx, move |window, cx| {
                                                                                _ = ide.update(cx, |ide, cx| {
                                                                                    if let Some(ActiveModal::Settings(s)) = &mut ide.active_modal {
                                                                                        s.remove_selected_server(window, cx);
                                                                                        cx.notify();
                                                                                    }
                                                                                });
                                                                            });
                                                                        }
                                                                    }),
                                                            )
                                                        }),
                                                ),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .gap_0p5()
                                                        .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Name"))
                                                        .child(Input::new(&modal.srv_name_input).xsmall()),
                                                )
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .gap_0p5()
                                                        .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Languages (comma-separated)"))
                                                        .child(Input::new(&modal.srv_languages_input).xsmall()),
                                                ),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .gap_0p5()
                                                        .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Command"))
                                                        .child(Input::new(&modal.srv_command_input).xsmall()),
                                                )
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .gap_0p5()
                                                        .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Args (space-separated)"))
                                                        .child(Input::new(&modal.srv_args_input).xsmall()),
                                                ),
                                        )
                                        .child(
                                            v_flex()
                                                .gap_0p5()
                                                .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Settings JSON (workspace/configuration)"))
                                                .child(Input::new(&modal.srv_settings_input).xsmall()),
                                        )
                                        .child(
                                            div()
                                                .p_1p5()
                                                .rounded_sm()
                                                .bg(cx.theme().secondary.opacity(0.5))
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!("Status: {status_desc}")),
                                        ),
                                )
                            }),
                    )
                    .when_some(modal.lsp_error.as_ref(), |c, err| {
                        c.child(
                            div()
                                .p_2()
                                .rounded_sm()
                                .bg(cx.theme().secondary)
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child(err.clone()),
                        )
                    })
                })
                .when(modal.tab == SettingsTab::Vim, |card| {
                    card.child(
                        v_flex()
                            .gap_3()
                            .child(
                                Switch::new("cfg-vim-enabled")
                                    .checked(modal.vim_enabled)
                                    .label("Enable built-in Vim modal editing")
                                    .on_click({
                                        let ide = ide.clone();
                                        move |checked, _, cx| {
                                            let checked = *checked;
                                            let ide = ide.clone();
                                            cx.defer(move |cx| {
                                                _ = ide.update(cx, |ide, cx| {
                                                    if let Some(ActiveModal::Settings(s)) =
                                                        &mut ide.active_modal
                                                    {
                                                        s.vim_enabled = checked;
                                                        cx.notify();
                                                    }
                                                });
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Switch::new("cfg-vim-clipboard")
                                    .checked(modal.vim_system_clipboard)
                                    .label("Use system clipboard for yank, delete, and paste")
                                    .on_click({
                                        let ide = ide.clone();
                                        move |checked, _, cx| {
                                            let checked = *checked;
                                            let ide = ide.clone();
                                            cx.defer(move |cx| {
                                                _ = ide.update(cx, |ide, cx| {
                                                    if let Some(ActiveModal::Settings(s)) =
                                                        &mut ide.active_modal
                                                    {
                                                        s.vim_system_clipboard = checked;
                                                        cx.notify();
                                                    }
                                                });
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Switch::new("cfg-vim-insert-start")
                                    .checked(modal.vim_start_in_insert)
                                    .label("Start newly opened files in Insert mode")
                                    .on_click({
                                        let ide = ide.clone();
                                        move |checked, _, cx| {
                                            let checked = *checked;
                                            let ide = ide.clone();
                                            cx.defer(move |cx| {
                                                _ = ide.update(cx, |ide, cx| {
                                                    if let Some(ActiveModal::Settings(s)) =
                                                        &mut ide.active_modal
                                                    {
                                                        s.vim_start_in_insert = checked;
                                                        cx.notify();
                                                    }
                                                });
                                            });
                                        }
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap_4()
                                    .items_center()
                                    .child(
                                        Switch::new("cfg-inlay-hints")
                                            .checked(modal.inlay_hints_enabled)
                                            .label("Show inline inlay hints")
                                            .on_click({
                                                let ide = ide.clone();
                                                move |checked, _, cx| {
                                                    let checked = *checked;
                                                    let ide = ide.clone();
                                                    cx.defer(move |cx| {
                                                        _ = ide.update(cx, |ide, cx| {
                                                            if let Some(ActiveModal::Settings(
                                                                s,
                                                            )) = &mut ide.active_modal
                                                            {
                                                                s.inlay_hints_enabled = checked;
                                                                cx.notify();
                                                            }
                                                        });
                                                    });
                                                }
                                            }),
                                    )
                                    .child(
                                        Switch::new("cfg-inline-hover-docs")
                                            .checked(modal.inline_hover_docs_enabled)
                                            .label("Show inline hover docstrings")
                                            .on_click({
                                                let ide = ide.clone();
                                                move |checked, _, cx| {
                                                    let checked = *checked;
                                                    let ide = ide.clone();
                                                    cx.defer(move |cx| {
                                                        _ = ide.update(cx, |ide, cx| {
                                                            if let Some(ActiveModal::Settings(
                                                                s,
                                                            )) = &mut ide.active_modal
                                                            {
                                                                s.inline_hover_docs_enabled =
                                                                    checked;
                                                                cx.notify();
                                                            }
                                                        });
                                                    });
                                                }
                                            }),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child("Scroll padding (lines)"),
                                            )
                                            .child(
                                                div()
                                                    .w(px(64.))
                                                    .child(
                                                        Input::new(&modal.scroll_padding_input)
                                                            .small(),
                                                    ),
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .p_2()
                                    .rounded(px(6.))
                                    .bg(cx.theme().background)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().foreground)
                                            .child("Built-in Vim Reference"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Motions: h j k l w b e W B E 0 ^ $ gg G % { } f F t T ; , / ? n N * # Ctrl+d/u"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Edits: i I a A o O v V R x X s S D C Y r J ~ >> << d/c/y + iw/aw/i\"/i(/i{/i[ p P u Ctrl+r ."),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Code & Jump Nav: gd/gD/gy/gI/gr/gf/gi/ga/gO K Ctrl+o/Ctrl+i ]d/[d ]e/[e ]s/[s [{/]} [(/]) zz/zt/zb"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Tabs & Ex: ⌘1–9 ⌘</⌘> gt/gT/:bn/:bp :w :q :wq :<line> :def :decl :type :impl :refs :jumps :tabs :fim :ask :explain g?"),
                                    ),
                            ),
                    )
                })
                .child(
                    h_flex()
                        .justify_between()
                        .pt_1()
                        .child(
                            div().when(modal.tab == SettingsTab::Fim, |slot| {
                                slot.child(
                                    Button::new("cfg-test-conn")
                                        .label(if modal.testing {
                                            "Testing..."
                                        } else {
                                            "Test Connection"
                                        })
                                        .icon(gpui_kit::assets::IconName::Zap)
                                        .small()
                                        .outline()
                                        .loading(modal.testing)
                                        .on_click({
                                            let ide = ide.clone();
                                            move |_, _, cx| {
                                                let ide = ide.clone();
                                                cx.defer(move |cx| {
                                                    _ = ide.update(cx, |ide, cx| {
                                                        ide.run_settings_connection_test(cx);
                                                    });
                                                });
                                            }
                                        }),
                                )
                            }),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("cfg-cancel")
                                        .label("Cancel")
                                        .small()
                                        .ghost()
                                        .on_click({
                                            let ide = ide.clone();
                                            move |_, _, cx| {
                                                let ide = ide.clone();
                                                cx.defer(move |cx| {
                                                    _ = ide.update(cx, |ide, cx| {
                                                        ide.close_modal(cx);
                                                    });
                                                });
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("cfg-save")
                                        .label("Save & Apply")
                                        .icon(gpui_kit::assets::IconName::Check)
                                        .small()
                                        .primary()
                                        .on_click({
                                            let ide = ide.clone();
                                            move |_, _, cx| {
                                                let ide = ide.clone();
                                                cx.defer(move |cx| {
                                                    _ = ide.update(cx, |ide, cx| {
                                                        ide.save_settings_modal(cx);
                                                    });
                                                });
                                            }
                                        }),
                                ),
                        ),
                ),
        )
}

pub fn trigger_test_connection(cfg: FimConfig) -> Result<String, String> {
    test_fim_connection(&cfg).map_err(|e| format!("{e:#}"))
}
