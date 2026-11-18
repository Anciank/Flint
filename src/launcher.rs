use crate::{
    brand::{APP_ID, APP_NAME},
    config::IdeConfig,
    ui::{
        FindReferences, GoToSymbol, KestrelIde, OpenFolder, OpenSettings, QuickOpen,
        register_keybindings,
    },
};
use gpui_kit::{
    App, AppContext as _, Bounds, Menu, MenuItem, WindowBounds, WindowOptions, actions,
    assets::AllAssets,
    component::TitleBar,
    px, size,
};
use std::path::PathBuf;

actions!(flint_app, [QuitApp]);

pub fn run_app() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--check-fim") {
        let cfg = IdeConfig::load(None);
        match crate::fim::client::test_fim_connection(&cfg.fim) {
            Ok(msg) => {
                println!("{msg}");
                return;
            }
            Err(err) => {
                eprintln!("FIM check failed: {err:#}");
                std::process::exit(1);
            }
        }
    }
    let initial_workspace = args.into_iter().find(|a| !a.starts_with('-')).map(PathBuf::from);
    let config = IdeConfig::load(initial_workspace.as_deref());

    gpui_kit::application()
        .with_assets(AllAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            register_keybindings(cx);
            cx.set_app_identity(APP_ID, APP_NAME);

            cx.on_action(|_: &QuitApp, cx| {
                cx.quit();
            });
            cx.on_window_closed(|cx, _window_id| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.set_menus(vec![
                Menu {
                    name: APP_NAME.into(),
                    items: vec![
                        MenuItem::action("AI FIM & Editor Settings...", OpenSettings),
                        MenuItem::separator(),
                        MenuItem::action(format!("Quit {APP_NAME}"), QuitApp),
                    ],
                    disabled: false,
                },
                Menu {
                    name: "File".into(),
                    items: vec![
                        MenuItem::action("Open Workspace Folder...", OpenFolder),
                        MenuItem::action("Quick Open...", QuickOpen),
                    ],
                    disabled: false,
                },
                Menu {
                    name: "Navigate".into(),
                    items: vec![
                        MenuItem::action("Go to Symbol...", GoToSymbol),
                        MenuItem::action("Find References", FindReferences),
                    ],
                    disabled: false,
                },
            ]);

            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            let mut titlebar = TitleBar::title_bar_options();
            titlebar.title = Some(APP_NAME.into());
            let mut window_options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(900.), px(600.))),
                ..TitleBar::window_options()
            };
            window_options.titlebar = Some(titlebar);
            window_options.app_id = Some(APP_ID.to_string());

            let ws = initial_workspace.clone();
            let cfg = config.clone();
            gpui_kit::open_window(window_options, cx, move |window, cx| {
                cx.new(|cx| KestrelIde::new(ws, cfg, window, cx))
            })
            .expect("failed to open Flint IDE window");

            cx.activate(true);
        });
}
