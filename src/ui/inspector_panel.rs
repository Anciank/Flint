use crate::{
    config::FimConfig,
    fim::{FimEngine, FimStatus},
    index::{CodeMapSnapshot, OutlineNode, WorkspaceIndex},
    ui::{ide::KestrelIde, symbol_color},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Subscription,
    WeakEntity, Window,
    component::{
        ActiveTheme as _, ColorName, Icon, Sizable as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel, PanelControl, PanelEvent},
        h_flex,
        input::{Input, InputEvent, InputState},
        list::ListItem,
        scroll::ScrollableElement as _,
        v_flex,
    },
    div, prelude::FluentBuilder as _, px,
};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorTab {
    Outline,
    CodeMap,
    FimWorkbench,
}

pub struct CodeNavAndFimPanel {
    tab: InspectorTab,
    outline_filter: Entity<InputState>,
    index: WorkspaceIndex,
    fim: FimEngine,
    fim_config: Arc<RwLock<FimConfig>>,
    ide: WeakEntity<KestrelIde>,
    active_file: Option<PathBuf>,
    active_offset: usize,
    outline: Vec<OutlineNode>,
    enclosing_idx: Option<usize>,
    code_map: Option<CodeMapSnapshot>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl CodeNavAndFimPanel {
    pub fn new(
        index: WorkspaceIndex,
        fim: FimEngine,
        fim_config: Arc<RwLock<FimConfig>>,
        ide: WeakEntity<KestrelIde>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let outline_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter outline..."));
        let sub = cx.subscribe_in(&outline_filter, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
            let _ = this;
        });

        Self {
            tab: InspectorTab::CodeMap,
            outline_filter,
            index,
            fim,
            fim_config,
            ide,
            active_file: None,
            active_offset: 0,
            outline: Vec::new(),
            enclosing_idx: None,
            code_map: None,
            focus: cx.focus_handle(),
            _subscriptions: vec![sub],
        }
    }

    pub fn current_tab(&self) -> InspectorTab {
        self.tab
    }

    pub fn set_tab(&mut self, tab: InspectorTab, cx: &mut Context<Self>) {
        self.tab = tab;
        if matches!(tab, InspectorTab::CodeMap | InspectorTab::Outline) {
            let ide = self.ide.clone();
            cx.defer(move |cx| {
                _ = ide.update(cx, |ide, cx| ide.refresh_inspector_for_active(cx));
            });
        }
        cx.notify();
    }

    pub fn update_context(
        &mut self,
        path: Option<PathBuf>,
        offset: usize,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        self.active_file = path.clone();
        self.active_offset = offset;
        if let Some(p) = &path {
            match self.tab {
                InspectorTab::Outline => {
                    self.outline = self.index.file_symbols(p);
                    let chain = self.index.enclosing_chain(p, offset);
                    self.enclosing_idx = chain.last().map(|s| s.idx);
                }
                InspectorTab::CodeMap => {
                    self.code_map = Some(self.index.code_map(p, offset, text));
                }
                InspectorTab::FimWorkbench => {}
            }
        } else {
            self.outline.clear();
            self.enclosing_idx = None;
            self.code_map = None;
        }
        cx.notify();
    }

