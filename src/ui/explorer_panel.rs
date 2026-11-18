use crate::{
    index::{FileSummary, SymbolHit, WorkspaceIndex},
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
        tree::{TreeItem, TreeState, tree},
        v_flex,
    },
    div, prelude::FluentBuilder as _, px,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplorerTab {
    Files,
    Symbols,
}

pub struct WorkspaceExplorerPanel {
    tab: ExplorerTab,
    filter_input: Entity<InputState>,
    tree_state: Entity<TreeState>,
    index: WorkspaceIndex,
    ide: WeakEntity<KestrelIde>,
    active_file: Option<PathBuf>,
    focus: FocusHandle,
    file_meta: BTreeMap<String, FileSummary>,
    symbol_hits: Vec<SymbolHit>,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceExplorerPanel {
    pub fn new(
        index: WorkspaceIndex,
        ide: WeakEntity<KestrelIde>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files or symbols..."));
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let focus = cx.focus_handle();

        let sub = cx.subscribe_in(&filter_input, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                this.refresh(cx);
            }
        });

        let mut this = Self {
            tab: ExplorerTab::Files,
            filter_input,
            tree_state,
            index,
            ide,
            active_file: None,
            focus,
            file_meta: BTreeMap::new(),
            symbol_hits: Vec::new(),
            _subscriptions: vec![sub],
        };
        this.refresh(cx);
        this
    }

    pub fn set_active_file(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.active_file = path;
        cx.notify();
    }

    pub fn set_tab(&mut self, tab: ExplorerTab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.refresh(cx);
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let query = self.filter_input.read(cx).value().to_string();
        let files = self.index.file_entries();
        self.file_meta.clear();

        let mut filtered_files = Vec::new();
        for f in files {
            self.file_meta.insert(f.path.to_string_lossy().to_string(), f.clone());
            if query.trim().is_empty()
                || f.rel_path
                    .to_lowercase()
                    .contains(&query.trim().to_lowercase())
            {
                filtered_files.push(f);
            }
        }

        let items = build_file_tree_items(&filtered_files, !query.trim().is_empty());
        self.tree_state.update(cx, |tree, cx| {
            tree.set_items(items, cx);
        });

        if self.tab == ExplorerTab::Symbols {
            self.symbol_hits = self.index.search_symbols(&query);
        } else {
            self.symbol_hits.clear();
        }
        cx.notify();
    }
}

#[derive(Default)]
struct DirBuilder {
    dirs: BTreeMap<String, DirBuilder>,
    files: Vec<(String, PathBuf)>,
}

impl DirBuilder {
    fn insert(&mut self, parts: &[&str], full_path: PathBuf) {
        if parts.len() <= 1 {
            if let Some(name) = parts.first() {
                self.files.push(((*name).to_string(), full_path));
            }
            return;
        }
        let dir = self.dirs.entry(parts[0].to_string()).or_default();
        dir.insert(&parts[1..], full_path);
    }

    fn into_tree_items(self, prefix: &str, force_expand: bool, depth: usize) -> Vec<TreeItem> {
        let mut items = Vec::new();
        for (dir_name, child_builder) in self.dirs {
            let id = if prefix.is_empty() {
                format!("dir:{dir_name}")
            } else {
                format!("dir:{prefix}/{dir_name}")
            };
            let next_prefix = if prefix.is_empty() {
                dir_name.clone()
            } else {
                format!("{prefix}/{dir_name}")
            };
            let children = child_builder.into_tree_items(&next_prefix, force_expand, depth + 1);
            let item = TreeItem::new(id, dir_name)
                .expanded(force_expand || depth == 0)
                .children(children);
            items.push(item);
        }
        for (file_name, full_path) in self.files {
            items.push(TreeItem::new(
                full_path.to_string_lossy().to_string(),
                file_name,
            ));
        }
        items
    }
}

fn build_file_tree_items(files: &[FileSummary], force_expand: bool) -> Vec<TreeItem> {
    let mut root = DirBuilder::default();
    for f in files {
        let parts: Vec<&str> = Path::new(&f.rel_path)
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect();
        if !parts.is_empty() {
            root.insert(&parts, f.path.clone());
        }
    }
    root.into_tree_items("", force_expand, 0)
}

