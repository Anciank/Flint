use gpui_kit::{
    AppContext as _, Bounds, Entity, Focusable as _, Point, TestAppContext, WindowBounds,
    WindowHandle, WindowOptions, base::Root, component::dock::DockPlacement, px, size,
    test::TestWindowExt as _,
};
use kestrel::{
    config::{FimConfig, FimProviderKind, IdeConfig, LspConfig, LspServerConfig, VimConfig},
    lsp::LspServerStatus,
    ui::{
        KestrelIde,
        modals::{ActiveModal, SettingsTab},
        register_keybindings,
    },
    vim::VimMode,
};
use std::{fs, path::Path, time::Duration};
use tempfile::TempDir;

fn setup_sample_workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"sample_app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();

    fs::write(
        src.join("models.rs"),
        r#"/// Represents an order record.
pub struct Order {
    pub id: u64,
    pub total_cents: i64,
}

pub enum OrderState {
    Pending,
    Fulfilled,
}

impl Order {
    pub fn new(id: u64, total_cents: i64) -> Self {
        Self { id, total_cents }
    }
}
"#,
    )
    .unwrap();

    fs::write(
        src.join("main.rs"),
        r#"mod models;
use models::{Order, OrderState};

fn process_order(state: OrderState) -> Order {
    let order = Order::new(1, 2500);
    order
}

fn main() {
    let _o = process_order(OrderState::Pending);
}
"#,
    )
    .unwrap();

    dir
}

fn open_kestrel(
    cx: &mut TestAppContext,
    root: &Path,
) -> (WindowHandle<Root>, Entity<KestrelIde>) {
    let mut cfg = IdeConfig::default();
    cfg.fim = FimConfig {
        provider: FimProviderKind::LocalAst,
        debounce_ms: 20,
        ..Default::default()
    };
    cfg.lsp.enabled = false;
    cfg.vim.enabled = false;
    open_kestrel_with_config(cx, root, cfg)
}

fn open_kestrel_vim(
    cx: &mut TestAppContext,
    root: &Path,
) -> (WindowHandle<Root>, Entity<KestrelIde>) {
    let mut cfg = IdeConfig::default();
    cfg.fim = FimConfig {
        provider: FimProviderKind::LocalAst,
        debounce_ms: 20,
        ..Default::default()
    };
    cfg.lsp.enabled = false;
    cfg.vim = VimConfig {
        enabled: true,
        use_system_clipboard: false,
        start_in_insert: false,
    };
    open_kestrel_with_config(cx, root, cfg)
}

fn open_kestrel_with_config(
    cx: &mut TestAppContext,
    root: &Path,
    cfg: IdeConfig,
) -> (WindowHandle<Root>, Entity<KestrelIde>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        register_keybindings(cx);
        let bounds = Bounds {
            origin: Point::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, view) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| {
                cx.new(|cx| {
                    let mut ide = KestrelIde::new(Some(root.to_path_buf()), cfg, window, cx);
                    ide.persist_config = false;
                    ide
                })
            },
        )
        .expect("open kestrel test window");
        (window.downcast::<Root>().expect("Root window"), view)
    })
}

fn settle(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn ide_cross_file_navigation_history_and_references(cx: &mut TestAppContext) {
    let ws = setup_sample_workspace();
    let (handle, ide) = open_kestrel(cx, ws.path());
    settle(cx, handle);

    // Default entry file src/main.rs should be open
    let main_path = fs::canonicalize(ws.path().join("src/main.rs")).unwrap();
    let models_path = fs::canonicalize(ws.path().join("src/models.rs")).unwrap();

    cx.update(|cx| {
        assert_eq!(ide.read(cx).active_tab.as_ref(), Some(&main_path));
        let (files, syms, _) = ide.read(cx).index.stats();
        assert!(files >= 3);
        assert!(syms >= 5);
    });

    // Place cursor on `Order` in `src/main.rs` and invoke Go to Definition
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            let tab = ide.open_tabs.get(&main_path).unwrap().clone();
            let text = tab.read(cx).editor.read(cx).value().to_string();
            let offset = text.find("Order::new").unwrap() + 2;
            tab.update(cx, |t, cx| {
                t.editor.update(cx, |ed, cx| {
                    ed.set_selected_range(offset..offset, cx);
                });
            });
            ide.go_to_definition_at_cursor(window, cx);
        });
    })
    .unwrap();
    settle(cx, handle);

    cx.update(|cx| {
        let ide_ref = ide.read(cx);
        assert_eq!(ide_ref.active_tab.as_ref(), Some(&models_path));
        assert!(ide_ref.can_go_back());
    });

    // Navigate back to src/main.rs, then forward to src/models.rs
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| ide.navigate_back(window, cx));
    })
    .unwrap();
    settle(cx, handle);
    cx.update(|cx| {
        assert_eq!(ide.read(cx).active_tab.as_ref(), Some(&main_path));
        assert!(ide.read(cx).can_go_forward());
    });

    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| ide.navigate_forward(window, cx));
    })
    .unwrap();
    settle(cx, handle);
    cx.update(|cx| {
        assert_eq!(ide.read(cx).active_tab.as_ref(), Some(&models_path));
    });

    // Find References should open bottom dock and list cross-file usages of `Order`
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| ide.find_references_at_cursor(window, cx));
    })
    .unwrap();
    settle(cx, handle);
    cx.update(|cx| {
        let ide_ref = ide.read(cx);
        assert!(ide_ref.dock_area.read(cx).is_dock_open(DockPlacement::Bottom));
    });
}

#[gpui_kit::test]
fn ide_ai_fim_ghost_completion_partial_and_tab_accept(cx: &mut TestAppContext) {
    let ws = setup_sample_workspace();
    let (handle, ide) = open_kestrel(cx, ws.path());
    settle(cx, handle);

    let main_path = fs::canonicalize(ws.path().join("src/main.rs")).unwrap();

    // Put cursor in `let x = Order {` in src/main.rs and type `{`
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            let tab = ide.open_tabs.get(&main_path).unwrap().clone();
            tab.update(cx, |t, cx| {
                t.editor.update(cx, |ed, cx| {
                    ed.set_value(
                        "mod models;\nuse models::Order;\nfn build() -> Order {\n    Order \n}\n",
                        window,
                        cx,
                    );
                    let pos = ed.value().find("Order \n").unwrap() + "Order ".len();
                    ed.set_selected_range(pos..pos, cx);
                    ed.focus(window, cx);
                });
            });
        });
        window.input("{", cx);
    })
    .unwrap();

    // Advance clock past 20ms FIM debounce and settle async FIM completion
    cx.background_executor
        .advance_clock(Duration::from_millis(50));
    settle(cx, handle);

    // Check FIM active suggestion has struct fields and cross-file repo context from models.rs
    cx.update(|cx| {
        let active = ide.read(cx).fim.active().expect("expected active FIM suggestion");
        let text = active.current_text().expect("expected FIM text");
        assert!(text.contains("id,"), "expected `id,` in `{text}`");
        assert!(
            text.contains("total_cents,"),
            "expected `total_cents,` in `{text}`"
        );
        assert!(
            active
                .repo_context
                .iter()
                .any(|s| s.symbol_name == "Order"),
            "expected cross-file Order context in FIM"
        );
    });

    // Accept next word (partial acceptance), verify buffer grows and remainder stays active
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            assert!(ide.accept_active_fim_word(window, cx));
        });
    })
    .unwrap();
    cx.background_executor
        .advance_clock(Duration::from_millis(50));
    settle(cx, handle);

    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap().clone();
        let val = tab.read(cx).editor.read(cx).value().to_string();
        assert!(val.contains("id"), "expected partial word inserted in `{val}`");
    });

    // Accept full remaining suggestion via Tab key
    cx.update_window(handle.into(), |_, window, cx| {
        window.press("tab", cx);
    })
    .unwrap();
    settle(cx, handle);

    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap().clone();
        let val = tab.read(cx).editor.read(cx).value().to_string();
        assert!(
            val.contains("total_cents,"),
            "expected Tab to accept full remaining FIM text in `{val}`"
        );
    });
}

