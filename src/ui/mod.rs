pub mod ask_ai_panel;
pub mod editor_pane;
pub mod explorer_panel;
pub mod ide;
pub mod inspector_panel;
pub mod modals;
pub mod results_panel;

use crate::index::extractor::SymbolKind;
use gpui_kit::{
    App, Hsla, KeyBinding, actions,
    component::{ActiveTheme as _, ColorName},
};

pub use ide::KestrelIde;

actions!(
    kestrel,
    [
        QuickOpen,
        GoToSymbol,
        FindReferences,
        NavigateBack,
        NavigateForward,
        PrevTab,
        NextTab,
        SwitchTab1,
        SwitchTab2,
        SwitchTab3,
        SwitchTab4,
        SwitchTab5,
        SwitchTab6,
        SwitchTab7,
        SwitchTab8,
        SwitchTab9,
        TriggerFim,
        AcceptFimWord,
        AcceptFimLine,
        CycleFimNext,
        CycleFimPrev,
        AskAi,
        ExplainSelection,
        SaveActiveFile,
        OpenSettings,
        OpenFolder,
        ToggleLeftDock,
        ToggleRightDock,
        ToggleBottomDock,
        ToggleVimMode,
        ToggleTheme,
        CloseModal,
        ConfirmModal,
        ModalUp,
        ModalDown,
        TogglePaletteRegex,
    ]
);

pub const MODAL_CONTEXT: &str = "KestrelModal";

pub fn register_keybindings(cx: &mut App) {
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-p", QuickOpen, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-p", QuickOpen, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-o", GoToSymbol, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-o", GoToSymbol, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-t", GoToSymbol, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-t", GoToSymbol, None),
        KeyBinding::new("shift-f12", FindReferences, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-left", NavigateBack, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt-left", NavigateBack, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-right", NavigateForward, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt-right", NavigateForward, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-<", PrevTab, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-,", PrevTab, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd->", NextTab, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-.", NextTab, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-tab", NextTab, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt-<", PrevTab, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt->", NextTab, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-1", SwitchTab1, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-2", SwitchTab2, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-3", SwitchTab3, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-4", SwitchTab4, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-5", SwitchTab5, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-6", SwitchTab6, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-7", SwitchTab7, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-8", SwitchTab8, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-9", SwitchTab9, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-1", SwitchTab1, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-2", SwitchTab2, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-3", SwitchTab3, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-4", SwitchTab4, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-5", SwitchTab5, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-6", SwitchTab6, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-7", SwitchTab7, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-8", SwitchTab8, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-9", SwitchTab9, None),
        KeyBinding::new("alt-\\", TriggerFim, None),
        KeyBinding::new("alt-right", AcceptFimWord, None),
        KeyBinding::new("alt-down", AcceptFimLine, None),
        KeyBinding::new("alt-]", CycleFimNext, None),
        KeyBinding::new("alt-[", CycleFimPrev, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-l", AskAi, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-l", AskAi, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-e", ExplainSelection, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-e", ExplainSelection, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-s", SaveActiveFile, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-s", SaveActiveFile, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-,", OpenSettings, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-,", OpenSettings, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-o", OpenFolder, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-o", OpenFolder, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-b", ToggleLeftDock, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-b", ToggleLeftDock, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-b", ToggleRightDock, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-alt-b", ToggleRightDock, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-j", ToggleBottomDock, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-j", ToggleBottomDock, None),
        KeyBinding::new("escape", CloseModal, Some(MODAL_CONTEXT)),
        KeyBinding::new("enter", ConfirmModal, Some(MODAL_CONTEXT)),
        KeyBinding::new("up", ModalUp, Some(MODAL_CONTEXT)),
        KeyBinding::new("down", ModalDown, Some(MODAL_CONTEXT)),
        KeyBinding::new("alt-r", TogglePaletteRegex, Some(MODAL_CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-r", TogglePaletteRegex, Some(MODAL_CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-r", TogglePaletteRegex, Some(MODAL_CONTEXT)),
    ]);
}

pub fn symbol_color(kind: SymbolKind, cx: &App) -> Hsla {
    let dark = cx.theme().is_dark();
    let shade = if dark { 400 } else { 600 };
    match kind {
        SymbolKind::Function | SymbolKind::Method => ColorName::Blue.scale(shade),
        SymbolKind::Struct | SymbolKind::Class => ColorName::Amber.scale(shade),
        SymbolKind::Enum => ColorName::Purple.scale(shade),
        SymbolKind::Trait | SymbolKind::Interface => ColorName::Cyan.scale(shade),
        SymbolKind::Impl => ColorName::Indigo.scale(shade),
        SymbolKind::Module | SymbolKind::Section => ColorName::Teal.scale(shade),
        SymbolKind::Constant => ColorName::Orange.scale(shade),
        SymbolKind::TypeAlias => ColorName::Emerald.scale(shade),
        SymbolKind::Field | SymbolKind::Variant | SymbolKind::Variable => {
            cx.theme().muted_foreground
        }
    }
}
