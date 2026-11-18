use crate::{
    index::{ReferenceHit, TextSearchHit, WorkspaceDiagnostic, WorkspaceIndex},
    lsp::{LspManager, LspWorkspaceDiagnostic},
    ui::ide::KestrelIde,
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
use lsp_types::DiagnosticSeverity as LspSeverity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultsTab {
    References,
    Search,
    Diagnostics,
}

pub struct NavResultsPanel {
    tab: ResultsTab,
    ref_query_label: Option<String>,
    references: Vec<ReferenceHit>,
    search_input: Entity<InputState>,
    search_hits: Vec<TextSearchHit>,
    ts_diagnostics: Vec<WorkspaceDiagnostic>,
    lsp_diagnostics: Vec<LspWorkspaceDiagnostic>,
    index: WorkspaceIndex,
    lsp: LspManager,
    ide: WeakEntity<KestrelIde>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl NavResultsPanel {
    pub fn new(
        index: WorkspaceIndex,
        lsp: LspManager,
        ide: WeakEntity<KestrelIde>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search text across workspace..."));
        let sub = cx.subscribe_in(&search_input, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                let q = this.search_input.read(cx).value().to_string();
                this.search_hits = this.index.search_text(&q);
                cx.notify();
            }
        });
        let ts_diagnostics = index.all_diagnostics();
        let lsp_diagnostics = lsp.all_diagnostics();

        Self {
            tab: ResultsTab::References,
            ref_query_label: None,
            references: Vec::new(),
            search_input,
            search_hits: Vec::new(),
            ts_diagnostics,
            lsp_diagnostics,
            index,
            lsp,
            ide,
            focus: cx.focus_handle(),
            _subscriptions: vec![sub],
        }
    }

    pub fn set_tab(&mut self, tab: ResultsTab, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == ResultsTab::Diagnostics {
            self.ts_diagnostics = self.index.all_diagnostics();
            self.lsp_diagnostics = self.lsp.all_diagnostics();
        }
        cx.notify();
    }

    pub fn set_references(
        &mut self,
        symbol: String,
        hits: Vec<ReferenceHit>,
        cx: &mut Context<Self>,
    ) {
        self.ref_query_label = Some(symbol);
        self.references = hits;
        self.tab = ResultsTab::References;
        cx.notify();
    }

    pub fn refresh_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.ts_diagnostics = self.index.all_diagnostics();
        self.lsp_diagnostics = self.lsp.all_diagnostics();
        let q = self.search_input.read(cx).value().to_string();
        if !q.trim().is_empty() {
            self.search_hits = self.index.search_text(&q);
        }
        cx.notify();
    }
}

impl BasePanel for NavResultsPanel {
    fn panel_name(&self) -> &'static str {
        "NavResultsPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for NavResultsPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1p5()
            .items_center()
            .child(
                Icon::from(gpui_kit::assets::IconName::Waypoints)
                    .size(px(13.))
                    .text_color(cx.theme().muted_foreground),
            )
            .child("Navigation & Search")
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for NavResultsPanel {}

impl Focusable for NavResultsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for NavResultsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.tab;
        let ide = self.ide.clone();
        let diag_count = self.lsp_diagnostics.len() + self.ts_diagnostics.len();
        let ref_count = self.references.len();