#[gpui_kit::test]
fn ide_quick_open_symbol_modal_and_settings_modal(cx: &mut TestAppContext) {
    let ws = setup_sample_workspace();
    let (handle, ide) = open_kestrel(cx, ws.path());
    settle(cx, handle);

    let models_path = fs::canonicalize(ws.path().join("src/models.rs")).unwrap();

    // Quick Open -> filter "models" -> confirm
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| ide.open_quick_open(window, cx));
    })
    .unwrap();
    settle(cx, handle);

    cx.update_window(handle.into(), |_, window, cx| {
        window.input("models", cx);
    })
    .unwrap();
    settle(cx, handle);

    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| ide.confirm_modal(window, cx));
    })
    .unwrap();
    settle(cx, handle);

    cx.update(|cx| {
        assert_eq!(ide.read(cx).active_tab.as_ref(), Some(&models_path));
        assert!(ide.read(cx).active_modal.is_none());
    });

    // Go to Symbol -> filter "process_order" -> confirm jumps back to main.rs
    let main_path = fs::canonicalize(ws.path().join("src/main.rs")).unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| ide.open_go_to_symbol(window, cx));
    })
    .unwrap();
    settle(cx, handle);

    cx.update_window(handle.into(), |_, window, cx| {
        window.input("process_order", cx);
    })
    .unwrap();
    settle(cx, handle);

    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| ide.confirm_modal(window, cx));
    })
    .unwrap();
    settle(cx, handle);

    cx.update(|cx| {
        assert_eq!(ide.read(cx).active_tab.as_ref(), Some(&main_path));
    });

    // Settings modal -> switch provider to Ollama -> save
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            ide.open_settings_modal(window, cx);
            ide.select_settings_provider(FimProviderKind::Ollama, window, cx);
            if let Some(ActiveModal::Settings(s)) = &mut ide.active_modal {
                s.include_repo_context = false;
            }
            ide.save_settings_modal(cx);
        });
    })
    .unwrap();
    settle(cx, handle);

    cx.update(|cx| {
        let cfg = ide.read(cx).fim_config.read().unwrap().clone();
        assert_eq!(cfg.provider, FimProviderKind::Ollama);
        assert!(!cfg.include_repo_context);
        assert!(ide.read(cx).active_modal.is_none());
    });
}

#[gpui_kit::test]
fn ide_lsp_config_diagnostics_and_navigation(cx: &mut TestAppContext) {
    let ws = setup_sample_workspace();
    let models_path = fs::canonicalize(ws.path().join("src/models.rs")).unwrap();
    let main_path = fs::canonicalize(ws.path().join("src/main.rs")).unwrap();
    let models_uri = format!("file://{}", models_path.display());

    let script = ws.path().join("ui_mock_lsp.py");
    fs::write(
        &script,
        format!(
            r#"
import json, sys

def read_msg():
    headers = {{}}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        s = line.decode("utf-8").strip()
        if not s:
            break
        if ":" in s:
            k, v = s.split(":", 1)
            headers[k.strip().lower()] = v.strip()
    length = int(headers.get("content-length", "0"))
    return json.loads(sys.stdin.buffer.read(length).decode("utf-8"))

def send_msg(obj):
    raw = json.dumps(obj).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {{len(raw)}}\r\n\r\n".encode("ascii"))
    sys.stdout.buffer.write(raw)
    sys.stdout.buffer.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method")
    msg_id = msg.get("id")
    if method == "initialize":
        send_msg({{
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {{
                "capabilities": {{
                    "textDocumentSync": 1,
                    "definitionProvider": True,
                    "hoverProvider": True
                }},
                "serverInfo": {{"name": "ui-mock-lsp", "version": "1.0"}}
            }}
        }})
    elif method == "textDocument/didOpen":
        uri = msg["params"]["textDocument"]["uri"]
        send_msg({{
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {{
                "uri": uri,
                "diagnostics": [{{
                    "range": {{"start": {{"line": 1, "character": 0}}, "end": {{"line": 1, "character": 4}}}},
                    "severity": 1,
                    "source": "ui-mock-lsp",
                    "message": "UI mock LSP error"
                }}]
            }}
        }})
    elif method == "textDocument/definition":
        send_msg({{
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": [{{
                "uri": "{models_uri}",
                "range": {{"start": {{"line": 1, "character": 11}}, "end": {{"line": 1, "character": 16}}}}
            }}]
        }})
    elif method == "shutdown":
        send_msg({{"jsonrpc": "2.0", "id": msg_id, "result": None}})
    elif method == "exit":
        break
