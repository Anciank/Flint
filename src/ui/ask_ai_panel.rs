use crate::{
    ai::{
        AgentStepKind, AgentStepStatus, AgentTraceStep, AskAiMessage, AskAiPreset, AskAiRequest,
        AskAiStreamEvent, ChatRole, CodeContextAttachment, CodeLinkTarget,
        collect_related_symbol_notes, collect_scope_diagnostics, execute_ask_ai_streaming,
        extract_code_references, known_rel_paths_for_request, linkify_code_references,
        resolve_chat_target,
    },
    config::{AskAiConfig, FimConfig},
    index::WorkspaceIndex,
    lsp::LspManager,
    ui::{ide::KestrelIde, modals::SettingsTab},
};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window,
    component::{
        ActiveTheme as _, ColorName, Icon, Sizable as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel, PanelControl, PanelEvent},
        h_flex,
        input::{InputEvent, Textarea, TextareaState},
        scroll::ScrollableElement as _,
        text::TextView,
        v_flex,
    },
    div, prelude::FluentBuilder as _, px,
};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};

pub struct AskAiConversation {
    pub id: u64,
    pub title: String,
    pub attached_contexts: Vec<CodeContextAttachment>,
    pub include_live_selection: bool,
    pub expanded_ctx_id: Option<u64>,
    pub expanded_step_id: Option<u64>,
    pub messages: Vec<AskAiMessage>,
    pub explore_repo: bool,
    pub generating: bool,
    pub active_request_seq: u64,
    cancel_flag: Option<Arc<AtomicBool>>,
    request_task: Option<Task<()>>,
}

impl AskAiConversation {
    fn new(id: u64, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            attached_contexts: Vec::new(),
            include_live_selection: true,
            expanded_ctx_id: None,
            expanded_step_id: None,
            messages: Vec::new(),
            explore_repo: true,
            generating: false,
            active_request_seq: 0,
            cancel_flag: None,
            request_task: None,
        }
    }

    fn cancel_active_stream(&mut self) {
        self.active_request_seq = self.active_request_seq.wrapping_add(1);
        if let Some(flag) = self.cancel_flag.take() {
            flag.store(true, Ordering::Relaxed);
        }
        self.request_task = None;
        self.generating = false;
        for msg in &mut self.messages {
            if msg.streaming {
                msg.streaming = false;
                for step in &mut msg.trace_steps {
                    if step.status == AgentStepStatus::Running {
                        step.status = AgentStepStatus::Done;
                    }
                }
            }
        }
    }
}