impl BasePanel for WorkspaceExplorerPanel {
    fn panel_name(&self) -> &'static str {
        "WorkspaceExplorerPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for WorkspaceExplorerPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (file_count, sym_count, _) = self.index.stats();
        h_flex()
            .gap_1p5()
            .items_center()
            .child(
                Icon::from(gpui_kit::assets::IconName::FolderOpen)
                    .size(px(13.))
                    .text_color(cx.theme().muted_foreground),
            )
            .child("Explorer")
            .child(
                div()
                    .px_1p5()
                    .rounded_sm()
                    .bg(cx.theme().secondary)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{file_count}f · {sym_count}s")),
            )
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for WorkspaceExplorerPanel {}

impl Focusable for WorkspaceExplorerPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for WorkspaceExplorerPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.tab;
        let ide = self.ide.clone();
        let active_path_str = self
            .active_file
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());
        let file_meta = self.file_meta.clone();

        v_flex()
            .id("explorer-panel")
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().sidebar)
            .child(
                v_flex()
                    .p_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("exp-tab-files")
                                    .label("Files")
                                    .icon(gpui_kit::assets::IconName::Folder)
                                    .xsmall()
                                    .when(tab == ExplorerTab::Files, |b| b.primary())
                                    .when(tab != ExplorerTab::Files, |b| b.ghost())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_tab(ExplorerTab::Files, cx);
                                    })),
                            )
                            .child(
                                Button::new("exp-tab-symbols")
                                    .label("Symbols")
                                    .icon(gpui_kit::assets::IconName::Braces)
                                    .xsmall()
                                    .when(tab == ExplorerTab::Symbols, |b| b.primary())
                                    .when(tab != ExplorerTab::Symbols, |b| b.ghost())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_tab(ExplorerTab::Symbols, cx);
                                    })),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new("exp-refresh")
                                    .icon(gpui_kit::assets::IconName::RefreshCw)
                                    .tooltip("Re-index workspace")
                                    .xsmall()
                                    .ghost()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let ide = this.ide.clone();
                                        cx.defer(move |cx| {
                                            _ = ide.update(cx, |ide, cx| ide.reindex_workspace(cx));
                                        });
                                    })),
                            ),
                    )
                    .child(Input::new(&self.filter_input).xsmall().cleanable(true)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .when(tab == ExplorerTab::Files, |this| {
                        this.child(
                            tree(&self.tree_state, move |ix, entry, selected, _window, cx| {
                                let is_dir = entry.is_folder();
                                let depth = entry.depth();
                                let id_str = entry.item().id.to_string();
                                let label = entry.item().label.clone();
                                let is_active =
                                    !is_dir && active_path_str.as_deref() == Some(id_str.as_str());
                                let meta = if is_dir { None } else { file_meta.get(&id_str) };
                                let ide = ide.clone();
                                let click_path = PathBuf::from(&id_str);

                                let icon_name = if is_dir {
                                    if entry.is_expanded() {
                                        gpui_kit::assets::IconName::FolderOpen
                                    } else {
                                        gpui_kit::assets::IconName::Folder
                                    }
                                } else {
                                    gpui_kit::assets::IconName::FileCode
                                };

                                ListItem::new(("tree-row", ix))
                                    .selected(selected || is_active)
                                    .py_0p5()
                                    .px_2()
                                    .child(
                                        h_flex()
                                            .pl(px((depth * 14) as f32))
                                            .w_full()
                                            .gap_1p5()
                                            .items_center()
                                            .child(
                                                Icon::from(icon_name)
                                                    .size(px(13.))
                                                    .text_color(if is_dir {
                                                        ColorName::Amber.scale(400)
                                                    } else if is_active {
                                                        cx.theme().primary
                                                    } else {
                                                        cx.theme().muted_foreground
                                                    }),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .truncate()
                                                    .text_xs()
                                                    .text_color(if is_active {
                                                        cx.theme().foreground
                                                    } else {
                                                        cx.theme().foreground
                                                    })
                                                    .child(label),
                                            )
                                            .when_some(meta, |row, info| {
                                                row.when(info.error_count > 0, |r| {
                                                    r.child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(cx.theme().danger)
                                                            .child(format!("{}!", info.error_count)),
                                                    )
                                                })
                                                .when(info.symbol_count > 0, |r| {
                                                    r.child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(cx.theme().muted_foreground)
                                                            .child(format!("{}", info.symbol_count)),
                                                    )
                                                })
                                            }),
                                    )
                                    .when(!is_dir, |item| {
                                        item.on_click(move |_, window, cx| {
                                            let path = click_path.clone();
                                            let ide = ide.clone();
                                            window.defer(cx, move |window, cx| {
                                                _ = ide.update(cx, |ide, cx| {
                                                    ide.open_file(&path, None, true, window, cx);
                                                });
                                            });
                                        })
                                    })
                            })
                            .size_full(),
                        )
                    })
                    .when(tab == ExplorerTab::Symbols, |this| {
                        this.child(
                            v_flex()
                                .id("workspace-symbols-scroll")
                                .size_full()
                                .overflow_y_scrollbar()
                                .p_1()
                                .gap_0p5()
                                .children(self.symbol_hits.iter().take(100).enumerate().map(
                                    |(ix, hit)| {
                                        let loc = hit.location.clone();
                                        let click_loc = loc.clone();
                                        let ide = self.ide.clone();
                                        let badge_color = symbol_color(loc.kind, cx);
                                        ListItem::new(("ws-sym", ix))
                                            .py_1()
                                            .px_2()
                                            .on_click(move |_, window, cx| {
                                                let loc = click_loc.clone();
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
                                                    .child(
                                                        div()
                                                            .w(px(24.))
                                                            .text_xs()
                                                            .text_color(badge_color)
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
                                                                    .child(loc.symbol_name.clone()),
                                                            )
                                                            .child(
                                                                div()
                                                                    .truncate()
                                                                    .text_xs()
                                                                    .text_color(
                                                                        cx.theme().muted_foreground,
                                                                    )
                                                                    .child(format!(
                                                                        "{}:{}",
                                                                        loc.rel_path,
                                                                        loc.start_line + 1
                                                                    )),
                                                            ),
                                                    ),
                                            )
                                    },
                                )),
                        )
                    }),
            )
    }
}