"#
        ),
    )
    .unwrap();

    let mut cfg = IdeConfig::default();
    cfg.fim = FimConfig {
        provider: FimProviderKind::LocalAst,
        debounce_ms: 20,
        ..Default::default()
    };
    cfg.lsp = LspConfig {
        enabled: true,
        request_timeout_ms: 3000,
        servers: vec![LspServerConfig {
            id: "mock-rust".to_string(),
            name: "ui-mock-lsp".to_string(),
            enabled: true,
            languages: vec!["rust".to_string()],
            command: "python3".to_string(),
            args: vec![script.to_string_lossy().to_string()],
            ..Default::default()
        }],
    };

    let (handle, ide) = open_kestrel_with_config(cx, ws.path(), cfg);

    // Wait for mock LSP server initialization and diagnostic publication
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    loop {
        settle(cx, handle);
        cx.background_executor
            .advance_clock(Duration::from_millis(65));
        settle(cx, handle);
        let ready = cx.update(|cx| {
            let ide_ref = ide.read(cx);
            let status_ready = ide_ref
                .lsp
                .active_status_for_language("rust")
                .is_some_and(|(_, st)| matches!(st, LspServerStatus::Ready { .. }));
            let editor_has_diag = ide_ref
                .open_tabs
                .get(&main_path)
                .and_then(|tab| tab.read(cx).editor.read(cx).diagnostics())
                .is_some_and(|d| !d.is_empty());
            status_ready && editor_has_diag
        });
        if ready || std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    cx.update(|cx| {
        let ide_ref = ide.read(cx);
        assert!(matches!(
            ide_ref.lsp.active_status_for_language("rust"),
            Some((_, LspServerStatus::Ready { .. }))
        ));
        let tab = ide_ref.open_tabs.get(&main_path).unwrap();
        let diags_len = tab
            .read(cx)
            .editor
            .read(cx)
            .diagnostics()
            .map(|d| d.len())
            .unwrap_or(0);
        assert!(diags_len >= 1, "expected LSP diagnostic synced into EditorState");
    });

    // Open Settings -> LSP tab -> add custom server & save
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            ide.open_settings_tab(SettingsTab::Lsp, window, cx);
            if let Some(ActiveModal::Settings(s)) = &mut ide.active_modal {
                s.add_custom_server(window, cx);
                s.srv_name_input
                    .update(cx, |i, cx| i.set_value("ruff-lsp", window, cx));
                s.srv_languages_input
                    .update(cx, |i, cx| i.set_value("python", window, cx));
                s.srv_command_input
                    .update(cx, |i, cx| i.set_value("ruff", window, cx));
                s.srv_args_input
                    .update(cx, |i, cx| i.set_value("server", window, cx));
            }
            ide.save_settings_modal(cx);
        });
    })
    .unwrap();
    settle(cx, handle);

    cx.update(|cx| {
        let ide_ref = ide.read(cx);
        let lsp_cfg = ide_ref.lsp_config.read().unwrap();
        assert_eq!(lsp_cfg.servers.len(), 2);
        assert_eq!(lsp_cfg.servers[1].name, "ruff-lsp");
        assert_eq!(lsp_cfg.servers[1].command, "ruff");
        assert_eq!(lsp_cfg.servers[1].args, vec!["server".to_string()]);
        assert_eq!(lsp_cfg.servers[1].languages, vec!["python".to_string()]);
        ide_ref.lsp.shutdown_all();
    });
}

#[gpui_kit::test]
fn ide_vim_modal_editing_undo_visual_ex_and_code_nav(cx: &mut TestAppContext) {
    let ws = setup_sample_workspace();
    let (handle, ide) = open_kestrel_vim(cx, ws.path());
    settle(cx, handle);

    let main_path = fs::canonicalize(ws.path().join("src/main.rs")).unwrap();
    let models_path = fs::canonicalize(ws.path().join("src/models.rs")).unwrap();

    // Verify file starts in Normal mode with readonly editor preventing stray Normal key text insertion.
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            let tab = ide.open_tabs.get(&main_path).unwrap().clone();
            tab.update(cx, |t, cx| {
                assert_eq!(t.vim.mode, VimMode::Normal);
                assert!(t.is_vim_readonly());
                t.editor.update(cx, |ed, cx| {
                    ed.set_value("alpha\nbeta\ngamma\n", window, cx);
                    ed.set_selected_range(0..0, cx);
                    ed.focus(window, cx);
                });
            });
        });
    })
    .unwrap();
    settle(cx, handle);

    // `d d` deletes "alpha\n", `j` moves to gamma, `p` pastes "alpha\n" after gamma, `u` undoes paste.
    cx.simulate_keystrokes(handle.into(), "d d");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert_eq!(tab.read(cx).editor.read(cx).value(), "beta\ngamma\n");
    });

    cx.simulate_keystrokes(handle.into(), "j p");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert_eq!(tab.read(cx).editor.read(cx).value(), "beta\ngamma\nalpha\n");
    });

    cx.simulate_keystrokes(handle.into(), "u");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert_eq!(tab.read(cx).editor.read(cx).value(), "beta\ngamma\n");
    });

    cx.simulate_keystrokes(handle.into(), "ctrl-r");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert_eq!(tab.read(cx).editor.read(cx).value(), "beta\ngamma\nalpha\n");
    });

    cx.simulate_keystrokes(handle.into(), "u");
    settle(cx, handle);

    // `g g` -> `V` -> `>` indents line 1, `escape` ensures Normal, `i` enters Insert, type text, `escape` returns to Normal.
    cx.simulate_keystrokes(handle.into(), "g g shift-v > escape");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert_eq!(tab.read(cx).vim.mode, VimMode::Normal);
        assert_eq!(tab.read(cx).editor.read(cx).value(), "    beta\ngamma\n");
    });

    cx.simulate_keystrokes(handle.into(), "i ! escape");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert_eq!(tab.read(cx).vim.mode, VimMode::Normal);
        assert_eq!(tab.read(cx).editor.read(cx).value(), "    !beta\ngamma\n");
    });

    // Undo in Normal mode reverts the entire Insert session back to `"    beta\ngamma\n"`, then Redo restores `"!"`.
    cx.simulate_keystrokes(handle.into(), "u ctrl-r");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert_eq!(tab.read(cx).editor.read(cx).value(), "    !beta\ngamma\n");
    });

    // `:w` writes buffer to disk and clears dirty flag.
    cx.simulate_keystrokes(handle.into(), ": w enter");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert!(!tab.read(cx).dirty);
    });
    assert_eq!(fs::read_to_string(&main_path).unwrap(), "    !beta\ngamma\n");

    // Put `Order::new` back, place cursor on `Order`, test Vim `g d` cross-file Go To Definition!
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            let tab = ide.open_tabs.get(&main_path).unwrap().clone();
            tab.update(cx, |t, cx| {
                t.editor.update(cx, |ed, cx| {
                    ed.set_value("use models::Order;\nfn f() { let _x = Order::new(1, 2); }\n", window, cx);
                    let pos = ed.value().find("Order::new").unwrap();
                    ed.set_selected_range(pos..pos, cx);
                    ed.focus(window, cx);
                });
            });
        });
    })
    .unwrap();
    settle(cx, handle);

    cx.simulate_keystrokes(handle.into(), "g d");
    settle(cx, handle);
    cx.update(|cx| {
        assert_eq!(ide.read(cx).active_tab.as_ref(), Some(&models_path));
    });
}

#[gpui_kit::test]
fn ide_vim_insert_ai_fim_and_settings_toggle(cx: &mut TestAppContext) {
    let ws = setup_sample_workspace();
    let (handle, ide) = open_kestrel_vim(cx, ws.path());
    settle(cx, handle);

    let main_path = fs::canonicalize(ws.path().join("src/main.rs")).unwrap();

    // Place cursor after `Order ` in Normal mode, press `a` (Insert at end), type `{`, verify AI FIM triggers & `Escape` returns to Normal.
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            let tab = ide.open_tabs.get(&main_path).unwrap().clone();
            tab.update(cx, |t, cx| {
                t.editor.update(cx, |ed, cx| {
                    ed.set_value(
                        "mod models;\nuse models::Order;\nfn build() -> Order {\n    Order\n}\n",
                        window,
                        cx,
                    );
                    let pos = ed.value().find("Order\n").unwrap() + "Order".len() - 1;
                    ed.set_selected_range(pos..pos, cx);
                    ed.focus(window, cx);
                });
            });
        });
    })
    .unwrap();
    settle(cx, handle);

    cx.simulate_keystrokes(handle.into(), "a space {");
    cx.background_executor
        .advance_clock(Duration::from_millis(60));
    settle(cx, handle);

    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert_eq!(tab.read(cx).vim.mode, VimMode::Insert);
        let active = ide.read(cx).fim.active().expect("expected FIM suggestion inside Vim Insert mode");
        let text = active.current_text().unwrap();
        assert!(text.contains("id,"), "expected `id,` in `{text}`");
    });

    cx.simulate_keystrokes(handle.into(), "tab");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        let text = tab.read(cx).editor.read(cx).value().to_string();
        assert!(text.contains("id,"), "expected Tab to accept FIM inside Vim Insert mode: `{text}`");
    });

    cx.simulate_keystrokes(handle.into(), "escape");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert_eq!(tab.read(cx).vim.mode, VimMode::Normal);
        assert!(tab.read(cx).is_vim_readonly());
    });

    // Open Settings -> Vim Mode tab, disable Vim & Save, verify tab transitions out of readonly Vim mode.
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            ide.open_settings_tab(SettingsTab::Vim, window, cx);
            if let Some(ActiveModal::Settings(s)) = &mut ide.active_modal {
                assert!(s.vim_enabled);
                s.vim_enabled = false;
            }
            ide.save_settings_modal(cx);
        });
    })
    .unwrap();
    settle(cx, handle);

    cx.update(|cx| {
        let ide_ref = ide.read(cx);
        assert!(!ide_ref.config.vim.enabled);
        let tab = ide_ref.open_tabs.get(&main_path).unwrap();
        assert!(!tab.read(cx).is_vim_readonly());
    });
}