pub struct AskAiPanel {
    index: WorkspaceIndex,
    lsp: LspManager,
    fim_config: Arc<RwLock<FimConfig>>,
    ask_config: Arc<RwLock<AskAiConfig>>,
    ide: WeakEntity<KestrelIde>,
    prompt_input: Entity<TextareaState>,
    live_selection: Option<CodeContextAttachment>,
    conversations: Vec<AskAiConversation>,
    active_conversation_id: u64,
    next_id: u64,
    next_thread_num: usize,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl AskAiPanel {
    pub fn new(
        index: WorkspaceIndex,
        lsp: LspManager,
        fim_config: Arc<RwLock<FimConfig>>,
        ask_config: Arc<RwLock<AskAiConfig>>,
        ide: WeakEntity<KestrelIde>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prompt_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 5)
                .submit_on_enter(true)
                .placeholder("Ask or explore repo... (Enter to send, ⇧Enter newline)")
        });

        let panel_ide = ide.clone();
        let input_sub =
            cx.subscribe_in(&prompt_input, window, move |_, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { secondary, shift } = ev {
                    if !shift || *secondary {
                        let ide = panel_ide.clone();
                        window.defer(cx, move |window, cx| {
                            let _ = ide.update(cx, |ide, cx| {
                                ide.submit_ask_ai_prompt(window, cx);
                            });
                        });
                    }
                }
            });

        let first_conv = AskAiConversation::new(1, "Thread 1");

        Self {
            index,
            lsp,
            fim_config,
            ask_config,
            ide,
            prompt_input,
            live_selection: None,
            conversations: vec![first_conv],
            active_conversation_id: 1,
            next_id: 2,
            next_thread_num: 2,
            focus: cx.focus_handle(),
            _subscriptions: vec![input_sub],
        }
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    pub fn conversations(&self) -> &[AskAiConversation] {
        &self.conversations
    }

    pub fn active_conversation_id(&self) -> u64 {
        self.active_conversation_id
    }

    pub fn active_conversation(&self) -> &AskAiConversation {
        self.conversations
            .iter()
            .find(|c| c.id == self.active_conversation_id)
            .unwrap_or(&self.conversations[0])
    }

    fn active_conversation_mut(&mut self) -> &mut AskAiConversation {
        let target = self.active_conversation_id;
        let idx = self
            .conversations
            .iter()
            .position(|c| c.id == target)
            .unwrap_or(0);
        &mut self.conversations[idx]
    }

    fn conversation_mut(&mut self, conv_id: u64) -> Option<&mut AskAiConversation> {
        self.conversations.iter_mut().find(|c| c.id == conv_id)
    }

    pub fn new_conversation(
        &mut self,
        title: Option<String>,
        cx: &mut Context<Self>,
    ) -> u64 {
        let id = self.alloc_id();
        let num = self.next_thread_num;
        self.next_thread_num = self.next_thread_num.saturating_add(1);
        let label = title
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("Thread {num}"));
        self.conversations.push(AskAiConversation::new(id, label));
        self.active_conversation_id = id;
        cx.notify();
        id
    }

    pub fn select_conversation(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.conversations.iter().any(|c| c.id == id) {
            self.active_conversation_id = id;
            cx.notify();
        }
    }

    pub fn fork_active_conversation(&mut self, cx: &mut Context<Self>) -> u64 {
        let src_title = self.active_conversation().title.clone();
        let src_contexts = self.active_conversation().attached_contexts.clone();
        let src_include_live = self.active_conversation().include_live_selection;
        let src_explore = self.active_conversation().explore_repo;
        let src_messages: Vec<AskAiMessage> = self
            .active_conversation()
            .messages
            .iter()
            .filter(|m| !m.streaming)
            .cloned()
            .collect();

        let id = self.alloc_id();
        self.next_thread_num = self.next_thread_num.saturating_add(1);
        let mut forked = AskAiConversation::new(id, format!("{src_title} (fork)"));
        forked.attached_contexts = src_contexts;
        forked.include_live_selection = src_include_live;
        forked.explore_repo = src_explore;
        forked.messages = src_messages;
        self.conversations.push(forked);
        self.active_conversation_id = id;
        cx.notify();
        id
    }

    pub fn rename_conversation(
        &mut self,
        id: u64,
        title: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let trimmed = title.into().trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        if let Some(conv) = self.conversation_mut(id) {
            conv.title = trimmed;
            cx.notify();
        }
    }

    pub fn close_conversation(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(pos) = self.conversations.iter().position(|c| c.id == id) else {
            return;
        };
        if self.conversations.len() == 1 {
            self.conversations[0].cancel_active_stream();
            self.conversations[0].messages.clear();
            self.conversations[0].attached_contexts.clear();
            self.conversations[0].expanded_ctx_id = None;
            self.conversations[0].expanded_step_id = None;
            self.conversations[0].title = "Thread 1".to_string();
            cx.notify();
            return;
        }
        let mut removed = self.conversations.remove(pos);
        removed.cancel_active_stream();
        if self.active_conversation_id == id {
            let next_pos = pos.min(self.conversations.len().saturating_sub(1));
            self.active_conversation_id = self.conversations[next_pos].id;
        }
        cx.notify();
    }

    pub fn messages(&self) -> &[AskAiMessage] {
        &self.active_conversation().messages
    }

    pub fn attached_contexts(&self) -> &[CodeContextAttachment] {
        &self.active_conversation().attached_contexts
    }

    pub fn live_selection(&self) -> Option<&CodeContextAttachment> {
        self.live_selection.as_ref()
    }

    pub fn is_generating(&self) -> bool {
        self.active_conversation().generating
    }

    pub fn set_explore_repo(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.active_conversation_mut().explore_repo = enabled;
        cx.notify();
    }

    pub fn focus_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_input.update(cx, |inp, cx| {
            inp.focus(window, cx);
        });
    }

    pub fn set_prompt_text(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let s: SharedString = text.into();
        self.prompt_input.update(cx, |inp, cx| {
            inp.set_value(s, window, cx);
        });
    }

    pub fn set_live_selection(
        &mut self,
        selection: Option<CodeContextAttachment>,
        cx: &mut Context<Self>,
    ) {
        let changed = match (&self.live_selection, &selection) {
            (None, None) => false,
            (Some(a), Some(b)) => !a.same_span(b),
            _ => true,
        };
        if !changed {
            return;
        }
        if selection.is_some() {
            self.active_conversation_mut().include_live_selection = true;
        }
        self.live_selection = selection.map(|mut s| {
            if s.id == 0 {
                s.id = self.alloc_id();
            }
            s
        });
        cx.notify();
    }

    pub fn attach_context(&mut self, mut ctx: CodeContextAttachment, cx: &mut Context<Self>) {
        if ctx.id == 0 {
            ctx.id = self.alloc_id();
        }
        let conv = self.active_conversation_mut();
        if conv.attached_contexts.iter().any(|existing| existing.same_span(&ctx)) {
            cx.notify();
            return;
        }
        conv.attached_contexts.push(ctx);
        if conv.attached_contexts.len() > 8 {
            conv.attached_contexts.remove(0);
        }
        cx.notify();
    }

    pub fn pin_live_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(live) = self.live_selection.clone() {
            self.attach_context(live, cx);
        }
    }

    pub fn remove_context(&mut self, id: u64, cx: &mut Context<Self>) {
        let conv = self.active_conversation_mut();
        conv.attached_contexts.retain(|c| c.id != id);
        if conv.expanded_ctx_id == Some(id) {
            conv.expanded_ctx_id = None;
        }
        cx.notify();
    }

    pub fn clear_contexts(&mut self, cx: &mut Context<Self>) {
        let conv = self.active_conversation_mut();
        conv.attached_contexts.clear();
        conv.include_live_selection = false;
        conv.expanded_ctx_id = None;
        cx.notify();
    }

    pub fn clear_messages(&mut self, cx: &mut Context<Self>) {
        let conv = self.active_conversation_mut();
        conv.cancel_active_stream();
        conv.messages.clear();
        cx.notify();
    }

    pub fn cancel_request(&mut self, cx: &mut Context<Self>) {
        self.active_conversation_mut().cancel_active_stream();
        cx.notify();
    }

    pub fn effective_contexts(
        &self,
        fallback: Option<CodeContextAttachment>,
    ) -> Vec<CodeContextAttachment> {
        let conv = self.active_conversation();
        let mut contexts = conv.attached_contexts.clone();
        if conv.include_live_selection {
            if let Some(live) = &self.live_selection {
                if !contexts.iter().any(|c| c.same_span(live)) {
                    contexts.push(live.clone());
                }
            }
        }
        if contexts.is_empty() {
            if let Some(fb) = fallback {
                contexts.push(fb);
            }
        }
        contexts
    }

    pub fn submit_input(
        &mut self,
        fallback: Option<CodeContextAttachment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let raw = self.prompt_input.read(cx).value().to_string();
        let trimmed = raw.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.prompt_input.update(cx, |inp, cx| {
            inp.set_value("", window, cx);
        });
        let include_live = self.active_conversation().include_live_selection;
        let ctx_fb = self
            .live_selection
            .clone()
            .filter(|_| include_live)
            .or(fallback);
        self.submit_preset(AskAiPreset::Custom, Some(trimmed), ctx_fb, cx);
    }

    pub fn submit_preset(
        &mut self,
        preset: AskAiPreset,
        custom_prompt: Option<String>,
        fallback_context: Option<CodeContextAttachment>,
        cx: &mut Context<Self>,
    ) {
        let conv_id = self.active_conversation_id;
        let contexts = self.effective_contexts(fallback_context);
        let user_prompt = custom_prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| preset.default_user_label())
            .to_string();

        let ask_cfg = self.ask_config.read().unwrap().clone();
        let fim_cfg = self.fim_config.read().unwrap().clone();
        let backend_label = resolve_chat_target(&ask_cfg, &fim_cfg).badge_label();
        let related_definitions = if ask_cfg.include_related_symbols {
            collect_related_symbol_notes(&contexts, &self.index, 6)
        } else {
            Vec::new()
        };
        let diagnostics_in_scope = collect_scope_diagnostics(&contexts, &self.index, &self.lsp);
        let workspace_root = self.index.root().unwrap_or_default();

        let msg_id = self.alloc_id();
        let reply_id = self.alloc_id();

        let (history, explore_repo, seq, cancel_flag) = {
            let Some(conv) = self.conversation_mut(conv_id) else {
                return;
            };
            if conv.generating {
                conv.cancel_active_stream();
            }
            let history: Vec<(ChatRole, String)> = conv
                .messages
                .iter()
                .filter(|m| !m.is_error && !m.streaming)
                .map(|m| (m.role, m.content.clone()))
                .collect();

            if conv.messages.is_empty() && conv.title.starts_with("Thread ") {
                let short = auto_thread_title(&user_prompt);
                if !short.is_empty() {
                    conv.title = short;
                }
            }

            conv.messages.push(AskAiMessage {
                id: msg_id,
                role: ChatRole::User,
                content: user_prompt.clone(),
                contexts: contexts.clone(),
                trace_steps: Vec::new(),
                citations: Vec::new(),
                streaming: false,
                provider_label: None,
                latency_ms: None,
                is_error: false,
            });
            conv.messages.push(AskAiMessage {
                id: reply_id,
                role: ChatRole::Assistant,
                content: String::new(),
                contexts: Vec::new(),
                trace_steps: Vec::new(),
                citations: Vec::new(),
                streaming: true,
                provider_label: Some(backend_label),
                latency_ms: None,
                is_error: false,
            });

            conv.active_request_seq = conv.active_request_seq.wrapping_add(1);
            conv.generating = true;
            let cancel_flag = Arc::new(AtomicBool::new(false));
            conv.cancel_flag = Some(cancel_flag.clone());
            (history, conv.explore_repo, conv.active_request_seq, cancel_flag)
        };

        let known_rel_paths =
            known_rel_paths_for_request(&workspace_root, Some(&self.index), &contexts);
        let req = AskAiRequest {
            prompt: user_prompt,
            preset,
            contexts,
            related_definitions,
            diagnostics_in_scope,
            history,
            workspace_root: workspace_root.clone(),
            index: Some(self.index.clone()),
            explore_repo,
            fim_config: fim_cfg,
            ask_config: ask_cfg,
        };

        cx.notify();

        let (tx, rx) = async_channel::unbounded::<AskAiStreamEvent>();
        let bg = cx.background_executor().clone();
        let cancel_bg = cancel_flag.clone();
        let worker_task = bg.spawn(async move { execute_ask_ai_streaming(&req, &tx, &cancel_bg) });

        let ui_task = cx.spawn(async move |this, cx| {
            let mut stream_task = Some(worker_task);
            while let Ok(ev) = rx.recv().await {
                let mut should_stop = false;
                let _ = this.update(cx, |panel, cx| {
                    let Some(conv) = panel.conversation_mut(conv_id) else {
                        should_stop = true;
                        return;
                    };
                    if conv.active_request_seq != seq {
                        should_stop = true;
                        return;
                    }
                    if let Some(msg) = conv.messages.iter_mut().find(|m| m.id == reply_id) {
                        apply_stream_event_to_message(
                            msg,
                            ev,
                            &workspace_root,
                            &known_rel_paths,
                        );
                    }
                    cx.notify();
                });
                if should_stop {
                    return;
                }
            }
            if let Some(worker) = stream_task.take() {
                let resp = worker.await;
                let _ = this.update(cx, |panel, cx| {
                    if let Some(conv) = panel.conversation_mut(conv_id) {
                        if conv.active_request_seq != seq {
                            return;
                        }
                        if let Some(msg) = conv.messages.iter_mut().find(|m| m.id == reply_id) {
                            msg.content = resp.content;
                            if !resp.trace_steps.is_empty() && msg.trace_steps.is_empty() {
                                msg.trace_steps = resp.trace_steps;
                            } else {
                                for step in &mut msg.trace_steps {
                                    if step.status == AgentStepStatus::Running {
                                        step.status = AgentStepStatus::Done;
                                    }
                                }
                            }
                            msg.citations = resp.citations;
                            msg.streaming = false;
                            msg.provider_label = Some(resp.provider_label);
                            msg.latency_ms = Some(resp.latency_ms);
                            msg.is_error = resp.is_error;
                        }
                        conv.generating = false;
                        conv.cancel_flag = None;
                        conv.request_task = None;
                        cx.notify();
                    }
                });
            }
        });

        if let Some(conv) = self.conversation_mut(conv_id) {
            if conv.active_request_seq == seq {
                conv.request_task = Some(ui_task);
            }
        }
    }

    fn backend_badge(&self) -> String {
        let ask_cfg = self.ask_config.read().unwrap().clone();
        let fim_cfg = self.fim_config.read().unwrap().clone();
        resolve_chat_target(&ask_cfg, &fim_cfg).badge_label()
    }

    fn render_context_chip(
        &self,
        ctx: &CodeContextAttachment,
        is_live: bool,
        self_ent: &WeakEntity<Self>,
        cx: &App,
    ) -> impl IntoElement {
        let ide = self.ide.clone();
        let path = ctx.path.clone();
        let jump_line = ctx.start_line.saturating_sub(1);
        let ctx_id = ctx.id;
        let expanded = self.active_conversation().expanded_ctx_id == Some(ctx_id);
        let chip_label = ctx.chip_label();
        let lines = ctx.line_count();
        let symbol_suffix = ctx
            .enclosing_symbol
            .as_deref()
            .map(|s| format!(" · {s}"))
            .unwrap_or_default();

        v_flex()
            .gap_1()
            .p_1p5()
            .rounded_sm()
            .border_1()
            .border_color(if is_live {
                ColorName::Cyan.scale(600).opacity(0.6)
            } else {
                cx.theme().border
            })
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(
                                Icon::from(gpui_kit::assets::IconName::Code)
                                    .size(px(12.))
                                    .text_color(if is_live {
                                        ColorName::Cyan.scale(400)
                                    } else {
                                        ColorName::Blue.scale(400)
                                    }),
                            )
                            .child(
                                div()
                                    .id(("ask-ctx-jump", ctx_id))
                                    .cursor_pointer()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(format!(
                                        "{}{chip_label}{symbol_suffix} ({lines}L)",
                                        if is_live { "Selection: " } else { "" }
                                    ))
                                    .on_click(move |_, window, cx| {
                                        let ide = ide.clone();
                                        let path = path.clone();
                                        window.defer(cx, move |window, cx| {
                                            _ = ide.update(cx, |ide, cx| {
                                                ide.open_file(
                                                    &path,
                                                    Some((jump_line, 0)),
                                                    true,
                                                    window,
                                                    cx,
                                                );
                                            });
                                        });
                                    }),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_0p5()
                            .items_center()
                            .child(
                                Button::new(("ask-ctx-toggle", ctx_id))
                                    .label(if expanded { "Hide" } else { "Code" })
                                    .xsmall()
                                    .ghost()
                                    .on_click({
                                        let self_ent = self_ent.clone();
                                        move |_, _, cx| {
                                            let _ = self_ent.update(cx, |panel, cx| {
                                                let conv = panel.active_conversation_mut();
                                                conv.expanded_ctx_id = if expanded {
                                                    None
                                                } else {
                                                    Some(ctx_id)
                                                };
                                                cx.notify();
                                            });
                                        }
                                    }),
                            )
                            .when(is_live, |row| {
                                row.child(
                                    Button::new(("ask-ctx-pin", ctx_id))
                                        .label("Pin")
                                        .icon(gpui_kit::assets::IconName::Plus)
                                        .xsmall()
                                        .outline()
                                        .on_click({
                                            let self_ent = self_ent.clone();
                                            move |_, _, cx| {
                                                let _ = self_ent.update(cx, |panel, cx| {
                                                    panel.pin_live_selection(cx);
                                                });
                                            }
                                        }),
                                )
                                .child(
                                    Button::new(("ask-ctx-dismiss-live", ctx_id))
                                        .icon(gpui_kit::assets::IconName::X)
                                        .xsmall()
                                        .ghost()
                                        .tooltip("Ignore current editor selection for next question")
                                        .on_click({
                                            let self_ent = self_ent.clone();
                                            move |_, _, cx| {
                                                let _ = self_ent.update(cx, |panel, cx| {
                                                    panel
                                                        .active_conversation_mut()
                                                        .include_live_selection = false;
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                )
                            })
                            .when(!is_live, |row| {
                                row.child(
                                    Button::new(("ask-ctx-del", ctx_id))
                                        .icon(gpui_kit::assets::IconName::X)
                                        .xsmall()
                                        .ghost()
                                        .on_click({
                                            let self_ent = self_ent.clone();
                                            move |_, _, cx| {
                                                let _ = self_ent.update(cx, |panel, cx| {
                                                    panel.remove_context(ctx_id, cx);
                                                });
                                            }
                                        }),
                                )
                            }),
                    ),
            )
            .when(expanded, |card| {
                let preview: String = ctx.code.lines().take(12).collect::<Vec<_>>().join("\n");
                card.child(
                    div()
                        .p_1p5()
                        .rounded_sm()
                        .bg(cx.theme().secondary.opacity(0.5))
                        .font_family("Menlo")
                        .text_xs()
                        .text_color(cx.theme().foreground)
                        .child(preview),
                )
            })
    }

    fn render_trace_step(
        &self,
        msg_id: u64,
        step: &AgentTraceStep,
        self_ent: &WeakEntity<Self>,
        cx: &App,
    ) -> impl IntoElement {
        let ide = self.ide.clone();
        let composite_id = (msg_id << 20) ^ step.id;
        let expanded =
            self.active_conversation().expanded_step_id == Some(composite_id)
                || step.status == AgentStepStatus::Running;
        let (kind_label, kind_color) = match step.kind {
            AgentStepKind::Thought => ("Thought", ColorName::Purple.scale(400)),
            AgentStepKind::ToolCall => ("Action", ColorName::Amber.scale(400)),
            AgentStepKind::ToolResult => ("Result", ColorName::Cyan.scale(400)),
        };
        let status_dot = match step.status {
            AgentStepStatus::Running => "●",
            AgentStepStatus::Done => "✓",
            AgentStepStatus::Error => "!",
        };

        v_flex()
            .p_1p5()
            .gap_1()
            .rounded_sm()
            .border_1()
            .border_color(if step.status == AgentStepStatus::Running {
                ColorName::Cyan.scale(600).opacity(0.6)
            } else {
                cx.theme().border
            })
            .bg(cx.theme().secondary.opacity(0.35))
            .child(
                h_flex()
                    .id(("ask-trace-step", composite_id))
                    .cursor_pointer()
                    .justify_between()
                    .items_center()
                    .gap_1()
                    .on_click({
                        let self_ent = self_ent.clone();
                        move |_, _, cx| {
                            let _ = self_ent.update(cx, |panel, cx| {
                                let conv = panel.active_conversation_mut();
                                conv.expanded_step_id = if expanded {
                                    None
                                } else {
                                    Some(composite_id)
                                };
                                cx.notify();
                            });
                        }
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(kind_color)
                                    .child(format!("{status_dot} {kind_label}")),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(step.title.clone()),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if expanded { "−" } else { "+" }),
                    ),
            )
            .when(!step.citations.is_empty(), |box_el| {
                box_el.child(h_flex().gap_1().flex_wrap().children(
                    step.citations.iter().take(6).enumerate().map(|(ci, cite)| {
                        let ide = ide.clone();
                        let url = cite.to_kestrel_url();
                        Button::new(("ask-step-cite", (composite_id << 8) ^ (ci as u64)))
                            .label(cite.display_label())
                            .icon(gpui_kit::assets::IconName::ArrowUpRight)
                            .xsmall()
                            .outline()
                            .on_click(move |_, window, cx| {
                                let ide = ide.clone();
                                let url = url.clone();
                                window.defer(cx, move |window, cx| {
                                    _ = ide.update(cx, |ide, cx| {
                                        ide.open_code_link(&url, window, cx);
                                    });
                                });
                            })
                    }),
                ))
            })
            .when(expanded && !step.detail.trim().is_empty(), |box_el| {
                let preview: String = step
                    .detail
                    .lines()
                    .take(18)
                    .collect::<Vec<_>>()
                    .join("\n");
                box_el.child(
                    div()
                        .p_1p5()
                        .rounded_sm()
                        .bg(cx.theme().background)
                        .font_family("Menlo")
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(preview),
                )
            })
    }

    fn render_message(
        &self,
        msg: &AskAiMessage,
        self_ent: &WeakEntity<Self>,
        cx: &App,
    ) -> impl IntoElement {
        let is_user = msg.role == ChatRole::User;
        let ide = self.ide.clone();
        let copy_content = msg.content.clone();
        let workspace_root = self.index.root().unwrap_or_default();
        let known_rel_paths = known_rel_paths_for_request(
            &workspace_root,
            Some(&self.index),
            &msg.contexts,
        );
        let linked_md = if is_user || msg.content.trim().is_empty() {
            msg.content.clone()
        } else {
            linkify_code_references(&msg.content, &workspace_root, &known_rel_paths)
        };

        v_flex()
            .w_full()
            .p_2()
            .gap_1p5()
            .rounded_md()
            .border_1()
            .border_color(if msg.is_error {
                cx.theme().danger.opacity(0.6)
            } else if msg.streaming {
                ColorName::Cyan.scale(600).opacity(0.65)
            } else if is_user {
                cx.theme().primary.opacity(0.35)
            } else {
                cx.theme().border
            })
            .bg(if is_user {
                cx.theme().primary.opacity(0.08)
            } else {
                cx.theme().background
            })
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(
                                Icon::from(if is_user {
                                    gpui_kit::assets::IconName::MessageCircleCode
                                } else {
                                    gpui_kit::assets::IconName::Sparkles
                                })
                                .size(px(12.))
                                .text_color(if is_user {
                                    cx.theme().primary
                                } else {
                                    ColorName::Cyan.scale(400)
                                }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(if is_user {
                                        "You".to_string()
                                    } else {
                                        format!("{} AI", crate::brand::APP_NAME)
                                    }),
                            )
                            .when_some(msg.provider_label.as_ref(), |row, prov| {
                                row.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("· {prov}")),
                                )
                            })
                            .when(msg.streaming, |row| {
                                row.child(
                                    div()
                                        .text_xs()
                                        .text_color(ColorName::Cyan.scale(400))
                                        .child("· Streaming..."),
                                )
                            })
                            .when_some(msg.latency_ms, |row, ms| {
                                row.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("· {ms}ms")),
                                )
                            }),
                    )
                    .when(!is_user && !msg.content.trim().is_empty(), |row| {
                        row.child(
                            Button::new(("ask-msg-copy", msg.id))
                                .label("Copy")
                                .icon(gpui_kit::assets::IconName::Copy)
                                .xsmall()
                                .ghost()
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        copy_content.clone(),
                                    ));
                                }),
                        )
                    }),
            )
            .when(!msg.contexts.is_empty(), |card| {
                card.child(
                    h_flex().gap_1().flex_wrap().children(msg.contexts.iter().map(|c| {
                        let ide = ide.clone();
                        let path = c.path.clone();
                        let line = c.start_line.saturating_sub(1);
                        let label = c.chip_label();
                        Button::new(("ask-msg-ctx", (msg.id << 16) ^ c.id))
                            .label(label)
                            .icon(gpui_kit::assets::IconName::Code)
                            .xsmall()
                            .outline()
                            .on_click(move |_, window, cx| {
                                let ide = ide.clone();
                                let path = path.clone();
                                window.defer(cx, move |window, cx| {
                                    _ = ide.update(cx, |ide, cx| {
                                        ide.open_file(&path, Some((line, 0)), true, window, cx);
                                    });
                                });
                            })
                    })),
                )
            })
            .when(!is_user && !msg.trace_steps.is_empty(), |card| {
                let count = msg.trace_steps.len();
                card.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("ReAct Repo Explore · {count} step(s)")),
                        )
                        .children(
                            msg.trace_steps
                                .iter()
                                .map(|s| self.render_trace_step(msg.id, s, self_ent, cx)),
                        ),
                )
            })
            .child(if is_user {
                div()
                    .text_xs()
                    .text_color(cx.theme().foreground)
                    .child(msg.content.clone())
                    .into_any_element()
            } else {
                let md_ide = ide.clone();
                div()
                    .w_full()
                    .text_xs()
                    .text_color(cx.theme().foreground)
                    .child(
                        TextView::markdown(
                            ("ask-ai-md", msg.id),
                            if linked_md.trim().is_empty() {
                                if msg.streaming {
                                    "_Exploring repository & streaming response..._".to_string()
                                } else {
                                    "...".to_string()
                                }
                            } else {
                                linked_md
                            },
                        )
                        .selectable(true)
                        .on_link_click(move |url, _, window, cx| {
                            let url_str = url.to_string();
                            let ide = md_ide.clone();
                            if url_str.starts_with("http://") || url_str.starts_with("https://") {
                                cx.open_url(&url_str);
                                return;
                            }
                            window.defer(cx, move |window, cx| {
                                _ = ide.update(cx, |ide, cx| {
                                    ide.open_code_link(&url_str, window, cx);
                                });
                            });
                        }),
                    )
                    .into_any_element()
            })
            .when(!is_user && !msg.citations.is_empty(), |card| {
                card.child(
                    v_flex()
                        .gap_1()
                        .pt_1()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Code Links (click to jump):"),
                        )
                        .child(h_flex().gap_1().flex_wrap().children(
                            msg.citations.iter().take(10).enumerate().map(|(ix, cite)| {
                                let ide = ide.clone();
                                let url = cite.to_kestrel_url();
                                Button::new(("ask-msg-cite", (msg.id << 12) ^ (ix as u64)))
                                    .label(cite.display_label())
                                    .icon(gpui_kit::assets::IconName::ArrowUpRight)
                                    .xsmall()
                                    .outline()
                                    .on_click(move |_, window, cx| {
                                        let ide = ide.clone();
                                        let url = url.clone();
                                        window.defer(cx, move |window, cx| {
                                            _ = ide.update(cx, |ide, cx| {
                                                ide.open_code_link(&url, window, cx);
                                            });
                                        });
                                    })
                            }),
                        )),
                )
            })
    }
}