        v_flex()
            .id("results-panel")
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().sidebar)
            .child(
                h_flex()
                    .px_2()
                    .py_1p5()
                    .gap_1p5()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .items_center()
                    .child(
                        Button::new("res-tab-refs")
                            .label(format!("References ({ref_count})"))
                            .icon(gpui_kit::assets::IconName::Crosshair)
                            .xsmall()
                            .when(tab == ResultsTab::References, |b| b.primary())
                            .when(tab != ResultsTab::References, |b| b.ghost())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_tab(ResultsTab::References, cx);
                            })),
                    )
                    .child(
                        Button::new("res-tab-search")
                            .label(format!("Search ({})", self.search_hits.len()))
                            .icon(gpui_kit::assets::IconName::Search)
                            .xsmall()
                            .when(tab == ResultsTab::Search, |b| b.primary())
                            .when(tab != ResultsTab::Search, |b| b.ghost())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_tab(ResultsTab::Search, cx);
                            })),
                    )
                    .child(
                        Button::new("res-tab-diags")
                            .label(format!("Diagnostics ({diag_count})"))
                            .icon(gpui_kit::assets::IconName::CircleDot)
                            .xsmall()
                            .when(tab == ResultsTab::Diagnostics, |b| b.primary())
                            .when(tab != ResultsTab::Diagnostics, |b| b.ghost())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_tab(ResultsTab::Diagnostics, cx);
                            })),
                    )
                    .when(tab == ResultsTab::Search, |bar| {
                        bar.child(
                            div()
                                .w(px(280.))
                                .ml_2()
                                .child(Input::new(&self.search_input).xsmall().cleanable(true)),
                        )
                    })
                    .when_some(
                        (tab == ResultsTab::References)
                            .then(|| self.ref_query_label.clone())
                            .flatten(),
                        |bar, sym| {
                            bar.child(
                                div()
                                    .ml_2()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("Symbol: `{sym}`")),
                            )
                        },
                    ),
            )
            .child(
                v_flex()
                    .id("results-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .p_1()
                    .gap_0p5()
                    .when(tab == ResultsTab::References, |list| {
                        list.when(self.references.is_empty(), |l| {
                            l.child(
                                div()
                                    .p_3()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Place cursor on any symbol and press ⇧F12 (or click References) to list all usages across the workspace."),
                            )
                        })
                        .children(self.references.iter().enumerate().map(|(ix, hit)| {
                            let ide = ide.clone();
                            let path = hit.path.clone();
                            let line = hit.line;
                            let col = hit.col;
                            let len = hit.name.len() as u32;
                            ListItem::new(("ref-hit", ix))
                                .py_1()
                                .px_2()
                                .on_click(move |_, window, cx| {
                                    let path = path.clone();
                                    let ide = ide.clone();
                                    window.defer(cx, move |window, cx| {
                                        _ = ide.update(cx, |ide, cx| {
                                            ide.open_file_selecting(
                                                &path,
                                                line,
                                                col,
                                                Some(len),
                                                true,
                                                window,
                                                cx,
                                            );
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
                                                .px_1()
                                                .rounded_sm()
                                                .bg(cx.theme().background)
                                                .text_xs()
                                                .text_color(if hit.is_definition {
                                                    ColorName::Amber.scale(400)
                                                } else if hit.is_call {
                                                    ColorName::Blue.scale(400)
                                                } else {
                                                    cx.theme().muted_foreground
                                                })
                                                .child(if hit.is_definition {
                                                    "DEF"
                                                } else if hit.is_call {
                                                    "CALL"
                                                } else {
                                                    "REF"
                                                }),
                                        )
                                        .child(
                                            div()
                                                .w(px(240.))
                                                .truncate()
                                                .text_xs()
                                                .text_color(cx.theme().foreground)
                                                .child(format!("{}:{}", hit.rel_path, hit.line + 1)),
                                        )
                                        .when_some(hit.enclosing_symbol.clone(), |r, enc| {
                                            r.child(
                                                div()
                                                    .w(px(140.))
                                                    .truncate()
                                                    .text_xs()
                                                    .text_color(ColorName::Cyan.scale(400))
                                                    .child(enc),
                                            )
                                        })
                                        .child(
                                            div()
                                                .flex_1()
                                                .truncate()
                                                .font_family("Menlo")
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(hit.line_text.clone()),
                                        ),
                                )
                        }))
                    })
                    .when(tab == ResultsTab::Search, |list| {
                        list.when(self.search_hits.is_empty(), |l| {
                            l.child(
                                div()
                                    .p_3()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Type a query above to search across all indexed workspace files."),
                            )
                        })
                        .children(self.search_hits.iter().enumerate().map(|(ix, hit)| {
                            let ide = ide.clone();
                            let path = hit.path.clone();
                            let line = hit.line;
                            let col = hit.col;
                            ListItem::new(("search-hit", ix))
                                .py_1()
                                .px_2()
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
                                    h_flex()
                                        .w_full()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            div()
                                                .w(px(240.))
                                                .truncate()
                                                .text_xs()
                                                .text_color(cx.theme().foreground)
                                                .child(format!("{}:{}", hit.rel_path, hit.line + 1)),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .truncate()
                                                .font_family("Menlo")
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(hit.line_text.clone()),
                                        ),
                                )
                        }))
                    })
                    .when(tab == ResultsTab::Diagnostics, |list| {
                        list.when(diag_count == 0, |l| {
                            l.child(
                                div()
                                    .p_3()
                                    .text_xs()
                                    .text_color(cx.theme().success)
                                    .child("No LSP or tree-sitter diagnostics detected in workspace."),
                            )
                        })
                        .children(self.lsp_diagnostics.iter().enumerate().map(|(ix, d)| {
                            let ide = ide.clone();
                            let path = d.path.clone();
                            let line = d.diagnostic.range.start.line;
                            let col = d.diagnostic.range.start.character;
                            let (sev_label, sev_color) = match d.diagnostic.severity {
                                Some(LspSeverity::ERROR) | None => ("ERR", cx.theme().danger),
                                Some(LspSeverity::WARNING) => ("WARN", cx.theme().warning),
                                Some(LspSeverity::INFORMATION) => ("INFO", ColorName::Cyan.scale(400)),
                                _ => ("HINT", cx.theme().muted_foreground),
                            };
                            let source = d
                                .diagnostic
                                .source
                                .clone()
                                .unwrap_or_else(|| "lsp".to_string());
                            ListItem::new(("lsp-diag-hit", ix))
                                .py_1()
                                .px_2()
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
                                    h_flex()
                                        .w_full()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            div()
                                                .px_1()
                                                .rounded_sm()
                                                .bg(cx.theme().background)
                                                .text_xs()
                                                .text_color(sev_color)
                                                .child(sev_label),
                                        )
                                        .child(
                                            div()
                                                .w(px(220.))
                                                .truncate()
                                                .text_xs()
                                                .text_color(cx.theme().foreground)
                                                .child(format!(
                                                    "{}:{}:{}",
                                                    d.rel_path,
                                                    line + 1,
                                                    col + 1
                                                )),
                                        )
                                        .child(
                                            div()
                                                .w(px(90.))
                                                .truncate()
                                                .text_xs()
                                                .text_color(ColorName::Cyan.scale(400))
                                                .child(source),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .truncate()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(d.diagnostic.message.clone()),
                                        ),
                                )
                        }))
                        .children(self.ts_diagnostics.iter().enumerate().map(|(ix, d)| {
                            let ide = ide.clone();
                            let path = d.path.clone();
                            let line = d.diagnostic.line;
                            let col = d.diagnostic.col;
                            ListItem::new(("ts-diag-hit", ix))
                                .py_1()
                                .px_2()
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
                                    h_flex()
                                        .w_full()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            div()
                                                .px_1()
                                                .rounded_sm()
                                                .bg(cx.theme().background)
                                                .text_xs()
                                                .text_color(cx.theme().danger)
                                                .child("SYNTAX"),
                                        )
                                        .child(
                                            div()
                                                .w(px(220.))
                                                .truncate()
                                                .text_xs()
                                                .text_color(cx.theme().foreground)
                                                .child(format!(
                                                    "{}:{}:{}",
                                                    d.rel_path,
                                                    d.diagnostic.line + 1,
                                                    d.diagnostic.col + 1
                                                )),
                                        )
                                        .child(
                                            div()
                                                .w(px(90.))
                                                .truncate()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("tree-sitter"),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .truncate()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(d.diagnostic.message.clone()),
                                        ),
                                )
                        }))
                    }),
            )
    }
}