#[gpui_kit::test]
fn ide_tabs_shortcuts_and_vim_code_navigation(cx: &mut TestAppContext) {
    let ws = setup_sample_workspace();
    let (handle, ide) = open_kestrel_vim(cx, ws.path());
    settle(cx, handle);

    let main_path = fs::canonicalize(ws.path().join("src/main.rs")).unwrap();
    let models_path = fs::canonicalize(ws.path().join("src/models.rs")).unwrap();
    let cargo_path = fs::canonicalize(ws.path().join("Cargo.toml")).unwrap();

    // Open models.rs (tab 2) and Cargo.toml (tab 3) alongside main.rs (tab 1).
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            ide.open_file(&models_path, None, false, window, cx);
            ide.open_file(&cargo_path, None, false, window, cx);
        });
    })
    .unwrap();
    settle(cx, handle);

    let assert_active_and_focused = |cx: &mut TestAppContext, expected: &Path| {
        cx.update_window(handle.into(), |_, window, cx| {
            let ide_ref = ide.read(cx);
            assert_eq!(ide_ref.active_tab.as_deref(), Some(expected));
            let tab = ide_ref.open_tabs.get(expected).expect("open tab missing");
            assert!(
                tab.read(cx).editor.read(cx).focus_handle(cx).is_focused(window),
                "expected active editor `{}` to hold keyboard focus after tab switch",
                expected.display()
            );
        })
        .unwrap();
    };

    assert_active_and_focused(cx, &cargo_path);

    // 1. Direct tab shortcuts: `cmd-1`, `cmd-2`, `cmd-3` (plus focus preservation).
    cx.simulate_keystrokes(handle.into(), "cmd-1");
    settle(cx, handle);
    assert_active_and_focused(cx, &main_path);

    cx.simulate_keystrokes(handle.into(), "cmd-2");
    settle(cx, handle);
    assert_active_and_focused(cx, &models_path);

    cx.simulate_keystrokes(handle.into(), "cmd-3");
    settle(cx, handle);
    assert_active_and_focused(cx, &cargo_path);

    // 2. Previous / Next tab shortcuts: `cmd->` / `cmd-<` (wrapping) and `cmd-shift-,` / `cmd-shift-.`.
    cx.simulate_keystrokes(handle.into(), "cmd->");
    settle(cx, handle);
    assert_active_and_focused(cx, &main_path);

    cx.simulate_keystrokes(handle.into(), "cmd-<");
    settle(cx, handle);
    assert_active_and_focused(cx, &cargo_path);

    cx.simulate_keystrokes(handle.into(), "cmd-shift-,");
    settle(cx, handle);
    assert_active_and_focused(cx, &models_path);

    cx.simulate_keystrokes(handle.into(), "cmd-shift-.");
    settle(cx, handle);
    assert_active_and_focused(cx, &cargo_path);

    // 3. Vim tab switching: `gt`, `2gt`, `gT`.
    cx.simulate_keystrokes(handle.into(), "g t");
    settle(cx, handle);
    assert_active_and_focused(cx, &main_path);

    cx.simulate_keystrokes(handle.into(), "2 g t");
    settle(cx, handle);
    assert_active_and_focused(cx, &models_path);

    cx.simulate_keystrokes(handle.into(), "g T");
    settle(cx, handle);
    assert_active_and_focused(cx, &main_path);

    // 4. Cross-file `gd` followed immediately by `Ctrl+o` (jump back) and `Ctrl+i` (jump forward).
    let origin_offset = cx
        .update_window(handle.into(), |_, window, cx| {
            ide.update(cx, |ide, cx| {
                let tab = ide.open_tabs.get(&main_path).unwrap().clone();
                let pos = tab.read(cx).editor.read(cx).value().find("Order::new").unwrap();
                tab.update(cx, |t, cx| {
                    t.editor.update(cx, |ed, cx| {
                        ed.set_selected_range(pos..pos, cx);
                        ed.focus(window, cx);
                    });
                });
                pos
            })
        })
        .unwrap();
    settle(cx, handle);

    cx.simulate_keystrokes(handle.into(), "g d");
    settle(cx, handle);
    assert_active_and_focused(cx, &models_path);
    let def_offset = cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&models_path).unwrap();
        let range = tab.read(cx).editor.read(cx).selected_range();
        assert_eq!(range.start, range.end, "Vim Normal jump should not leave selection active");
        let text = tab.read(cx).editor.read(cx).value();
        assert!(
            text[range.start..].starts_with("pub struct Order")
                || text[range.start..].starts_with("Order"),
            "expected cursor on `Order` definition line, got `{}`",
            &text[range.start..text.len().min(range.start + 18)]
        );
        range.start
    });

    cx.simulate_keystrokes(handle.into(), "ctrl-o");
    settle(cx, handle);
    assert_active_and_focused(cx, &main_path);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        let range = tab.read(cx).editor.read(cx).selected_range();
        assert_eq!(range.start, origin_offset, "Ctrl+o should restore exact origin offset in main.rs");
    });

    cx.simulate_keystrokes(handle.into(), "ctrl-i");
    settle(cx, handle);
    assert_active_and_focused(cx, &models_path);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&models_path).unwrap();
        let range = tab.read(cx).editor.read(cx).selected_range();
        assert_eq!(range.start, def_offset, "Ctrl+i should restore definition offset in models.rs");
    });

    // 5. Intra-file jump list (`gg` -> `G` -> `ctrl-o` -> `ctrl-i`) and symbol navigation (`]s` / `[s`) + delimiters (`[{`).
    cx.simulate_keystrokes(handle.into(), "g g shift-g");
    settle(cx, handle);
    let eof_offset = cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&models_path).unwrap();
        tab.read(cx).editor.read(cx).selected_range().start
    });
    assert!(eof_offset > 40);

    cx.simulate_keystrokes(handle.into(), "ctrl-o");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&models_path).unwrap();
        assert_eq!(tab.read(cx).editor.read(cx).selected_range().start, 0);
    });

    cx.simulate_keystrokes(handle.into(), "ctrl-i");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&models_path).unwrap();
        assert_eq!(tab.read(cx).editor.read(cx).selected_range().start, eof_offset);
    });

    // `]s` and `[s` navigate symbols in `models.rs`.
    cx.simulate_keystrokes(handle.into(), "g g ] s");
    settle(cx, handle);
    let first_sym_offset = cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&models_path).unwrap();
        let offset = tab.read(cx).editor.read(cx).selected_range().start;
        assert!(offset > 0, "expected `]s` to move onto first symbol");
        offset
    });
    cx.simulate_keystrokes(handle.into(), "] s");
    settle(cx, handle);
    let second_sym_offset = cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&models_path).unwrap();
        let offset = tab.read(cx).editor.read(cx).selected_range().start;
        assert!(offset > first_sym_offset, "expected second `]s` to advance to next symbol");
        offset
    });
    cx.simulate_keystrokes(handle.into(), "[ s");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&models_path).unwrap();
        assert_eq!(tab.read(cx).editor.read(cx).selected_range().start, first_sym_offset);
    });
    let _ = second_sym_offset;

    // `[{` jumps to enclosing `{` inside `impl Order`.
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&models_path).unwrap().clone();
        let pos = tab.read(cx).editor.read(cx).value().find("total_cents }").unwrap();
        tab.update(cx, |t, cx| {
            t.editor.update(cx, |ed, cx| ed.set_selected_range(pos..pos, cx));
        });
    });
    settle(cx, handle);
    cx.simulate_keystrokes(handle.into(), "[ {");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&models_path).unwrap();
        let ed = tab.read(cx).editor.read(cx);
        let pos = ed.selected_range().start;
        assert_eq!(ed.value().as_bytes().get(pos), Some(&b'{'));
    });

    // 6. Diagnostic navigation (`]d` / `[d`) + `gf` (Go to File) + `gi` (Last Insert).
    cx.simulate_keystrokes(handle.into(), "cmd-1");
    settle(cx, handle);
    assert_active_and_focused(cx, &main_path);

    let broken_src = "mod models;\nfn bad1( {}\nfn ok() {}\nfn bad2( {}\n";
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            let tab = ide.open_tabs.get(&main_path).unwrap().clone();
            tab.update(cx, |t, cx| {
                t.editor.update(cx, |ed, cx| {
                    ed.set_value(broken_src, window, cx);
                    ed.set_selected_range(0..0, cx);
                    ed.focus(window, cx);
                });
            });
            ide.index.reindex_file(&main_path, broken_src);
            tab.update(cx, |t, cx| t.sync_diagnostics(cx));
        });
    })
    .unwrap();
    settle(cx, handle);

    cx.simulate_keystrokes(handle.into(), "] d");
    settle(cx, handle);
    let diag1_offset = cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        tab.read(cx).editor.read(cx).selected_range().start
    });
    cx.simulate_keystrokes(handle.into(), "] d");
    settle(cx, handle);
    let diag2_offset = cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        tab.read(cx).editor.read(cx).selected_range().start
    });
    assert!(diag2_offset > diag1_offset, "expected `]d` to step across diagnostics: {diag1_offset} -> {diag2_offset}");

    cx.simulate_keystrokes(handle.into(), "[ d");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert_eq!(tab.read(cx).editor.read(cx).selected_range().start, diag1_offset);
    });

    // `gi`: edit on line 3 (`fn ok`), leave Insert mode, jump to line 1 (`gg`), press `gi` -> returns to Insert mode at edit pos!
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap().clone();
        let ok_pos = tab.read(cx).editor.read(cx).value().find("fn ok").unwrap();
        tab.update(cx, |t, cx| {
            t.editor.update(cx, |ed, cx| ed.set_selected_range(ok_pos..ok_pos, cx));
        });
    });
    settle(cx, handle);
    cx.simulate_keystrokes(handle.into(), "i a b c escape");
    settle(cx, handle);
    let last_insert_offset = cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert_eq!(tab.read(cx).vim.mode, VimMode::Normal);
        tab.read(cx).last_insert_offset.expect("expected recorded last_insert_offset")
    });
    cx.simulate_keystrokes(handle.into(), "g g g i");
    settle(cx, handle);
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap();
        assert_eq!(tab.read(cx).vim.mode, VimMode::Insert);
        assert_eq!(tab.read(cx).editor.read(cx).selected_range().start, last_insert_offset);
    });
    cx.simulate_keystrokes(handle.into(), "escape");
    settle(cx, handle);

    // `gf`: place cursor on `models` in `mod models;` -> opens `models.rs`!
    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_path).unwrap().clone();
        let pos = tab.read(cx).editor.read(cx).value().find("models;").unwrap() + 1;
        tab.update(cx, |t, cx| {
            t.editor.update(cx, |ed, cx| ed.set_selected_range(pos..pos, cx));
        });
    });
    settle(cx, handle);
    cx.simulate_keystrokes(handle.into(), "g f");
    settle(cx, handle);
    assert_active_and_focused(cx, &models_path);
}