fn auto_thread_title(prompt: &str) -> String {
    let clean = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for ch in clean.chars().take(28) {
        out.push(ch);
    }
    if clean.chars().count() > 28 {
        out.push('…');
    }
    out
}

fn apply_stream_event_to_message(
    msg: &mut AskAiMessage,
    ev: AskAiStreamEvent,
    workspace_root: &std::path::Path,
    known_rel_paths: &std::collections::HashSet<String>,
) {
    match ev {
        AskAiStreamEvent::StepAdded(step) => {
            merge_citations(&mut msg.citations, &step.citations);
            msg.trace_steps.push(step);
        }
        AskAiStreamEvent::StepUpdated {
            step_id,
            detail_append,
            replace_detail,
            status,
            citations,
        } => {
            merge_citations(&mut msg.citations, &citations);
            if let Some(step) = msg.trace_steps.iter_mut().find(|s| s.id == step_id) {
                if let Some(append) = detail_append {
                    step.detail.push_str(&append);
                }
                if let Some(rep) = replace_detail {
                    step.detail = rep;
                }
                if let Some(st) = status {
                    step.status = st;
                }
                merge_citations(&mut step.citations, &citations);
            }
        }
        AskAiStreamEvent::ContentDelta(chunk) => {
            msg.content.push_str(&chunk);
            let live_cites =
                extract_code_references(&msg.content, workspace_root, known_rel_paths, 10);
            merge_citations(&mut msg.citations, &live_cites);
        }
        AskAiStreamEvent::Finished {
            provider_label,
            latency_ms,
            is_error,
        } => {
            msg.provider_label = Some(provider_label);
            msg.latency_ms = Some(latency_ms);
            msg.is_error = is_error;
            msg.streaming = false;
        }
    }
}