    fn render_outline(&self, cx: &App) -> impl IntoElement {
        let query = self.outline_filter.read(cx).value().to_lowercase();
        let active_path = self.active_file.clone();
        let enclosing_idx = self.enclosing_idx;
        let ide = self.ide.clone();

        let filtered: Vec<OutlineNode> = self
            .outline
            .iter()
            .filter(|sym| {
                query.trim().is_empty() || sym.name.to_lowercase().contains(query.trim())
            })
            .cloned()
            .collect();

        v_flex()
            .size_full()
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Input::new(&self.outline_filter).xsmall().cleanable(true)),
            )
            .child(
                v_flex()
                    .id("outline-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .p_1()
                    .gap_0p5()
                    .when(filtered.is_empty(), |this| {
                        this.child(
                            div()
                                .p_4()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("No symbols indexed for this file."),
                        )
                    })
                    .children(filtered.into_iter().enumerate().map(|(ix, sym)| {
                        let is_cur = enclosing_idx == Some(sym.idx);
                        let color = symbol_color(sym.kind, cx);
                        let path = active_path.clone();
                        let ide = ide.clone();
                        ListItem::new(("outline-row", ix))
                            .selected(is_cur)
                            .py_1()
                            .px_2()
                            .on_click(move |_, window, cx| {
                                if let Some(path) = &path {
                                    let path = path.clone();
                                    let ide = ide.clone();
                                    let line = sym.start_line;
                                    let col = sym.start_col;
                                    window.defer(cx, move |window, cx| {
                                        _ = ide.update(cx, |ide, cx| {
                                            ide.open_file(&path, Some((line, col)), true, window, cx);
                                        });
                                    });
                                }
                            })
                            .child(
                                h_flex()
                                    .pl(px((sym.depth * 12) as f32))
                                    .w_full()
                                    .gap_1p5()
                                    .items_center()
                                    .child(
                                        div()
                                            .w(px(24.))
                                            .text_xs()
                                            .text_color(color)
                                            .child(sym.kind.badge()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .truncate()
                                            .text_xs()
                                            .text_color(cx.theme().foreground)
                                            .child(sym.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(":{}", sym.start_line + 1)),
                                    ),
                            )
                    })),
            )
    }

    fn render_code_map(&self, cx: &App) -> gpui_kit::AnyElement {
        let Some(map) = &self.code_map else {
            return v_flex()
                .p_4()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Open a source file to inspect its live AST Code Map.")
                .into_any_element();
        };

        let ide = self.ide.clone();
        let active_file = self.active_file.clone();

        v_flex()
            .id("code-map-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .p_2p5()
            .gap_3()
            // 1. Enclosing Symbol
            .child(
                v_flex()
                    .p_2p5()
                    .gap_1()
                    .rounded_md()
                    .bg(cx.theme().secondary.opacity(0.5))
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("ENCLOSING SCOPE"),
                    )
                    .when_some(map.enclosing.clone(), |box_el, enc| {
                        let color = symbol_color(enc.kind, cx);
                        box_el
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .items_center()
                                    .child(
                                        div()
                                            .px_1()
                                            .rounded_sm()
                                            .bg(cx.theme().background)
                                            .text_xs()
                                            .text_color(color)
                                            .child(enc.kind.badge()),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().foreground)
                                            .child(enc.name),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(enc.signature),
                            )
                            .when_some(enc.doc_comment, |b, doc| {
                                b.child(
                                    div()
                                        .pt_1()
                                        .text_xs()
                                        .text_color(cx.theme().foreground.opacity(0.85))
                                        .child(doc),
                                )
                            })
                    })
                    .when(map.enclosing.is_none(), |box_el| {
                        box_el.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Top-level file scope"),
                        )
                    }),
            )
            // 2. Cursor Identifier Definition
            .when_some(map.cursor_symbol.clone(), |panel, ident| {
                let defs = map.cursor_definitions.clone();
                let ide_ref = ide.clone();
                let active = active_file.clone();
                panel.child(
                    v_flex()
                        .p_2p5()
                        .gap_1p5()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .justify_between()
                                .items_center()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("SYMBOL `{ident}`")),
                                )
                                .child(
                                    Button::new("codemap-find-refs")
                                        .label("Find Refs")
                                        .xsmall()
                                        .ghost()
                                        .on_click(move |_, window, cx| {
                                            let ident = ident.clone();
                                            let ide = ide_ref.clone();
                                            let active = active.clone();
                                            window.defer(cx, move |window, cx| {
                                                _ = ide.update(cx, |ide, cx| {
                                                    ide.show_references_for(
                                                        &ident,
                                                        active.as_deref(),
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            });
                                        }),
                                ),
                        )
                        .when(defs.is_empty(), |v| {
                            v.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Local or external symbol (no workspace definition)"),
                            )
                        })
                        .children(defs.into_iter().take(4).enumerate().map(|(ix, loc)| {
                            let ide = ide.clone();
                            let color = symbol_color(loc.kind, cx);
                            let jump_loc = loc.clone();
                            ListItem::new(("cm-def", ix))
                                .py_1()
                                .px_1p5()
                                .on_click(move |_, window, cx| {
                                    let loc = jump_loc.clone();
                                    let ide = ide.clone();
                                    window.defer(cx, move |window, cx| {
                                        _ = ide.update(cx, |ide, cx| {
                                            ide.jump_to_nav_location(&loc, true, window, cx);
                                        });
                                    });
                                })
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap_0p5()
                                        .child(
                                            h_flex()
                                                .gap_1p5()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(color)
                                                        .child(loc.kind.badge()),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(cx.theme().foreground)
                                                        .child(format!(
                                                            "{}:{}",
                                                            loc.rel_path,
                                                            loc.start_line + 1
                                                        )),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(loc.signature),
                                        ),
                                )
                        })),
                )
            })
            // 3. Outgoing Referenced Symbols
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("OUTGOING CALLS / TYPES ({})", map.outgoing_symbols.len())),
                    )
                    .when(map.outgoing_symbols.is_empty(), |v| {
                        v.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("None resolved in this scope."),
                        )
                    })
                    .children(map.outgoing_symbols.iter().enumerate().map(|(ix, loc)| {
                        let jump = loc.clone();
                        let ide = ide.clone();
                        let color = symbol_color(loc.kind, cx);
                        ListItem::new(("cm-out", ix))
                            .py_1()
                            .px_1p5()
                            .on_click(move |_, window, cx| {
                                let loc = jump.clone();
                                let ide = ide.clone();
                                window.defer(cx, move |window, cx| {
                                    _ = ide.update(cx, |ide, cx| {
                                        ide.jump_to_nav_location(&loc, true, window, cx);
                                    });
                                });
                            })
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_1p5()
                                    .items_center()
                                    .child(div().w(px(22.)).text_xs().text_color(color).child(loc.kind.badge()))
                                    .child(
                                        div()
                                            .flex_1()
                                            .truncate()
                                            .text_xs()
                                            .text_color(cx.theme().foreground)
                                            .child(loc.symbol_name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{}:{}", loc.rel_path, loc.start_line + 1)),
                                    ),
                            )
                    })),
            )
            // 4. Incoming Callers / References
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("INCOMING REFERENCES ({})", map.incoming_references.len())),
                    )
                    .when(map.incoming_references.is_empty(), |v| {
                        v.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("No external usages found."),
                        )
                    })
                    .children(map.incoming_references.iter().take(15).enumerate().map(|(ix, hit)| {
                        let ide = ide.clone();
                        let path = hit.path.clone();
                        let line = hit.line;
                        let col = hit.col;
                        ListItem::new(("cm-in", ix))
                            .py_1()
                            .px_1p5()
                            .on_click(move |_, window, cx| {
                                let path = path.clone();
                                let ide = ide.clone();
                                window.defer(cx, move |window, cx| {
                                    _ = ide.update(cx, |ide, cx| {
                                        ide.open_file(&path, Some((line, col)), true, window, cx);
                                    });
                                });
                            })
                            .child(
                                v_flex()
                                    .w_full()
                                    .gap_0p5()
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_xs()
                                                    .text_color(cx.theme().foreground)
                                                    .child(format!("{}:{}", hit.rel_path, hit.line + 1)),
                                            )
                                            .when_some(hit.enclosing_symbol.clone(), |r, enc| {
                                                r.child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(ColorName::Blue.scale(400))
                                                        .child(format!("in {enc}")),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(hit.line_text.clone()),
                                    ),
                            )
                    })),
            )
            // 5. File Imports
            .when(!map.file_imports.is_empty(), |panel| {
                panel.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("IMPORTS ({})", map.file_imports.len())),
                        )
                        .children(map.file_imports.iter().take(12).map(|imp| {
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(cx.theme().secondary.opacity(0.35))
                                .truncate()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(imp.raw.clone())
                        })),
                )
            })
            .into_any_element()
    }

    fn render_fim_workbench(&self, cx: &App) -> impl IntoElement {
        let telemetry = self.fim.telemetry();
        let active = self.fim.active();
        let history = self.fim.history();
        let cfg = self.fim_config.read().unwrap().clone();
        let ide = self.ide.clone();

        let (status_label, status_color) = match &telemetry.status {
            FimStatus::Disabled => ("Disabled".to_string(), cx.theme().muted_foreground),
            FimStatus::Idle => ("Idle".to_string(), cx.theme().muted_foreground),
            FimStatus::Predicting => ("Predicting...".to_string(), ColorName::Amber.scale(400)),
            FimStatus::Ready => (
                format!("Ready ({}ms)", telemetry.last_latency_ms),
                cx.theme().success,
            ),
            FimStatus::Cached => ("Cached (0ms)".to_string(), ColorName::Cyan.scale(400)),
            FimStatus::Error(_) => ("Fallback".to_string(), cx.theme().warning),
        };

        v_flex()
            .id("fim-workbench-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .p_2p5()
            .gap_3()
            // Telemetry Card
            .child(
                v_flex()
                    .p_2p5()
                    .gap_2()
                    .rounded_md()
                    .bg(cx.theme().secondary.opacity(0.45))
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .items_center()
                                    .child(
                                        Icon::from(gpui_kit::assets::IconName::Sparkles)
                                            .size(px(14.))
                                            .text_color(ColorName::Cyan.scale(400)),
                                    )
                                    .child(div().text_xs().text_color(cx.theme().foreground).child(
                                        format!("{} · {}", cfg.provider.short_label(), telemetry.model_label),
                                    )),
                            )
                            .child(
                                div()
                                    .px_1p5()
                                    .py_0p5()
                                    .rounded_sm()
                                    .bg(cx.theme().background)
                                    .text_xs()
                                    .text_color(status_color)
                                    .child(status_label),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Requests: {}", telemetry.total_requests))
                            .child(format!("Cache: {}", telemetry.cache_hits))
                            .child(format!("Accepted: {}", telemetry.accepted_count)),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("fim-wb-trigger")
                                    .label("Complete (⌥\\)")
                                    .icon(gpui_kit::assets::IconName::Zap)
                                    .xsmall()
                                    .primary()
                                    .on_click({
                                        let ide = ide.clone();
                                        move |_, window, cx| {
                                            let ide = ide.clone();
                                            window.defer(cx, move |window, cx| {
                                                _ = ide.update(cx, |ide, cx| {
                                                    ide.trigger_active_fim(window, cx);
                                                });
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new("fim-wb-settings")
                                    .label("Configure")
                                    .icon(gpui_kit::assets::IconName::Settings)
                                    .xsmall()
                                    .outline()
                                    .on_click({
                                        let ide = ide.clone();
                                        move |_, window, cx| {
                                            let ide = ide.clone();
                                            window.defer(cx, move |window, cx| {
                                                _ = ide.update(cx, |ide, cx| {
                                                    ide.open_settings_modal(window, cx);
                                                });
                                            });
                                        }
                                    }),
                            ),
                    ),
            )
            // Active Suggestion Card
            .when_some(active.clone(), |panel, sug| {
                let candidate_text = sug.current_text().unwrap_or("").to_string();
                let cand_count = sug.candidates.len();
                let cand_idx = sug.selected_index + 1;
                let reason = sug
                    .candidates
                    .get(sug.selected_index)
                    .map(|c| c.reason.clone())
                    .unwrap_or_default();

                panel
                    .child(
                        v_flex()
                            .p_2p5()
                            .gap_2()
                            .rounded_md()
                            .border_1()
                            .border_color(ColorName::Cyan.scale(600).opacity(0.6))
                            .bg(cx.theme().background)
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(ColorName::Cyan.scale(400))
                                            .child(format!(
                                                "ACTIVE SUGGESTION ({cand_idx}/{cand_count}) · {reason}"
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .p_2()
                                    .rounded_sm()
                                    .bg(cx.theme().secondary.opacity(0.5))
                                    .font_family("Menlo")
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(candidate_text),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .flex_wrap()
                                    .child(
                                        Button::new("fim-accept-all")
                                            .label("Accept (Tab)")
                                            .xsmall()
                                            .primary()
                                            .on_click({
                                                let ide = ide.clone();
                                                move |_, window, cx| {
                                                    let ide = ide.clone();
                                                    window.defer(cx, move |window, cx| {
                                                        _ = ide.update(cx, |ide, cx| {
                                                            ide.accept_active_fim_full(window, cx);
                                                        });
                                                    });
                                                }
                                            }),
                                    )
                                    .child(
                                        Button::new("fim-accept-word")
                                            .label("Word (⌥→)")
                                            .xsmall()
                                            .outline()
                                            .on_click({
                                                let ide = ide.clone();
                                                move |_, window, cx| {
                                                    let ide = ide.clone();
                                                    window.defer(cx, move |window, cx| {
                                                        _ = ide.update(cx, |ide, cx| {
                                                            ide.accept_active_fim_word(window, cx);
                                                        });
                                                    });
                                                }
                                            }),
                                    )
                                    .child(
                                        Button::new("fim-accept-line")
                                            .label("Line (⌥↓)")
                                            .xsmall()
                                            .outline()
                                            .on_click({
                                                let ide = ide.clone();
                                                move |_, window, cx| {
                                                    let ide = ide.clone();
                                                    window.defer(cx, move |window, cx| {
                                                        _ = ide.update(cx, |ide, cx| {
                                                            ide.accept_active_fim_line(window, cx);
                                                        });
                                                    });
                                                }
                                            }),
                                    )
                                    .when(cand_count > 1, |row| {
                                        row.child(
                                            Button::new("fim-cycle-next")
                                                .label("Alt (⌥])")
                                                .xsmall()
                                                .ghost()
                                                .on_click({
                                                    let ide = ide.clone();
                                                    move |_, window, cx| {
                                                        let ide = ide.clone();
                                                        window.defer(cx, move |window, cx| {
                                                            _ = ide.update(cx, |ide, cx| {
                                                                ide.cycle_active_fim(1, window, cx);
                                                            });
                                                        });
                                                    }
                                                }),
                                        )
                                    }),
                            )
                            .when_some(sug.warning, |box_el, w| {
                                box_el.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().warning)
                                        .child(format!("Remote fallback note: {w}")),
                                )
                            }),
                    )
                    // Cross-File Repo Context injected into FIM
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "CROSS-FILE FIM CONTEXT ({})",
                                        sug.repo_context.len()
                                    )),
                            )
                            .when(sug.repo_context.is_empty(), |v| {
                                v.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("No external file definitions referenced near cursor."),
                                )
                            })
                            .children(sug.repo_context.into_iter().map(|ctx_item| {
                                v_flex()
                                    .p_1p5()
                                    .gap_0p5()
                                    .rounded_sm()
                                    .bg(cx.theme().secondary.opacity(0.35))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(ColorName::Cyan.scale(400))
                                            .child(format!(
                                                "{} · {}",
                                                ctx_item.rel_path, ctx_item.symbol_name
                                            )),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(ctx_item.signature),
                                    )
                            })),
                    )
                    // Prompt Window Preview
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("FIM WINDOW (PREFIX / SUFFIX)"),
                            )
                            .child(
                                div()
                                    .p_1p5()
                                    .rounded_sm()
                                    .bg(cx.theme().secondary.opacity(0.25))
                                    .font_family("Menlo")
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "PREFIX:\n{}\n\nSUFFIX:\n{}",
                                        sug.prefix_preview, sug.suffix_preview
                                    )),
                            ),
                    )
            })
            .when(active.is_none(), |panel| {
                panel.child(
                    div()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Type in the editor or press ⌥\\ to generate an inline FIM ghost completion. Tab accepts full, ⌥→ accepts next word, ⌥↓ accepts next line."),
                )
            })
            // Recent FIM History
            .when(!history.is_empty(), |panel| {
                panel.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("RECENT FIM TRACE"),
                        )
                        .children(history.into_iter().take(8).map(|h| {
                            h_flex()
                                .p_1p5()
                                .gap_2()
                                .rounded_sm()
                                .bg(cx.theme().secondary.opacity(0.25))
                                .items_center()
                                .child(
                                    div()
                                        .w(px(10.))
                                        .text_xs()
                                        .text_color(if h.accepted {
                                            cx.theme().success
                                        } else {
                                            cx.theme().muted_foreground
                                        })
                                        .child(if h.accepted { "✓" } else { "·" }),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .truncate()
                                        .text_xs()
                                        .text_color(cx.theme().foreground)
                                        .child(h.snippet),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(if h.cache_hit {
                                            format!("{}:{} · hit", h.file_name, h.line)
                                        } else {
                                            format!("{}:{} · {}ms", h.file_name, h.line, h.latency_ms)
                                        }),
                                )
                        })),
                )
            })
            .into_any_element()
    }
}