#[gpui_kit::test]
fn ide_ask_ai_panel_code_selection_and_explanation(cx: &mut TestAppContext) {
    use kestrel::ai::ChatRole;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let ws = setup_sample_workspace();
    let (handle, ide) = open_kestrel_vim(cx, ws.path());
    settle(cx, handle);

    let main_path = fs::canonicalize(ws.path().join("src/main.rs")).unwrap();
    let models_path = fs::canonicalize(ws.path().join("src/models.rs")).unwrap();

    // 1. Select `process_order` body in src/main.rs and check reactive live_selection sync.
    let (sel_start, sel_end) = cx
        .update_window(handle.into(), |_, window, cx| {
            ide.update(cx, |ide, cx| {
                let tab = ide.open_tabs.get(&main_path).unwrap().clone();
                let text = tab.read(cx).editor.read(cx).value().to_string();
                let start = text.find("fn process_order").unwrap();
                let rel_end = text[start..].find("}\n").unwrap() + 1;
                let end = start + rel_end;
                tab.update(cx, |t, cx| {
                    t.editor.update(cx, |ed, cx| {
                        ed.set_selected_range(start..end, cx);
                        ed.focus(window, cx);
                    });
                });
                (start, end)
            })
        })
        .unwrap();
    assert!(sel_end > sel_start);
    settle(cx, handle);

    cx.update(|cx| {
        let live = ide
            .read(cx)
            .ask_ai
            .read(cx)
            .live_selection()
            .expect("expected active editor selection to sync to AskAiPanel")
            .clone();
        assert_eq!(live.path, main_path);
        assert!(live.from_selection);
        assert!(live.code.contains("Order::new(1, 2500)"));
        assert!(live.start_line >= 3);
    });

    // 2. Press `cmd-shift-e` (ExplainSelection) -> opens Right Dock to Ask AI & generates offline AST explanation.
    cx.simulate_keystrokes(handle.into(), "cmd-shift-e");
    for _ in 0..10 {
        settle(cx, handle);
        let done = cx.update(|cx| !ide.read(cx).ask_ai.read(cx).is_generating());
        if done {
            break;
        }
    }

    cx.update(|cx| {
        let ide_ref = ide.read(cx);
        assert!(ide_ref.dock_area.read(cx).is_dock_open(DockPlacement::Right));
        let ask = ide_ref.ask_ai.read(cx);
        assert!(
            !ask.attached_contexts().is_empty(),
            "expected selected code attached to AskAiPanel"
        );
        assert!(ask.attached_contexts()[0].code.contains("Order::new"));
        assert_eq!(ask.messages().len(), 2);
        assert_eq!(ask.messages()[0].role, ChatRole::User);
        assert_eq!(ask.messages()[1].role, ChatRole::Assistant);
        let reply = &ask.messages()[1].content;
        assert!(reply.contains("src/main.rs"), "expected file ref in explanation: {reply}");
        assert!(
            reply.contains("process_order"),
            "expected symbol/code details in explanation: {reply}"
        );
    });

    // 3. Switch to src/models.rs, use Vim Visual mode (`V j`), press `g ?` -> attaches models.rs selection too!
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            ide.open_file(&models_path, Some((1, 0)), false, window, cx);
        });
    })
    .unwrap();
    settle(cx, handle);

    cx.simulate_keystrokes(handle.into(), "shift-v j g ?");
    for _ in 0..10 {
        settle(cx, handle);
        let done = cx.update(|cx| !ide.read(cx).ask_ai.read(cx).is_generating());
        if done {
            break;
        }
    }

    cx.update(|cx| {
        let ask = ide.read(cx).ask_ai.read(cx);
        assert_eq!(ask.attached_contexts().len(), 2, "expected multi-file code contexts pinned");
        assert!(ask.attached_contexts().iter().any(|c| c.path == models_path && c.code.contains("pub struct Order")));
        assert_eq!(ask.messages().len(), 4);
    });

    // 4. Configure a local OpenAI-compatible mock server on 127.0.0.1 and verify prompt + selected code POST & Markdown reply!
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let captured_body = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured_ref = captured_body.clone();
    let srv_handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut expected_total: Option<usize> = None;
        loop {
            let n = stream.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..n]);
            if expected_total.is_none() {
                if let Some(hdr_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let hdr_str = String::from_utf8_lossy(&raw[..hdr_end]);
                    let content_len = hdr_str
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    expected_total = Some(hdr_end + 4 + content_len);
                }
            }
            if let Some(total) = expected_total {
                if raw.len() >= total {
                    break;
                }
            }
        }
        *captured_ref.lock().unwrap() = String::from_utf8_lossy(&raw).to_string();

        let resp_json = r####"{"choices":[{"message":{"content":"### Summary\n`process_order` constructs an `Order::new(1, 2500)` from an `OrderState`."}}]}"####;
        let http_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            resp_json.len(),
            resp_json
        );
        stream.write_all(http_resp.as_bytes()).unwrap();
        stream.flush().unwrap();
    });

    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            {
                let mut cfg = ide.ask_config.write().unwrap();
                cfg.endpoint = format!("http://{addr}/v1/chat/completions");
                cfg.model = "deepseek-chat".to_string();
                cfg.api_key = "test-ask-key".to_string();
            }
            ide.ask_ai.update(cx, |ai, cx| {
                ai.set_prompt_text("How do Order and process_order connect?", window, cx);
            });
            ide.submit_ask_ai_prompt(window, cx);
        });
    })
    .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        settle(cx, handle);
        let done = cx.update(|cx| !ide.read(cx).ask_ai.read(cx).is_generating());
        if done || std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    srv_handle.join().unwrap();

    let sent_http = captured_body.lock().unwrap().clone();
    assert!(sent_http.contains("How do Order and process_order connect?"));
    assert!(sent_http.contains("Order::new(1, 2500)"));
    assert!(sent_http.contains("pub struct Order"));

    cx.update(|cx| {
        let ask = ide.read(cx).ask_ai.read(cx);
        let last = ask.messages().last().unwrap();
        assert_eq!(last.role, ChatRole::Assistant);
        assert!(!last.is_error);
        assert!(last.content.contains("constructs an `Order::new(1, 2500)`"));
    });
}