fn merge_citations(dst: &mut Vec<CodeLinkTarget>, incoming: &[CodeLinkTarget]) {
    for cite in incoming {
        if !dst
            .iter()
            .any(|e| e.rel_path == cite.rel_path && e.start_line == cite.start_line)
        {
            dst.push(cite.clone());
            if dst.len() >= 12 {
                break;
            }
        }
    }
}

impl BasePanel for AskAiPanel {
    fn panel_name(&self) -> &'static str {
        "AskAiPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for AskAiPanel {
    fn title(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1p5()
            .items_center()
            .child(
                Icon::from(gpui_kit::assets::IconName::MessageCircleCode)
                    .size(px(13.))
                    .text_color(ColorName::Cyan.scale(400)),
            )
            .child("Ask AI")
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for AskAiPanel {}

impl Focusable for AskAiPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AskAiPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ide = self.ide.clone();
        let self_ent = cx.weak_entity();
        let backend_label = self.backend_badge();
        let conv = self.active_conversation();
        let active_conv_id = conv.id;
        let explore_repo = conv.explore_repo;
        let generating = conv.generating;
        let has_messages = !conv.messages.is_empty();
        let live_shown = if conv.include_live_selection {
            self.live_selection.as_ref().filter(|live| {
                !conv
                    .attached_contexts
                    .iter()
                    .any(|pinned| pinned.same_span(live))
            })
        } else {
            None
        };
        let total_context_count = conv.attached_contexts.len() + usize::from(live_shown.is_some());

        v_flex()
            .id("ask-ai-panel")
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().sidebar)
            // Top bar: Backend badge + Explore toggle + New/Fork/Settings/Clear
            .child(
                h_flex()
                    .p_2()
                    .gap_1()
                    .justify_between()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .min_w_0()
                            .overflow_hidden()
                            .child(
                                Icon::from(gpui_kit::assets::IconName::Sparkles)
                                    .size(px(13.))
                                    .text_color(ColorName::Cyan.scale(400)),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(backend_label.clone()),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(
                                Button::new("ask-ai-toggle-explore")
                                    .label(if explore_repo { "ReAct ON" } else { "ReAct OFF" })
                                    .xsmall()
                                    .when(explore_repo, |b| b.outline())
                                    .when(!explore_repo, |b| b.ghost())
                                    .tooltip("Toggle ReAct autonomous repository exploration")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let next = !this.active_conversation().explore_repo;
                                        this.set_explore_repo(next, cx);
                                    })),
                            )
                            .child(
                                Button::new("ask-ai-new-thread")
                                    .icon(gpui_kit::assets::IconName::Plus)
                                    .tooltip("New Isolated Conversation")
                                    .xsmall()
                                    .ghost()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.new_conversation(None, cx);
                                    })),
                            )
                            .child(
                                Button::new("ask-ai-fork-thread")
                                    .icon(gpui_kit::assets::IconName::GitBranch)
                                    .tooltip("Fork Active Conversation")
                                    .xsmall()
                                    .ghost()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.fork_active_conversation(cx);
                                    })),
                            )
                            .child(
                                Button::new("ask-ai-settings")
                                    .icon(gpui_kit::assets::IconName::Settings)
                                    .tooltip("Configure AI Backend (⌘,)")
                                    .xsmall()
                                    .ghost()
                                    .on_click({
                                        let ide = ide.clone();
                                        move |_, window, cx| {
                                            let ide = ide.clone();
                                            window.defer(cx, move |window, cx| {
                                                _ = ide.update(cx, |ide, cx| {
                                                    ide.open_settings_tab(
                                                        SettingsTab::Fim,
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            });
                                        }
                                    }),
                            )
                            .when(has_messages, |row| {
                                row.child(
                                    Button::new("ask-ai-clear-chat")
                                        .icon(gpui_kit::assets::IconName::Trash)
                                        .tooltip("Clear Active Thread")
                                        .xsmall()
                                        .ghost()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.clear_messages(cx);
                                        })),
                                )
                            }),
                    ),
            )
            // Conversation Isolation Thread Bar
            .child(
                h_flex()
                    .id("ask-ai-threads-bar")
                    .px_2()
                    .py_1()
                    .gap_1()
                    .items_center()
                    .overflow_x_scrollbar()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .children(self.conversations.iter().map(|c| {
                        let cid = c.id;
                        let is_active = cid == active_conv_id;
                        let count = c.messages.len() / 2;
                        let running_badge = if c.generating { "● " } else { "" };
                        let label = if count > 0 {
                            format!("{running_badge}{} ({count})", c.title)
                        } else {
                            format!("{running_badge}{}", c.title)
                        };
                        h_flex()
                            .gap_0p5()
                            .items_center()
                            .rounded_sm()
                            .border_1()
                            .border_color(if is_active {
                                ColorName::Cyan.scale(500).opacity(0.75)
                            } else {
                                cx.theme().border
                            })
                            .bg(if is_active {
                                cx.theme().background
                            } else {
                                cx.theme().secondary.opacity(0.35)
                            })
                            .child(
                                Button::new(("ask-thread-sel", cid))
                                    .label(label)
                                    .xsmall()
                                    .ghost()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select_conversation(cid, cx);
                                    })),
                            )
                            .when(self.conversations.len() > 1, |pill| {
                                pill.child(
                                    Button::new(("ask-thread-close", cid))
                                        .icon(gpui_kit::assets::IconName::X)
                                        .xsmall()
                                        .ghost()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.close_conversation(cid, cx);
                                        })),
                                )
                            })
                    })),
            )
            // Context Bar: Selected code / Pinned snippets + Capture buttons
            .child(
                v_flex()
                    .p_2()
                    .gap_1p5()
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
                                    .child(format!("THREAD CONTEXT ({total_context_count})")),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Button::new("ask-ai-add-sel")
                                            .label("Add Selection")
                                            .icon(gpui_kit::assets::IconName::Plus)
                                            .xsmall()
                                            .outline()
                                            .tooltip("Capture selected code or active symbol into this thread (⌘L)")
                                            .on_click({
                                                let ide = ide.clone();
                                                move |_, window, cx| {
                                                    let ide = ide.clone();
                                                    window.defer(cx, move |window, cx| {
                                                        _ = ide.update(cx, |ide, cx| {
                                                            ide.capture_selection_to_ask_ai(
                                                                window, cx,
                                                            );
                                                        });
                                                    });
                                                }
                                            }),
                                    )
                                    .when(total_context_count > 0, |row| {
                                        row.child(
                                            Button::new("ask-ai-clear-ctx")
                                                .label("Clear")
                                                .xsmall()
                                                .ghost()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.clear_contexts(cx);
                                                })),
                                        )
                                    }),
                            ),
                    )
                    .when(total_context_count == 0, |box_el| {
                        box_el.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Select code (⌘L) or ask any question — ReAct will explore the repo & stream clickable code links."),
                        )
                    })
                    .when_some(live_shown, |box_el, live| {
                        box_el.child(self.render_context_chip(live, true, &self_ent, cx))
                    })
                    .when(!conv.attached_contexts.is_empty(), |box_el| {
                        box_el.children(
                            conv.attached_contexts
                                .iter()
                                .map(|ctx| self.render_context_chip(ctx, false, &self_ent, cx)),
                        )
                    }),
            )
            // Quick Action Chips: Explain / Review / Tests / Fix
            .child(
                h_flex()
                    .px_2()
                    .py_1p5()
                    .gap_1()
                    .flex_wrap()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("ask-ai-explain")
                            .label("Explain")
                            .icon(gpui_kit::assets::IconName::WandSparkles)
                            .xsmall()
                            .primary()
                            .tooltip("Explain selected code or active symbol (⌘⇧E)")
                            .on_click({
                                let ide = ide.clone();
                                move |_, _, cx| {
                                    let ide = ide.clone();
                                    cx.defer(move |cx| {
                                        _ = ide.update(cx, |ide, cx| {
                                            ide.run_ask_ai_preset(AskAiPreset::Explain, cx);
                                        });
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("ask-ai-review")
                            .label("Review")
                            .icon(gpui_kit::assets::IconName::Lightbulb)
                            .xsmall()
                            .outline()
                            .on_click({
                                let ide = ide.clone();
                                move |_, _, cx| {
                                    let ide = ide.clone();
                                    cx.defer(move |cx| {
                                        _ = ide.update(cx, |ide, cx| {
                                            ide.run_ask_ai_preset(AskAiPreset::Review, cx);
                                        });
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("ask-ai-tests")
                            .label("Tests")
                            .xsmall()
                            .outline()
                            .on_click({
                                let ide = ide.clone();
                                move |_, _, cx| {
                                    let ide = ide.clone();
                                    cx.defer(move |cx| {
                                        _ = ide.update(cx, |ide, cx| {
                                            ide.run_ask_ai_preset(AskAiPreset::Tests, cx);
                                        });
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("ask-ai-fix")
                            .label("Fix")
                            .xsmall()
                            .outline()
                            .on_click({
                                let ide = ide.clone();
                                move |_, _, cx| {
                                    let ide = ide.clone();
                                    cx.defer(move |cx| {
                                        _ = ide.update(cx, |ide, cx| {
                                            ide.run_ask_ai_preset(AskAiPreset::Fix, cx);
                                        });
                                    });
                                }
                            }),
                    ),
            )
            // Conversation & Streaming Markdown Explanations
            .child(
                v_flex()
                    .id("ask-ai-messages-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .p_2()
                    .gap_2()
                    .when(conv.messages.is_empty() && !generating, |area| {
                        area.child(
                            v_flex()
                                .p_3()
                                .gap_2()
                                .rounded_md()
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().background)
                                .child(
                                    h_flex()
                                        .gap_1p5()
                                        .items_center()
                                        .child(
                                            Icon::from(gpui_kit::assets::IconName::Bot)
                                                .size(px(14.))
                                                .text_color(ColorName::Cyan.scale(400)),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().foreground)
                                                .child("Ask AI · Streaming ReAct Explorer"),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("1. Isolated conversations: switch, fork, or spin up parallel threads (+ above).\n2. ReAct explorer searches symbols, references & reads repo files live while streaming.\n3. Click any file:line link in AI output to jump directly to that line in the editor."),
                                ),
                        )
                    })
                    .children(
                        conv.messages
                            .iter()
                            .map(|m| self.render_message(m, &self_ent, cx)),
                    )
                    .when(generating, |area| {
                        area.child(
                            h_flex()
                                .p_2()
                                .justify_between()
                                .items_center()
                                .rounded_md()
                                .border_1()
                                .border_color(ColorName::Cyan.scale(600).opacity(0.6))
                                .bg(cx.theme().background)
                                .child(
                                    h_flex()
                                        .gap_1p5()
                                        .items_center()
                                        .child(
                                            Icon::from(gpui_kit::assets::IconName::Sparkles)
                                                .size(px(13.))
                                                .text_color(ColorName::Cyan.scale(400)),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().foreground)
                                                .child(format!("Streaming ({backend_label})...")),
                                        ),
                                )
                                .child(
                                    Button::new("ask-ai-stop")
                                        .label("Stop")
                                        .xsmall()
                                        .ghost()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.cancel_request(cx);
                                        })),
                                ),
                        )
                    }),
            )
            // Composer Footer
            .child(
                v_flex()
                    .p_2()
                    .gap_1p5()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(Textarea::new(&self.prompt_input).small())
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if total_context_count > 0 {
                                        format!("{total_context_count} context snippet(s) · {}", conv.title)
                                    } else {
                                        format!("Thread: {} · ReAct {}", conv.title, if explore_repo { "ON" } else { "OFF" })
                                    }),
                            )
                            .child(
                                Button::new("ask-ai-send")
                                    .label(if generating { "Streaming..." } else { "Ask AI" })
                                    .icon(gpui_kit::assets::IconName::Send)
                                    .xsmall()
                                    .primary()
                                    .loading(generating)
                                    .on_click({
                                        let ide = ide.clone();
                                        move |_, window, cx| {
                                            let ide = ide.clone();
                                            window.defer(cx, move |window, cx| {
                                                _ = ide.update(cx, |ide, cx| {
                                                    ide.submit_ask_ai_prompt(window, cx);
                                                });
                                            });
                                        }
                                    }),
                            ),
                    ),
            )
    }
}