impl BasePanel for CodeNavAndFimPanel {
    fn panel_name(&self) -> &'static str {
        "CodeNavAndFimPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for CodeNavAndFimPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1p5()
            .items_center()
            .child(
                Icon::from(gpui_kit::assets::IconName::Compass)
                    .size(px(13.))
                    .text_color(cx.theme().muted_foreground),
            )
            .child("Code & FIM")
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for CodeNavAndFimPanel {}

impl Focusable for CodeNavAndFimPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for CodeNavAndFimPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.tab;

        v_flex()
            .id("inspector-panel")
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().sidebar)
            .child(
                h_flex()
                    .p_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("insp-tab-codemap")
                            .label("Code Map")
                            .icon(gpui_kit::assets::IconName::Network)
                            .xsmall()
                            .when(tab == InspectorTab::CodeMap, |b| b.primary())
                            .when(tab != InspectorTab::CodeMap, |b| b.ghost())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_tab(InspectorTab::CodeMap, cx);
                            })),
                    )
                    .child(
                        Button::new("insp-tab-outline")
                            .label("Outline")
                            .icon(gpui_kit::assets::IconName::ListTree)
                            .xsmall()
                            .when(tab == InspectorTab::Outline, |b| b.primary())
                            .when(tab != InspectorTab::Outline, |b| b.ghost())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_tab(InspectorTab::Outline, cx);
                            })),
                    )
                    .child(
                        Button::new("insp-tab-fim")
                            .label("AI FIM")
                            .icon(gpui_kit::assets::IconName::Sparkles)
                            .xsmall()
                            .when(tab == InspectorTab::FimWorkbench, |b| b.primary())
                            .when(tab != InspectorTab::FimWorkbench, |b| b.ghost())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_tab(InspectorTab::FimWorkbench, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .when(tab == InspectorTab::Outline, |this| {
                        this.child(self.render_outline(cx))
                    })
                    .when(tab == InspectorTab::CodeMap, |this| {
                        this.child(self.render_code_map(cx))
                    })
                    .when(tab == InspectorTab::FimWorkbench, |this| {
                        this.child(self.render_fim_workbench(cx))
                    }),
            )
    }
}