#[gpui_kit::test]
fn ide_regex_symbol_search_inlay_inline_doc_and_scroll_padding(cx: &mut TestAppContext) {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"scroll_ws\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();

    // Generate 28 workspace files & symbols + a documented struct and a 90-line file for scroll padding.
    let mut big_src = String::from(
        "/// Primary catalog item stored in the warehouse.\npub struct CatalogEntry {\n    pub code: u64,\n    pub price: i64,\n}\n\nimpl CatalogEntry {\n    pub fn build(code: u64, price: i64) -> Self {\n        Self { code, price }\n    }\n}\n\n",
    );
    for i in 0..26 {
        let fname = format!("mod_{i:02}.rs");
        fs::write(
            src.join(&fname),
            format!("/// Helper number {i}\npub fn item_worker_{i:02}(val: i32) -> i32 {{ val + {i} }}\n"),
        )
        .unwrap();
        big_src.push_str(&format!(
            "pub fn helper_line_{i:02}() -> usize {{ {i} }}\n"
        ));
    }
    for i in 26..65 {
        big_src.push_str(&format!(
            "pub fn extra_filler_{i:02}() -> usize {{ {i} }}\n"
        ));
    }
    big_src.push_str("pub fn main() {\n    let entry = CatalogEntry::build(7, 999);\n    let _x = helper_line_00();\n}\n");
    let main_path = src.join("main.rs");
    fs::write(&main_path, &big_src).unwrap();

    let (handle, ide) = open_kestrel_vim(cx, dir.path());
    settle(cx, handle);

    let main_canon = fs::canonicalize(&main_path).unwrap();

    // 1. Open Quick Open (27+ files), simulate Down arrow keys & verify `#palette-results` scrolls with padding!
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            ide.open_quick_open(window, cx);
        });
    })
    .unwrap();
    settle(cx, handle);

    for _ in 0..14 {
        cx.simulate_keystrokes(handle.into(), "down");
        settle(cx, handle);
    }

    cx.update(|cx| {
        let ide_ref = ide.read(cx);
        let Some(ActiveModal::Palette(pal)) = &ide_ref.active_modal else {
            panic!("expected Quick Open palette");
        };
        assert_eq!(
            pal.selected_idx, 14,
            "pressing Down 14 times inside Quick Open must advance selection (check Input MoveDown propagation)"
        );
        let off_y = pal.scroll_handle.offset().y;
        assert!(
            off_y < px(0.),
            "Quick Open list must scroll when selection moves near bottom; offset.y={off_y:?}"
        );
        let vp = pal.scroll_handle.bounds();
        let row = pal.scroll_handle.bounds_for_item(pal.selected_idx).unwrap();
        let vis_bottom = row.bottom() + off_y;
        let expected_pad = row.size.height * (pal.scroll_padding_items as f32) - px(1.5);
        assert!(
            vis_bottom <= vp.bottom() - expected_pad,
            "Quick Open selection should retain {} rows bottom padding: vis_bottom={vis_bottom:?}, vp_bottom={:?}, row_h={:?}",
            pal.scroll_padding_items,
            vp.bottom(),
            row.size.height
        );
    });

    // Move back up with Up arrow and verify selection & scroll follow upward.
    for _ in 0..12 {
        cx.simulate_keystrokes(handle.into(), "up");
        settle(cx, handle);
    }
    cx.update(|cx| {
        let ide_ref = ide.read(cx);
        let Some(ActiveModal::Palette(pal)) = &ide_ref.active_modal else {
            panic!("expected Quick Open palette");
        };
        assert_eq!(pal.selected_idx, 2);
        let vp = pal.scroll_handle.bounds();
        let row = pal.scroll_handle.bounds_for_item(pal.selected_idx).unwrap();
        let off_y = pal.scroll_handle.offset().y;
        assert!(row.top() + off_y >= vp.top() - px(1.));
    });

    // 2. Global Regex Workspace Symbol Search (`⌘⇧O` + regex mode / `re:` + error feedback on bad regex)
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            ide.close_modal(cx);
            ide.open_go_to_symbol(window, cx);
            ide.toggle_palette_regex(cx);
            if let Some(ActiveModal::Palette(pal)) = &mut ide.active_modal {
                pal.query_input.update(cx, |inp, cx| {
                    inp.set_value("^item_worker_1[0-5]$", window, cx);
                });
            }
            ide.refresh_palette(cx);
        });
    })
    .unwrap();
    settle(cx, handle);

    cx.update(|cx| {
        let ide_ref = ide.read(cx);
        let Some(ActiveModal::Palette(pal)) = &ide_ref.active_modal else {
            panic!("expected Symbol palette");
        };
        assert!(pal.regex_mode);
        assert!(pal.regex_error.is_none());
        let labels: Vec<&str> = pal
            .symbol_hits
            .iter()
            .map(|i| i.location.symbol_name.as_str())
            .collect();
        assert_eq!(labels.len(), 6, "expected 10..=15 matched: {labels:?}");
        assert!(labels.contains(&"item_worker_10"));
        assert!(labels.contains(&"item_worker_15"));
    });

    // Confirm invalid regex surfaces `regex_error`, then close modal.
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            if let Some(ActiveModal::Palette(pal)) = &mut ide.active_modal {
                pal.query_input.update(cx, |inp, cx| {
                    inp.set_value("[unclosed", window, cx);
                });
            }
            ide.refresh_palette(cx);
        });
    })
    .unwrap();
    settle(cx, handle);
    cx.update(|cx| {
        let Some(ActiveModal::Palette(pal)) = &ide.read(cx).active_modal else {
            panic!("expected Symbol palette");
        };
        assert!(pal.regex_error.is_some());
    });
    cx.update(|cx| {
        ide.update(cx, |ide, cx| ide.close_modal(cx));
    });

    // 3. Inlay hints, inline hover docstrings & Editor scroll padding
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            ide.open_file(&main_canon, Some((1, 14)), true, window, cx);
            ide.show_hover_at_cursor(cx);
        });
    })
    .unwrap();
    // Advance simulated executor clock so debounced inlay hints & inline hover doc tasks run cleanly.
    for _ in 0..8 {
        cx.background_executor
            .advance_clock(Duration::from_millis(50));
        settle(cx, handle);
        let ready = cx.update(|cx| {
            let tab = ide.read(cx).open_tabs.get(&main_canon).unwrap().read(cx);
            !tab.inlay_hints().is_empty() && tab.inline_hover_doc.is_some()
        });
        if ready {
            break;
        }
    }

    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_canon).unwrap().read(cx);
        let hints = tab.inlay_hints();
        assert!(
            hints.iter().any(|h| h.label.contains("CatalogEntry") || h.label.contains("code:")),
            "expected local AST/LSP inlay hints for `let entry = CatalogEntry::build(7, 999)`: {hints:?}"
        );
        let doc = tab
            .inline_hover_doc
            .as_ref()
            .expect("expected inline hover docstring on CatalogEntry");
        assert!(
            doc.summary.contains("Primary catalog item"),
            "unexpected inline docstring summary: {}",
            doc.summary
        );
    });

    // Move cursor to line 40 via Vim Ex command, verify editor scroll padding keeps clearance, and `:set scrolloff=8`.
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            let cur_tab = ide.open_tabs.get(&main_canon).unwrap().clone();
            cur_tab.update(cx, |t, cx| {
                t.jump_to_line_col(40, 0, None, window, cx);
                t.align_cursor_in_viewport(kestrel::vim::ScrollAlign::Center, cx);
            });
            assert_eq!(cur_tab.read(cx).scroll_padding, 5);
            cur_tab.update(cx, |t, cx| {
                t.apply_display_settings(8, true, true, Some(window), cx);
            });
        });
    })
    .unwrap();
    settle(cx, handle);

    cx.update(|cx| {
        let tab = ide.read(cx).open_tabs.get(&main_canon).unwrap().read(cx);
        assert_eq!(tab.scroll_padding, 8);
        let off = tab.editor.read(cx).scroll_offset();
        assert!(
            off.y < px(0.),
            "jumping to line 40 and centering cursor must scroll editor viewport; off={off:?}"
        );
    });
}

#[gpui_kit::test]
fn ide_ai_code_links_react_streaming_and_conversation_isolation(cx: &mut TestAppContext) {
    use kestrel::ai::{AgentStepKind, AgentStepStatus, ChatRole};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let ws = setup_sample_workspace();
    let (handle, ide) = open_kestrel(cx, ws.path());
    settle(cx, handle);

    let main_path = fs::canonicalize(ws.path().join("src/main.rs")).unwrap();
    let models_path = fs::canonicalize(ws.path().join("src/models.rs")).unwrap();

    // Mock HTTP SSE server serving 2-turn ReAct stream (`reasoning_content` + `tool_calls` -> final answer)
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let srv_handle = std::thread::spawn(move || {
        let read_req = |stream: &mut std::net::TcpStream| {
            let mut raw = Vec::new();
            let mut chunk = [0u8; 4096];
            let mut expected_total: Option<usize> = None;
            loop {
                let n = stream.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..n]);
                if expected_total.is_none() {
                    if let Some(hdr_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let hdr_str = String::from_utf8_lossy(&raw[..hdr_end]);
                        let content_len = hdr_str
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.trim()
                                    .eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        expected_total = Some(hdr_end + 4 + content_len);
                    }
                }
                if let Some(total) = expected_total {
                    if raw.len() >= total {
                        break;
                    }
                }
            }
            String::from_utf8_lossy(&raw).to_string()
        };

        // Turn 1: stream ReAct Thought + tool_call(`read_lines` on `src/models.rs`)
        let (mut s1, _) = listener.accept().unwrap();
        let req1 = read_req(&mut s1);
        assert!(req1.contains("search_symbols"));
        assert!(req1.contains("Explain Order fields"));
        let sse1 = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Inspecting models.rs struct fields...\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"tc_1\",\"function\":{\"name\":\"read_lines\",\"arguments\":\"{\\\"path\\\":\\\"src/models.rs\\\",\\\"start_line\\\":1,\\\"end_line\\\":6}\"}}]}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let resp1 = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            sse1.len(),
            sse1
        );
        s1.write_all(resp1.as_bytes()).unwrap();
        s1.flush().unwrap();

        // Turn 2: accept follow-up request with `read_lines` output & stream final answer with code link
        let (mut s2, _) = listener.accept().unwrap();
        let req2 = read_req(&mut s2);
        assert!(req2.contains("pub total_cents: i64"));
        let sse2 = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"`Order` defines `total_cents` at \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"src/models.rs:L4.\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let resp2 = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            sse2.len(),
            sse2
        );
        s2.write_all(resp2.as_bytes()).unwrap();
        s2.flush().unwrap();
    });

    // Configure Thread 1 to use our SSE mock server and pin `src/models.rs` selection in Thread 1 only.
    let thread1_id = cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            {
                let mut cfg = ide.ask_config.write().unwrap();
                cfg.endpoint = format!("http://{addr}/v1/chat/completions");
                cfg.model = "deepseek-chat".to_string();
                cfg.api_key = "test-stream-key".to_string();
            }
            ide.open_file(&models_path, Some((0, 0)), false, window, cx);
            ide.open_ask_ai_with_selection(
                false,
                Some("Explain Order fields".to_string()),
                window,
                cx,
            );
            ide.ask_ai.read(cx).active_conversation_id()
        })
    })
    .unwrap();

    // Create isolated Thread 2 IMMEDIATELY while Thread 1 is actively generating, proving
    // empty initial contexts/messages/generating state and isolated concurrent execution!
    let thread2_id = cx.update(|cx| {
        ide.update(cx, |ide, cx| {
            ide.ask_ai.update(cx, |ai, cx| {
                let c1_live = ai.conversations().iter().find(|c| c.id == thread1_id).unwrap();
                assert!(c1_live.generating, "Thread 1 should be generating in background");

                let t2 = ai.new_conversation(Some("Isolated Thread 2".to_string()), cx);
                assert_ne!(t2, thread1_id);
                assert!(ai.attached_contexts().is_empty(), "new thread must have empty pinned contexts");
                assert!(ai.messages().is_empty(), "new thread must start with empty messages");
                assert!(!ai.is_generating(), "new thread must not inherit Thread 1's active stream state");

                // Switch config to Local AST for Thread 2's offline ReAct exploration.
                {
                    let mut cfg = ide.ask_config.write().unwrap();
                    cfg.endpoint.clear();
                    cfg.api_key.clear();
                }
                {
                    let mut fcfg = ide.fim_config.write().unwrap();
                    fcfg.provider = kestrel::config::FimProviderKind::LocalAst;
                    fcfg.api_key.clear();
                }
                ai.submit_preset(
                    kestrel::ai::AskAiPreset::Custom,
                    Some("Where is process_order defined?".to_string()),
                    None,
                    cx,
                );
                t2
            })
        })
    });

    for _ in 0..80 {
        settle(cx, handle);
        let both_done = cx.update(|cx| {
            let ask = ide.read(cx).ask_ai.read(cx);
            let t1_done = ask
                .conversations()
                .iter()
                .find(|c| c.id == thread1_id)
                .is_some_and(|c| !c.generating);
            let t2_done = ask
                .conversations()
                .iter()
                .find(|c| c.id == thread2_id)
                .is_some_and(|c| !c.generating);
            t1_done && t2_done
        });
        if both_done {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    srv_handle.join().unwrap();
    settle(cx, handle);

    // Verify complete isolation between Thread 1 & Thread 2, streaming ReAct steps, and auto-linked citations!
    let (t1_cite_url, t2_cite_url) = cx.update(|cx| {
        let ask = ide.read(cx).ask_ai.read(cx);
        let t1 = ask.conversations().iter().find(|c| c.id == thread1_id).unwrap();
        let t2 = ask.conversations().iter().find(|c| c.id == thread2_id).unwrap();

        assert_eq!(t1.messages.len(), 2);
        assert_eq!(t2.messages.len(), 2);

        let m1 = &t1.messages[1];
        assert_eq!(m1.role, ChatRole::Assistant);
        assert!(!m1.streaming);
        assert!(m1
            .trace_steps
            .iter()
            .any(|s| s.kind == AgentStepKind::Thought && s.detail.contains("models.rs")));
        assert!(m1.trace_steps.iter().any(|s| {
            s.tool_name.as_deref() == Some("read_lines") && s.status == AgentStepStatus::Done
        }));
        assert!(
            m1.content
                .contains("[src/models.rs:L4](kestrel://open?path=src/models.rs&line=4)"),
            "unexpected m1.content (is_error={}): {:?}",
            m1.is_error,
            m1.content
        );
        let cite1 = m1
            .citations
            .iter()
            .find(|c| c.rel_path == "src/models.rs" && c.start_line == 4)
            .expect("expected clickable citation for src/models.rs:L4")
            .to_kestrel_url();

        let m2 = &t2.messages[1];
        assert!(!m2.streaming);
        assert!(
            m2.trace_steps.len() >= 3,
            "Thread 2 should have streamed ReAct steps: {:?}",
            m2.trace_steps
        );
        assert!(!m2.content.contains("total_cents` at"));
        let cite2 = m2
            .citations
            .iter()
            .find(|c| c.rel_path == "src/main.rs")
            .expect("expected clickable citation discovered in src/main.rs")
            .to_kestrel_url();

        (cite1, cite2)
    });

    // Click code links & verify editor jumps straight to the target file and exact 0-based line!
    cx.update_window(handle.into(), |_, window, cx| {
        ide.update(cx, |ide, cx| {
            assert!(ide.open_code_link(&t2_cite_url, window, cx));
            assert_eq!(ide.active_tab.as_ref(), Some(&main_path));
            assert!(ide.open_code_link(&t1_cite_url, window, cx));
        });
    })
    .unwrap();
    settle(cx, handle);

    cx.update(|cx| {
        let ide_ref = ide.read(cx);
        assert_eq!(ide_ref.active_tab.as_ref(), Some(&models_path));
        let tab = ide_ref.open_tabs.get(&models_path).unwrap().read(cx);
        assert_eq!(
            tab.editor.read(cx).cursor_position().line,
            3,
            "clicking src/models.rs:L4 must jump cursor to 0-based line 3"
        );
    });

    // Test Fork & Close conversation isolation without mutating source thread.
    let forked_id = cx.update(|cx| {
        ide.update(cx, |ide, cx| {
            ide.ask_ai.update(cx, |ai, cx| {
                ai.select_conversation(thread1_id, cx);
                let fk = ai.fork_active_conversation(cx);
                assert_eq!(ai.messages().len(), 2);
                ai.clear_messages(cx);
                assert!(ai.messages().is_empty());
                ai.select_conversation(thread1_id, cx);
                assert_eq!(
                    ai.messages().len(),
                    2,
                    "clearing forked thread must keep original Thread 1 messages isolated"
                );
                ai.close_conversation(fk, cx);
                assert_eq!(ai.conversations().len(), 2);
                fk
            })
        })
    });
    assert_ne!(forked_id, thread1_id);
}
