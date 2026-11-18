pub mod motions;

use std::ops::Range;

use crate::vim::motions::{
    CharFind, FindDirection, WordMotionPurpose, all_line_bounds, clamp_char_boundary,
    clamp_normal_cursor, col_at_byte, find_all_matches, find_enclosing_delim,
    find_matching_bracket, find_next_match, line_bounds, line_end_with_newline, line_first_non_ws,
    line_index_at, line_start_at_index, move_left, move_paragraph, move_right, move_vertical,
    move_word_backward, move_word_end, move_word_forward, next_char_end, prev_char_start,
    repeat_char_find, text_object_range, total_lines, word_under_cursor,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Ex,
    SearchForward,
    SearchBackward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
    VisualLine,
    Replace,
    Command(CommandKind),
}

impl VimMode {
    pub fn label(self) -> &'static str {
        match self {
            VimMode::Normal => "NORMAL",
            VimMode::Insert => "INSERT",
            VimMode::Visual => "VISUAL",
            VimMode::VisualLine => "V-LINE",
            VimMode::Replace => "REPLACE",
            VimMode::Command(CommandKind::Ex) => "COMMAND",
            VimMode::Command(CommandKind::SearchForward) => "SEARCH",
            VimMode::Command(CommandKind::SearchBackward) => "SEARCH ?",
        }
    }

    pub fn is_readonly(self) -> bool {
        !matches!(self, VimMode::Insert)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    Lowercase,
    Uppercase,
    ToggleCase,
}

impl Operator {
    fn key_char(self) -> &'static str {
        match self {
            Operator::Delete => "d",
            Operator::Change => "c",
            Operator::Yank => "y",
            Operator::Indent => ">",
            Operator::Outdent => "<",
            Operator::Lowercase => "gu",
            Operator::Uppercase => "gU",
            Operator::ToggleCase => "g~",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKey {
    G,
    FindChar(FindDirection, bool),
    ReplaceOne,
    TextObject { around: bool },
    BracketNext,
    BracketPrev,
    Z,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VimKeyEvent {
    pub key: String,
    pub key_char: Option<String>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl VimKeyEvent {
    pub fn char_key(ch: &str) -> Self {
        let shift = ch.chars().next().is_some_and(|c| c.is_uppercase());
        Self {
            key: ch.to_lowercase(),
            key_char: Some(ch.to_string()),
            ctrl: false,
            alt: false,
            shift,
        }
    }

    pub fn named(key: &str) -> Self {
        Self {
            key: key.to_string(),
            key_char: None,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    pub fn ctrl(key: &str) -> Self {
        Self {
            key: key.to_string(),
            key_char: None,
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    fn text_char(&self) -> Option<char> {
        if self.ctrl || self.alt {
            return None;
        }
        if let Some(kc) = &self.key_char {
            let mut chars = kc.chars();
            if let Some(c) = chars.next() {
                if chars.next().is_none() && !c.is_control() {
                    return Some(c);
                }
            }
        }
        if self.key.chars().count() == 1 {
            let c = self.key.chars().next()?;
            if self.shift && c.is_ascii_lowercase() {
                return Some(c.to_ascii_uppercase());
            }
            if !c.is_control() {
                return Some(c);
            }
        }
        None
    }

    fn is_escape(&self) -> bool {
        (!self.ctrl && !self.alt && self.key == "escape")
            || (self.ctrl && !self.alt && (self.key == "[" || self.key == "c"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VimRegister {
    pub text: String,
    pub linewise: bool,
}

impl Default for VimRegister {
    fn default() -> Self {
        Self {
            text: String::new(),
            linewise: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAlign {
    Center,
    Top,
    Bottom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VimEffect {
    MoveCursor {
        pos: usize,
    },
    SetSelection {
        anchor: usize,
        head: usize,
        linewise: bool,
    },
    Replace {
        range: Range<usize>,
        replacement: String,
        new_cursor: usize,
    },
    ModeChanged(VimMode),
    WriteClipboard(VimRegister),
    RequestPaste {
        before: bool,
        count: usize,
    },
    RepeatLastChange,
    Undo,
    Redo,
    ScrollHalfPage {
        down: bool,
        count: usize,
    },
    AlignCursor(ScrollAlign),
    PushJumpPoint,
    JumpBack,
    JumpForward,
    JumpLastInsert,
    GoToDefinition,
    GoToDeclaration,
    GoToTypeDefinition,
    GoToImplementation,
    FindReferences,
    GoToFile,
    CodeActions,
    OpenOutline,
    ShowHover,
    NextDiagnostic {
        errors_only: bool,
    },
    PrevDiagnostic {
        errors_only: bool,
    },
    NextSymbol,
    PrevSymbol,
    NextTab,
    PrevTab,
    GoToTab(usize),
    ShowJumpList,
    ListTabs,
    SaveFile,
    SaveAndClose,
    CloseTab {
        force: bool,
    },
    OpenFile(Option<String>),
    TriggerFim,
    AskAi {
        explain: bool,
        question: Option<String>,
    },
    SetVimEnabled(bool),
    SetInlayHints(bool),
    SetInlineHoverDocs(bool),
    SetScrollPadding(usize),
    StatusMessage(String),
}

#[derive(Debug, Clone, Default)]
pub struct VimOutcome {
    pub consumed: bool,
    pub effects: Vec<VimEffect>,
}

impl VimOutcome {
    fn ignored() -> Self {
        Self {
            consumed: false,
            effects: Vec::new(),
        }
    }

    fn handled(effects: Vec<VimEffect>) -> Self {
        Self {
            consumed: true,
            effects,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VimState {
    pub mode: VimMode,
    pub visual_anchor: Option<usize>,
    pub visual_head: Option<usize>,
    pub desired_col: usize,
    pub register: VimRegister,
    pub command_buffer: String,
    pub search_query: String,
    pub search_forward: bool,
    pub highlight_search: bool,
    pub last_find: Option<CharFind>,
    pub last_change: Option<Vec<VimKeyEvent>>,
    pub status_message: Option<String>,
    count: Option<usize>,
    operator: Option<Operator>,
    pending: Option<PendingKey>,
    recording_change: Vec<VimKeyEvent>,
    replaying_dot: bool,
}

impl VimState {
    pub fn new(start_in_insert: bool) -> Self {
        Self {
            mode: if start_in_insert {
                VimMode::Insert
            } else {
                VimMode::Normal
            },
            visual_anchor: None,
            visual_head: None,
            desired_col: 0,
            register: VimRegister::default(),
            command_buffer: String::new(),
            search_query: String::new(),
            search_forward: true,
            highlight_search: false,
            last_find: None,
            last_change: None,
            status_message: None,
            count: None,
            operator: None,
            pending: None,
            recording_change: Vec::new(),
            replaying_dot: false,
        }
    }

    pub fn clear_pending(&mut self) {
        self.count = None;
        self.operator = None;
        self.pending = None;
        self.recording_change.clear();
    }

    /// Pending sequence display for the IDE status bar (e.g. `"2d"`, `"g"`).
    pub fn pending_display(&self) -> String {
        let mut s = String::new();
        if let Some(c) = self.count {
            s.push_str(&c.to_string());
        }
        if let Some(op) = self.operator {
            s.push_str(op.key_char());
        }
        if let Some(p) = self.pending {
            match p {
                PendingKey::G => s.push('g'),
                PendingKey::FindChar(dir, till) => s.push(match (dir, till) {
                    (FindDirection::Forward, false) => 'f',
                    (FindDirection::Backward, false) => 'F',
                    (FindDirection::Forward, true) => 't',
                    (FindDirection::Backward, true) => 'T',
                }),
                PendingKey::ReplaceOne => s.push('r'),
                PendingKey::TextObject { around } => s.push(if around { 'a' } else { 'i' }),
                PendingKey::BracketNext => s.push(']'),
                PendingKey::BracketPrev => s.push('['),
                PendingKey::Z => s.push('z'),
            }
        }
        s
    }

    pub fn clear_visual(&mut self) {
        self.visual_anchor = None;
        self.visual_head = None;
    }

    /// Visual selection byte range (`start..end`) when in Visual or VisualLine mode.
    pub fn visual_range(&self, text: &str, cursor: usize) -> Option<(Range<usize>, bool)> {
        let anchor = self.visual_anchor?;
        let anchor = clamp_char_boundary(text, anchor);
        let cursor = clamp_char_boundary(text, self.visual_head.unwrap_or(cursor));
        match self.mode {
            VimMode::Visual => {
                let (lo, hi) = if anchor <= cursor {
                    (anchor, cursor)
                } else {
                    (cursor, anchor)
                };
                let end = if text.is_empty() {
                    0
                } else {
                    let (ls, le) = line_bounds(text, hi);
                    if ls == le {
                        hi
                    } else {
                        next_char_end(text, clamp_normal_cursor(text, hi))
                    }
                };
                Some((lo.min(text.len())..end.min(text.len()), false))
            }
            VimMode::VisualLine => {
                let (lo, hi) = if anchor <= cursor {
                    (anchor, cursor)
                } else {
                    (cursor, anchor)
                };
                let (start, _) = line_bounds(text, lo);
                let end = line_end_with_newline(text, hi);
                Some((start..end, true))
            }
            _ => None,
        }
    }

    /// Search highlights in the current buffer if search highlight is enabled.
    pub fn search_matches(&self, text: &str) -> Vec<Range<usize>> {
        if !self.highlight_search || self.search_query.is_empty() {
            return Vec::new();
        }
        find_all_matches(text, &self.search_query)
    }

    /// Compute pasted text + cursor position given a buffer, cursor, register payload, and direction.
    pub fn apply_paste(
        &mut self,
        text: &str,
        cursor: usize,
        payload: &VimRegister,
        before: bool,
        count: usize,
    ) -> Option<VimOutcome> {
        if payload.text.is_empty() {
            return None;
        }
        let cursor = clamp_char_boundary(text, cursor);
        let repeat_count = count.max(1);

        if payload.linewise {
            let mut block = payload.text.clone();
            if !block.ends_with('\n') {
                block.push('\n');
            }
            let repeated = block.repeat(repeat_count);
            let (line_start, _) = line_bounds(text, cursor);
            let line_after = line_end_with_newline(text, cursor);

            if before {
                let new_cursor = first_non_ws_offset(&repeated) + line_start;
                return Some(VimOutcome::handled(vec![VimEffect::Replace {
                    range: line_start..line_start,
                    replacement: repeated,
                    new_cursor,
                }]));
            }

            if line_after < text.len() || text.ends_with('\n') {
                let insert_at = line_after;
                let new_cursor = insert_at + first_non_ws_offset(&repeated);
                return Some(VimOutcome::handled(vec![VimEffect::Replace {
                    range: insert_at..insert_at,
                    replacement: repeated,
                    new_cursor,
                }]));
            }

            // Current line is last line without trailing newline: prepend `\n`.
            let trimmed = repeated.trim_end_matches('\n');
            let insert = format!("\n{trimmed}");
            let new_cursor = text.len() + 1 + first_non_ws_offset(trimmed);
            Some(VimOutcome::handled(vec![VimEffect::Replace {
                range: text.len()..text.len(),
                replacement: insert,
                new_cursor,
            }]))
        } else {
            let repeated = payload.text.repeat(repeat_count);
            let (_, line_end) = line_bounds(text, cursor);
            let insert_at = if before || cursor >= line_end {
                cursor.min(line_end)
            } else {
                next_char_end(text, cursor).min(line_end)
            };
            let new_cursor = prev_char_start(
                &format!("{}{}", &text[..insert_at], repeated),
                insert_at + repeated.len(),
            );
            Some(VimOutcome::handled(vec![VimEffect::Replace {
                range: insert_at..insert_at,
                replacement: repeated,
                new_cursor,
            }]))
        }
    }

    pub fn enter_normal_mode(&mut self, text: &str, cursor: usize, was_insert: bool) -> Vec<VimEffect> {
        self.clear_pending();
        let active_cursor = if matches!(self.mode, VimMode::Visual | VimMode::VisualLine) {
            self.visual_head.unwrap_or(cursor)
        } else {
            cursor
        };
        self.clear_visual();
        self.command_buffer.clear();
        self.mode = VimMode::Normal;
        let clamped = clamp_char_boundary(text, active_cursor);
        let new_cursor = if was_insert {
            let (ls, _) = line_bounds(text, clamped);
            if clamped > ls {
                clamp_normal_cursor(text, prev_char_start(text, clamped))
            } else {
                clamp_normal_cursor(text, clamped)
            }
        } else {
            clamp_normal_cursor(text, clamped)
        };
        self.desired_col = col_at_byte(text, new_cursor);
        vec![
            VimEffect::ModeChanged(VimMode::Normal),
            VimEffect::MoveCursor { pos: new_cursor },
        ]
    }

    /// Fast check so pure Insert-mode typing never needs to materialize buffer text.
    pub fn needs_text_for_key(&self, key: &VimKeyEvent) -> bool {
        !(self.mode == VimMode::Insert && !key.is_escape() && self.recording_change.is_empty())
    }

    /// Main entry point for every keystroke routed to Vim.
    pub fn handle_key(&mut self, text: &str, cursor: usize, key: VimKeyEvent) -> VimOutcome {
        let cursor = clamp_char_boundary(text, cursor);

        // Track Insert typing for `.` dot-repeat when not replaying.
        if self.mode == VimMode::Insert {
            if key.is_escape() {
                if !self.replaying_dot && !self.recording_change.is_empty() {
                    self.recording_change.push(key);
                    self.last_change = Some(std::mem::take(&mut self.recording_change));
                }
                let effects = self.enter_normal_mode(text, cursor, true);
                return VimOutcome::handled(effects);
            }
            if !self.replaying_dot && !self.recording_change.is_empty() {
                self.recording_change.push(key);
            }
            return VimOutcome::ignored();
        }

        if let VimMode::Command(kind) = self.mode {
            return self.handle_command_key(text, cursor, kind, key);
        }

        if self.mode == VimMode::Replace {
            return self.handle_replace_key(text, cursor, key);
        }

        // Escape cancels pending counts/operators/visual modes.
        if key.is_escape() {
            if matches!(self.mode, VimMode::Visual | VimMode::VisualLine) {
                return VimOutcome::handled(self.enter_normal_mode(text, cursor, false));
            }
            self.clear_pending();
            self.status_message = None;
            return VimOutcome::handled(vec![VimEffect::MoveCursor {
                pos: clamp_normal_cursor(text, cursor),
            }]);
        }

        if !self.replaying_dot && self.mode == VimMode::Normal {
            // Do not record `.` into `recording_change`.
            if key.text_char() != Some('.') {
                self.recording_change.push(key.clone());
            }
        }

        match self.mode {
            VimMode::Normal => self.handle_normal_key(text, cursor, key),
            VimMode::Visual | VimMode::VisualLine => self.handle_visual_key(text, cursor, key),
            _ => VimOutcome::ignored(),
        }
    }

    /// Execute a synthetic Insert keystroke during `.` dot-repeat playback.
    pub fn handle_synthetic_insert_key(
        &mut self,
        text: &str,
        cursor: usize,
        key: &VimKeyEvent,
    ) -> VimOutcome {
        if key.is_escape() {
            return VimOutcome::handled(self.enter_normal_mode(text, cursor, true));
        }
        let cursor = clamp_char_boundary(text, cursor);
        if key.key == "enter" || key.key == "return" {
            return VimOutcome::handled(vec![VimEffect::Replace {
                range: cursor..cursor,
                replacement: "\n".to_string(),
                new_cursor: cursor + 1,
            }]);
        }
        if key.key == "backspace" && cursor > 0 {
            let prev = prev_char_start(text, cursor);
            return VimOutcome::handled(vec![VimEffect::Replace {
                range: prev..cursor,
                replacement: String::new(),
                new_cursor: prev,
            }]);
        }
        if let Some(ch) = key.text_char() {
            let s = ch.to_string();
            let len = s.len();
            return VimOutcome::handled(vec![VimEffect::Replace {
                range: cursor..cursor,
                replacement: s,
                new_cursor: cursor + len,
            }]);
        }
        VimOutcome::handled(Vec::new())
    }

    fn handle_replace_key(&mut self, text: &str, cursor: usize, key: VimKeyEvent) -> VimOutcome {
        if key.is_escape() {
            return VimOutcome::handled(self.enter_normal_mode(text, cursor, true));
        }
        if key.key == "backspace" {
            let (ls, _) = line_bounds(text, cursor);
            let target = if cursor > ls {
                prev_char_start(text, cursor)
            } else {
                ls
            };
            self.desired_col = col_at_byte(text, target);
            return VimOutcome::handled(vec![VimEffect::MoveCursor { pos: target }]);
        }
        if key.key == "enter" || key.key == "return" {
            return VimOutcome::handled(vec![VimEffect::Replace {
                range: cursor..cursor,
                replacement: "\n".to_string(),
                new_cursor: cursor + 1,
            }]);
        }
        if let Some(ch) = key.text_char() {
            let (_, line_end) = line_bounds(text, cursor);
            let range = if cursor < line_end {
                cursor..next_char_end(text, cursor)
            } else {
                cursor..cursor
            };
            let s = ch.to_string();
            let new_cursor = cursor + s.len();
            return VimOutcome::handled(vec![VimEffect::Replace {
                range,
                replacement: s,
                new_cursor,
            }]);
        }
        VimOutcome::handled(Vec::new())
    }

    fn handle_command_key(
        &mut self,
        text: &str,
        cursor: usize,
        kind: CommandKind,
        key: VimKeyEvent,
    ) -> VimOutcome {
        if key.is_escape() {
            self.command_buffer.clear();
            self.mode = VimMode::Normal;
            return VimOutcome::handled(vec![VimEffect::ModeChanged(VimMode::Normal)]);
        }

        if key.key == "backspace" {
            if self.command_buffer.is_empty() {
                self.mode = VimMode::Normal;
                return VimOutcome::handled(vec![VimEffect::ModeChanged(VimMode::Normal)]);
            }
            self.command_buffer.pop();
            return VimOutcome::handled(Vec::new());
        }

        if key.key == "enter" || key.key == "return" {
            let cmd = self.command_buffer.trim().to_string();
            self.command_buffer.clear();
            self.mode = VimMode::Normal;
            let mut effects = vec![VimEffect::ModeChanged(VimMode::Normal)];
            match kind {
                CommandKind::Ex => effects.extend(self.execute_ex_command(text, &cmd)),
                CommandKind::SearchForward | CommandKind::SearchBackward => {
                    let forward = matches!(kind, CommandKind::SearchForward);
                    if !cmd.is_empty() {
                        self.search_query = cmd;
                        self.search_forward = forward;
                        self.highlight_search = true;
                        if let Some(hit) =
                            find_next_match(text, cursor, &self.search_query, forward, true)
                        {
                            self.desired_col = col_at_byte(text, hit.start);
                            effects.push(VimEffect::PushJumpPoint);
                            effects.push(VimEffect::MoveCursor { pos: hit.start });
                        } else {
                            let msg = format!("Pattern not found: {}", self.search_query);
                            self.status_message = Some(msg.clone());
                            effects.push(VimEffect::StatusMessage(msg));
                        }
                    }
                }
            }
            return VimOutcome::handled(effects);
        }

        if let Some(ch) = key.text_char() {
            self.command_buffer.push(ch);
            // Live incremental jump preview during `/` or `?` without modifying search history yet.
            if matches!(kind, CommandKind::SearchForward | CommandKind::SearchBackward)
                && !self.command_buffer.is_empty()
            {
                let forward = matches!(kind, CommandKind::SearchForward);
                if let Some(hit) =
                    find_next_match(text, cursor, &self.command_buffer, forward, true)
                {
                    return VimOutcome::handled(vec![VimEffect::MoveCursor { pos: hit.start }]);
                }
            }
            return VimOutcome::handled(Vec::new());
        }

        VimOutcome::handled(Vec::new())
    }

    fn execute_ex_command(&mut self, text: &str, raw: &str) -> Vec<VimEffect> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }

        if let Ok(line_num) = trimmed.parse::<usize>() {
            let idx = line_num.saturating_sub(1);
            let pos = line_first_non_ws(text, line_start_at_index(text, idx));
            self.desired_col = col_at_byte(text, pos);
            return vec![VimEffect::PushJumpPoint, VimEffect::MoveCursor { pos }];
        }

        let (cmd, arg) = match trimmed.split_once(char::is_whitespace) {
            Some((c, a)) => (c, Some(a.trim().to_string())),
            None => (trimmed, None),
        };

        match cmd {
            "w" | "write" => vec![VimEffect::SaveFile],
            "wq" | "x" | "xit" => vec![VimEffect::SaveAndClose],
            "q" | "quit" => vec![VimEffect::CloseTab { force: false }],
            "q!" | "quit!" => vec![VimEffect::CloseTab { force: true }],
            "e" | "edit" => vec![VimEffect::OpenFile(arg.filter(|s| !s.is_empty()))],
            "noh" | "nohlsearch" => {
                self.highlight_search = false;
                Vec::new()
            }
            "def" | "gd" => vec![VimEffect::GoToDefinition],
            "decl" | "gD" => vec![VimEffect::GoToDeclaration],
            "type" | "typedef" | "gy" => vec![VimEffect::GoToTypeDefinition],
            "impl" | "gI" => vec![VimEffect::GoToImplementation],
            "refs" | "gr" => vec![VimEffect::FindReferences],
            "hover" | "K" => vec![VimEffect::ShowHover],
            "outline" | "symbols" | "gO" => vec![VimEffect::OpenOutline],
            "jumps" => vec![VimEffect::ShowJumpList],
            "tabs" => vec![VimEffect::ListTabs],
            "bn" | "bnext" | "tabn" | "tabnext" => vec![VimEffect::NextTab],
            "bp" | "bprev" | "bprevious" | "tabp" | "tabprev" | "tabprevious" => {
                vec![VimEffect::PrevTab]
            }
            "fim" => vec![VimEffect::TriggerFim],
            "ask" => vec![VimEffect::AskAi {
                explain: false,
                question: arg.filter(|s| !s.is_empty()),
            }],
            "explain" => vec![VimEffect::AskAi {
                explain: true,
                question: arg.filter(|s| !s.is_empty()),
            }],
            "set" => match arg.as_deref() {
                Some("novim") | Some("vim!") => vec![VimEffect::SetVimEnabled(false)],
                Some("vim") => vec![VimEffect::SetVimEnabled(true)],
                Some("inlay") | Some("inlay!") => vec![VimEffect::SetInlayHints(true)],
                Some("noinlay") => vec![VimEffect::SetInlayHints(false)],
                Some("inlinedoc") | Some("hoverdoc") => vec![VimEffect::SetInlineHoverDocs(true)],
                Some("noinlinedoc") | Some("nohoverdoc") => {
                    vec![VimEffect::SetInlineHoverDocs(false)]
                }
                Some(s)
                    if s.starts_with("scrolloff=")
                        || s.starts_with("so=")
                        || s.starts_with("scrollpadding=") =>
                {
                    let val = s.split_once('=').map(|(_, v)| v.trim()).unwrap_or("");
                    match val.parse::<usize>() {
                        Ok(n) => vec![VimEffect::SetScrollPadding(n.min(20))],
                        Err(_) => {
                            let msg = format!("Invalid scrolloff value: {val}");
                            self.status_message = Some(msg.clone());
                            vec![VimEffect::StatusMessage(msg)]
                        }
                    }
                }
                _ => {
                    let msg = format!(":set {} is not supported", arg.unwrap_or_default());
                    self.status_message = Some(msg.clone());
                    vec![VimEffect::StatusMessage(msg)]
                }
            },
            _ => {
                let msg = format!("Not an editor command: {trimmed}");
                self.status_message = Some(msg.clone());
                vec![VimEffect::StatusMessage(msg)]
            }
        }
    }

    fn handle_normal_key(&mut self, text: &str, cursor: usize, key: VimKeyEvent) -> VimOutcome {
        // Pending single-key completions take highest priority.
        if let Some(pending) = self.pending.take() {
            return self.complete_pending_normal(text, cursor, pending, key);
        }

        // Ctrl combinations in Normal mode.
        if key.ctrl && !key.alt {
            self.clear_pending();
            return match key.key.as_str() {
                "r" => VimOutcome::handled(vec![VimEffect::Redo]),
                "o" => VimOutcome::handled(vec![VimEffect::JumpBack]),
                "i" => VimOutcome::handled(vec![VimEffect::JumpForward]),
                "d" => VimOutcome::handled(vec![VimEffect::ScrollHalfPage {
                    down: true,
                    count: 1,
                }]),
                "u" => VimOutcome::handled(vec![VimEffect::ScrollHalfPage {
                    down: false,
                    count: 1,
                }]),
                "f" => VimOutcome::handled(vec![VimEffect::ScrollHalfPage {
                    down: true,
                    count: 2,
                }]),
                "b" => VimOutcome::handled(vec![VimEffect::ScrollHalfPage {
                    down: false,
                    count: 2,
                }]),
                _ => VimOutcome::ignored(),
            };
        }

        if key.alt {
            self.clear_pending();
            return VimOutcome::ignored();
        }

        // Arrow keys in Normal mode work cleanly as basic motions.
        match key.key.as_str() {
            "left" => {
                let target = move_left(text, cursor, self.take_count());
                return self.finish_motion_normal(text, cursor, target, false, false);
            }
            "right" => {
                let target = move_right(text, cursor, self.take_count(), false);
                return self.finish_motion_normal(text, cursor, target, true, false);
            }
            "up" => {
                let count = self.take_count() as isize;
                return self.finish_motion_normal(
                    text,
                    cursor,
                    move_vertical(text, cursor, -count, self.desired_col, true),
                    false,
                    true,
                );
            }
            "down" => {
                let count = self.take_count() as isize;
                return self.finish_motion_normal(
                    text,
                    cursor,
                    move_vertical(text, cursor, count, self.desired_col, true),
                    false,
                    true,
                );
            }
            "enter" | "return" => {
                let count = self.take_count();
                let next_line = line_index_at(text, cursor) + count;
                let pos = line_first_non_ws(text, line_start_at_index(text, next_line));
                return self.finish_motion_normal(text, cursor, pos, false, true);
            }
            "backspace" => {
                let count = self.take_count();
                let mut cur = cursor;
                for _ in 0..count {
                    cur = if cur > 0 {
                        prev_char_start(text, cur)
                    } else {
                        0
                    };
                }
                return self.finish_motion_normal(text, cursor, clamp_normal_cursor(text, cur), false, false);
            }
            _ => {}
        }

        let Some(ch) = key.text_char() else {
            self.clear_pending();
            return VimOutcome::ignored();
        };

        // Numeric prefix (`1-9`, or `0` once count already started).
        if let Some(digit) = ch.to_digit(10) {
            if digit > 0 || self.count.is_some() {
                let next = self
                    .count
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit as usize)
                    .min(100_000);
                self.count = Some(next);
                return VimOutcome::handled(Vec::new());
            }
        }

        // Handle doubled operator lines (`dd`, `yy`, `cc`, `>>`, `<<`).
        if let Some(op) = self.operator {
            let doubled = matches!(
                (op, ch),
                (Operator::Delete, 'd')
                    | (Operator::Yank, 'y')
                    | (Operator::Change, 'c')
                    | (Operator::Indent, '>')
                    | (Operator::Outdent, '<')
                    | (Operator::ToggleCase, '~')
            );
            if doubled {
                let count = self.take_count();
                self.operator = None;
                return self.apply_linewise_operator(text, cursor, op, count);
            }
            if ch == 'i' || ch == 'a' {
                self.pending = Some(PendingKey::TextObject { around: ch == 'a' });
                return VimOutcome::handled(Vec::new());
            }
        }

        match ch {
            // Prefix keys
            'g' => {
                self.pending = Some(PendingKey::G);
                VimOutcome::handled(Vec::new())
            }
            'f' => {
                self.pending = Some(PendingKey::FindChar(FindDirection::Forward, false));
                VimOutcome::handled(Vec::new())
            }
            'F' => {
                self.pending = Some(PendingKey::FindChar(FindDirection::Backward, false));
                VimOutcome::handled(Vec::new())
            }
            't' => {
                self.pending = Some(PendingKey::FindChar(FindDirection::Forward, true));
                VimOutcome::handled(Vec::new())
            }
            'T' => {
                self.pending = Some(PendingKey::FindChar(FindDirection::Backward, true));
                VimOutcome::handled(Vec::new())
            }
            ']' => {
                self.pending = Some(PendingKey::BracketNext);
                VimOutcome::handled(Vec::new())
            }
            '[' => {
                self.pending = Some(PendingKey::BracketPrev);
                VimOutcome::handled(Vec::new())
            }
            'z' if self.operator.is_none() => {
                self.pending = Some(PendingKey::Z);
                VimOutcome::handled(Vec::new())
            }

            // Operators (when no operator is active yet)
            'd' if self.operator.is_none() => {
                self.operator = Some(Operator::Delete);
                VimOutcome::handled(Vec::new())
            }
            'c' if self.operator.is_none() => {
                self.operator = Some(Operator::Change);
                VimOutcome::handled(Vec::new())
            }
            'y' if self.operator.is_none() => {
                self.operator = Some(Operator::Yank);
                VimOutcome::handled(Vec::new())
            }
            '>' if self.operator.is_none() => {
                self.operator = Some(Operator::Indent);
                VimOutcome::handled(Vec::new())
            }
            '<' if self.operator.is_none() => {
                self.operator = Some(Operator::Outdent);
                VimOutcome::handled(Vec::new())
            }

            // Core motions
            'h' => {
                let target = move_left(text, cursor, self.take_count());
                self.finish_motion_normal(text, cursor, target, false, false)
            }
            'l' => {
                let allow_end = self.operator.is_some();
                let target = move_right(text, cursor, self.take_count(), allow_end);
                self.finish_motion_normal(text, cursor, target, false, false)
            }
            'j' => {
                let count = self.take_count() as isize;
                let target = move_vertical(text, cursor, count, self.desired_col, true);
                self.finish_motion_normal(text, cursor, target, false, true)
            }
            'k' => {
                let count = self.take_count() as isize;
                let target = move_vertical(text, cursor, -count, self.desired_col, true);
                self.finish_motion_normal(text, cursor, target, false, true)
            }
            '0' => {
                let (ls, _) = line_bounds(text, cursor);
                self.count = None;
                self.finish_motion_normal(text, cursor, ls, false, false)
            }
            '^' => {
                self.count = None;
                let target = line_first_non_ws(text, cursor);
                self.finish_motion_normal(text, cursor, target, false, false)
            }
            '$' => {
                let count = self.take_count();
                let target_line = line_index_at(text, cursor) + count.saturating_sub(1);
                let line_start = line_start_at_index(text, target_line);
                let (_, le) = line_bounds(text, line_start);
                let target = if self.operator.is_some() {
                    le
                } else {
                    clamp_normal_cursor(text, le)
                };
                self.finish_motion_normal(text, cursor, target, false, false)
            }
            'w' | 'W' => {
                let count = self.take_count();
                let big = ch == 'W';
                let purpose = match self.operator {
                    Some(Operator::Change) => WordMotionPurpose::Change,
                    Some(_) => WordMotionPurpose::Delete,
                    None => WordMotionPurpose::Move,
                };
                let target = move_word_forward(text, cursor, count, big, purpose);
                self.finish_motion_normal(text, cursor, target, false, false)
            }
            'b' | 'B' => {
                let target = move_word_backward(text, cursor, self.take_count(), ch == 'B');
                self.finish_motion_normal(text, cursor, target, false, false)
            }
            'e' | 'E' => {
                let target = move_word_end(text, cursor, self.take_count(), ch == 'E');
                self.finish_motion_normal(text, cursor, target, true, false)
            }
            'G' => {
                let target_idx = match self.count.take() {
                    Some(n) => n.saturating_sub(1),
                    None => total_lines(text).saturating_sub(1),
                };
                let target = line_first_non_ws(text, line_start_at_index(text, target_idx));
                self.finish_jump_normal(text, cursor, target, false, true)
            }
            '%' => {
                self.count = None;
                if let Some(target) = find_matching_bracket(text, cursor) {
                    self.finish_jump_normal(text, cursor, target, true, false)
                } else {
                    self.clear_pending();
                    VimOutcome::handled(Vec::new())
                }
            }
            '{' => {
                let target = move_paragraph(text, cursor, self.take_count(), false);
                self.finish_motion_normal(text, cursor, target, false, true)
            }
            '}' => {
                let target = move_paragraph(text, cursor, self.take_count(), true);
                self.finish_motion_normal(text, cursor, target, false, true)
            }
            ';' | ',' => {
                let count = self.take_count();
                if let Some(find) = self.last_find {
                    if let Some(target) =
                        repeat_char_find(text, cursor, find, ch == ',', count)
                    {
                        let inclusive = find.direction == FindDirection::Forward;
                        return self.finish_motion_normal(text, cursor, target, inclusive, false);
                    }
                }
                self.clear_pending();
                VimOutcome::handled(Vec::new())
            }

            // Search motions (`/`, `?`, `n`, `N`, `*`, `#`)
            '/' => {
                self.clear_pending();
                self.command_buffer.clear();
                self.mode = VimMode::Command(CommandKind::SearchForward);
                VimOutcome::handled(vec![VimEffect::ModeChanged(self.mode)])
            }
            '?' => {
                self.clear_pending();
                self.command_buffer.clear();
                self.mode = VimMode::Command(CommandKind::SearchBackward);
                VimOutcome::handled(vec![VimEffect::ModeChanged(self.mode)])
            }
            ':' => {
                self.clear_pending();
                self.command_buffer.clear();
                self.mode = VimMode::Command(CommandKind::Ex);
                VimOutcome::handled(vec![VimEffect::ModeChanged(self.mode)])
            }
            'n' | 'N' => {
                let count = self.take_count();
                if !self.search_query.is_empty() {
                    self.highlight_search = true;
                    let forward = if ch == 'N' {
                        !self.search_forward
                    } else {
                        self.search_forward
                    };
                    let mut pos = cursor;
                    let mut found = None;
                    for _ in 0..count {
                        if let Some(hit) =
                            find_next_match(text, pos, &self.search_query, forward, true)
                        {
                            pos = hit.start;
                            found = Some(hit);
                        } else {
                            break;
                        }
                    }
                    if let Some(hit) = found {
                        return self.finish_jump_normal(text, cursor, hit.start, false, false);
                    }
                }
                self.clear_pending();
                VimOutcome::handled(Vec::new())
            }
            '*' | '#' => {
                self.clear_pending();
                if let Some((_, word)) = word_under_cursor(text, cursor) {
                    self.search_query = word.to_string();
                    self.search_forward = ch == '*';
                    self.highlight_search = true;
                    if let Some(hit) = find_next_match(
                        text,
                        cursor,
                        &self.search_query,
                        self.search_forward,
                        true,
                    ) {
                        self.desired_col = col_at_byte(text, hit.start);
                        return VimOutcome::handled(vec![
                            VimEffect::PushJumpPoint,
                            VimEffect::MoveCursor { pos: hit.start },
                        ]);
                    }
                }
                VimOutcome::handled(Vec::new())
            }

            // Remaining actions clear any active operator if invalid.
            _ if self.operator.is_some() => {
                self.clear_pending();
                VimOutcome::handled(Vec::new())
            }

            // Insert mode entry
            'i' => {
                self.count = None;
                self.mode = VimMode::Insert;
                VimOutcome::handled(vec![
                    VimEffect::ModeChanged(VimMode::Insert),
                    VimEffect::MoveCursor {
                        pos: clamp_normal_cursor(text, cursor),
                    },
                ])
            }
            'I' => {
                self.count = None;
                self.mode = VimMode::Insert;
                let pos = line_first_non_ws(text, cursor);
                VimOutcome::handled(vec![
                    VimEffect::ModeChanged(VimMode::Insert),
                    VimEffect::MoveCursor { pos },
                ])
            }
            'a' => {
                self.count = None;
                self.mode = VimMode::Insert;
                let (_, le) = line_bounds(text, cursor);
                let pos = if cursor < le {
                    next_char_end(text, cursor).min(le)
                } else {
                    le
                };
                VimOutcome::handled(vec![
                    VimEffect::ModeChanged(VimMode::Insert),
                    VimEffect::MoveCursor { pos },
                ])
            }
            'A' => {
                self.count = None;
                self.mode = VimMode::Insert;
                let (_, le) = line_bounds(text, cursor);
                VimOutcome::handled(vec![
                    VimEffect::ModeChanged(VimMode::Insert),
                    VimEffect::MoveCursor { pos: le },
                ])
            }
            'o' | 'O' => {
                self.count = None;
                self.mode = VimMode::Insert;
                let (ls, le) = line_bounds(text, cursor);
                let indent: String = text[ls..le]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                if ch == 'o' {
                    let insert = format!("\n{indent}");
                    let new_cursor = le + insert.len();
                    VimOutcome::handled(vec![
                        VimEffect::Replace {
                            range: le..le,
                            replacement: insert,
                            new_cursor,
                        },
                        VimEffect::ModeChanged(VimMode::Insert),
                    ])
                } else {
                    let insert = format!("{indent}\n");
                    let new_cursor = ls + indent.len();
                    VimOutcome::handled(vec![
                        VimEffect::Replace {
                            range: ls..ls,
                            replacement: insert,
                            new_cursor,
                        },
                        VimEffect::ModeChanged(VimMode::Insert),
                    ])
                }
            }
            'R' => {
                self.clear_pending();
                self.mode = VimMode::Replace;
                VimOutcome::handled(vec![VimEffect::ModeChanged(VimMode::Replace)])
            }

            // Single-key edits in Normal mode
            'x' => {
                let count = self.take_count();
                let (ls, le) = line_bounds(text, cursor);
                if ls == le || cursor >= le {
                    self.clear_pending();
                    return VimOutcome::handled(Vec::new());
                }
                let mut end = cursor;
                for _ in 0..count {
                    if end >= le {
                        break;
                    }
                    end = next_char_end(text, end).min(le);
                }
                self.apply_range_edit(text, cursor..end, Operator::Delete, false)
            }
            'X' => {
                let count = self.take_count();
                let (ls, _) = line_bounds(text, cursor);
                if cursor <= ls {
                    self.clear_pending();
                    return VimOutcome::handled(Vec::new());
                }
                let mut start = cursor;
                for _ in 0..count {
                    if start <= ls {
                        break;
                    }
                    start = prev_char_start(text, start).max(ls);
                }
                self.apply_range_edit(text, start..cursor, Operator::Delete, false)
            }
            's' => {
                let count = self.take_count();
                let (ls, le) = line_bounds(text, cursor);
                if ls == le || cursor >= le {
                    self.mode = VimMode::Insert;
                    return VimOutcome::handled(vec![VimEffect::ModeChanged(VimMode::Insert)]);
                }
                let mut end = cursor;
                for _ in 0..count {
                    if end >= le {
                        break;
                    }
                    end = next_char_end(text, end).min(le);
                }
                self.apply_range_edit(text, cursor..end, Operator::Change, false)
            }
            'S' => {
                let count = self.take_count();
                self.apply_linewise_operator(text, cursor, Operator::Change, count)
            }
            'D' => {
                self.count = None;
                let (_, le) = line_bounds(text, cursor);
                self.apply_range_edit(text, cursor..le, Operator::Delete, false)
            }
            'C' => {
                self.count = None;
                let (_, le) = line_bounds(text, cursor);
                self.apply_range_edit(text, cursor..le, Operator::Change, false)
            }
            'Y' => {
                let count = self.take_count();
                self.apply_linewise_operator(text, cursor, Operator::Yank, count)
            }
            'r' => {
                self.pending = Some(PendingKey::ReplaceOne);
                VimOutcome::handled(Vec::new())
            }
            '~' => {
                let count = self.take_count();
                let (_, le) = line_bounds(text, cursor);
                if cursor >= le {
                    self.clear_pending();
                    return VimOutcome::handled(Vec::new());
                }
                let mut end = cursor;
                for _ in 0..count {
                    if end >= le {
                        break;
                    }
                    end = next_char_end(text, end).min(le);
                }
                let toggled: String = text[cursor..end]
                    .chars()
                    .map(|c| {
                        if c.is_lowercase() {
                            c.to_uppercase().collect::<String>()
                        } else if c.is_uppercase() {
                            c.to_lowercase().collect::<String>()
                        } else {
                            c.to_string()
                        }
                    })
                    .collect();
                let new_cursor = clamp_normal_cursor(text, end.min(le));
                self.commit_non_insert_change();
                self.desired_col = col_at_byte(text, new_cursor);
                VimOutcome::handled(vec![VimEffect::Replace {
                    range: cursor..end,
                    replacement: toggled,
                    new_cursor,
                }])
            }
            'J' => {
                let count = self.take_count().max(1);
                let mut res = self.join_lines(text, cursor, count);
                if res.consumed {
                    self.commit_non_insert_change();
                } else {
                    self.clear_pending();
                }
                res.consumed = true;
                res
            }

            // Clipboard / Undo / Repeat / Visual / IDE Code Nav
            'p' | 'P' => {
                let count = self.take_count();
                self.commit_non_insert_change();
                VimOutcome::handled(vec![VimEffect::RequestPaste {
                    before: ch == 'P',
                    count,
                }])
            }
            'u' => {
                self.clear_pending();
                VimOutcome::handled(vec![VimEffect::Undo])
            }
            '.' => {
                self.clear_pending();
                VimOutcome::handled(vec![VimEffect::RepeatLastChange])
            }
            'v' => {
                self.clear_pending();
                let head = clamp_normal_cursor(text, cursor);
                self.visual_anchor = Some(head);
                self.visual_head = Some(head);
                self.mode = VimMode::Visual;
                VimOutcome::handled(vec![
                    VimEffect::ModeChanged(VimMode::Visual),
                    VimEffect::SetSelection {
                        anchor: head,
                        head,
                        linewise: false,
                    },
                ])
            }
            'V' => {
                self.clear_pending();
                let pos = clamp_normal_cursor(text, cursor);
                self.visual_anchor = Some(pos);
                self.visual_head = Some(pos);
                self.mode = VimMode::VisualLine;
                VimOutcome::handled(vec![
                    VimEffect::ModeChanged(VimMode::VisualLine),
                    VimEffect::SetSelection {
                        anchor: pos,
                        head: pos,
                        linewise: true,
                    },
                ])
            }
            'K' => {
                self.clear_pending();
                VimOutcome::handled(vec![VimEffect::ShowHover])
            }
            _ => {
                self.clear_pending();
                VimOutcome::handled(Vec::new())
            }
        }
    }

    fn complete_pending_normal(
        &mut self,
        text: &str,
        cursor: usize,
        pending: PendingKey,
        key: VimKeyEvent,
    ) -> VimOutcome {
        let Some(ch) = key.text_char() else {
            self.clear_pending();
            return VimOutcome::handled(Vec::new());
        };

        match pending {
            PendingKey::G => match ch {
                'g' => {
                    let target_idx = match self.count.take() {
                        Some(n) => n.saturating_sub(1),
                        None => 0,
                    };
                    let target = line_first_non_ws(text, line_start_at_index(text, target_idx));
                    self.finish_jump_normal(text, cursor, target, false, true)
                }
                '_' => {
                    self.count = None;
                    let (ls, le) = line_bounds(text, cursor);
                    let mut target = le;
                    for (rel, c) in text[ls..le].char_indices().rev() {
                        if !c.is_whitespace() {
                            target = ls + rel;
                            break;
                        }
                    }
                    let inclusive = self.operator.is_some();
                    self.finish_motion_normal(text, cursor, clamp_normal_cursor(text, target), inclusive, false)
                }
                'd' if self.operator.is_none() => {
                    self.clear_pending();
                    VimOutcome::handled(vec![VimEffect::GoToDefinition])
                }
                'D' if self.operator.is_none() => {
                    self.clear_pending();
                    VimOutcome::handled(vec![VimEffect::GoToDeclaration])
                }
                'y' if self.operator.is_none() => {
                    self.clear_pending();
                    VimOutcome::handled(vec![VimEffect::GoToTypeDefinition])
                }
                'I' if self.operator.is_none() => {
                    self.clear_pending();
                    VimOutcome::handled(vec![VimEffect::GoToImplementation])
                }
                'r' if self.operator.is_none() => {
                    self.clear_pending();
                    VimOutcome::handled(vec![VimEffect::FindReferences])
                }
                'f' if self.operator.is_none() => {
                    self.clear_pending();
                    VimOutcome::handled(vec![VimEffect::GoToFile])
                }
                'i' if self.operator.is_none() => {
                    self.clear_pending();
                    self.mode = VimMode::Insert;
                    VimOutcome::handled(vec![
                        VimEffect::JumpLastInsert,
                        VimEffect::ModeChanged(VimMode::Insert),
                    ])
                }
                'a' if self.operator.is_none() => {
                    self.clear_pending();
                    VimOutcome::handled(vec![VimEffect::CodeActions])
                }
                'O' if self.operator.is_none() => {
                    self.clear_pending();
                    VimOutcome::handled(vec![VimEffect::OpenOutline])
                }
                '?' if self.operator.is_none() => {
                    self.clear_pending();
                    VimOutcome::handled(vec![VimEffect::AskAi {
                        explain: true,
                        question: None,
                    }])
                }
                't' if self.operator.is_none() => {
                    let count = self.count.take();
                    self.clear_pending();
                    match count {
                        Some(n) => VimOutcome::handled(vec![VimEffect::GoToTab(n.max(1))]),
                        None => VimOutcome::handled(vec![VimEffect::NextTab]),
                    }
                }
                'T' if self.operator.is_none() => {
                    self.clear_pending();
                    VimOutcome::handled(vec![VimEffect::PrevTab])
                }
                'u' if self.operator.is_none() => {
                    self.operator = Some(Operator::Lowercase);
                    VimOutcome::handled(Vec::new())
                }
                'U' if self.operator.is_none() => {
                    self.operator = Some(Operator::Uppercase);
                    VimOutcome::handled(Vec::new())
                }
                '~' if self.operator.is_none() => {
                    self.operator = Some(Operator::ToggleCase);
                    VimOutcome::handled(Vec::new())
                }
                _ => {
                    self.clear_pending();
                    VimOutcome::handled(Vec::new())
                }
            },
            PendingKey::FindChar(dir, till) => {
                let count = self.take_count();
                let find = CharFind {
                    ch,
                    direction: dir,
                    till,
                };
                self.last_find = Some(find);
                if let Some(target) = repeat_char_find(text, cursor, find, false, count) {
                    let inclusive = dir == FindDirection::Forward;
                    self.finish_motion_normal(text, cursor, target, inclusive, false)
                } else {
                    self.clear_pending();
                    VimOutcome::handled(Vec::new())
                }
            }
            PendingKey::ReplaceOne => {
                let count = self.take_count();
                let (_, le) = line_bounds(text, cursor);
                if cursor >= le {
                    self.clear_pending();
                    return VimOutcome::handled(Vec::new());
                }
                let mut end = cursor;
                let mut chars_available = 0usize;
                for _ in 0..count {
                    if end >= le {
                        break;
                    }
                    end = next_char_end(text, end).min(le);
                    chars_available += 1;
                }
                if chars_available < count {
                    self.clear_pending();
                    return VimOutcome::handled(Vec::new());
                }
                let replacement = ch.to_string().repeat(count);
                let new_cursor = prev_char_start(
                    &format!("{}{}", &text[..cursor], replacement),
                    cursor + replacement.len(),
                );
                self.commit_non_insert_change();
                self.desired_col = col_at_byte(text, new_cursor);
                VimOutcome::handled(vec![VimEffect::Replace {
                    range: cursor..end,
                    replacement,
                    new_cursor,
                }])
            }
            PendingKey::TextObject { around } => {
                self.count = None;
                let Some(op) = self.operator.take() else {
                    self.clear_pending();
                    return VimOutcome::handled(Vec::new());
                };
                if let Some(range) = text_object_range(text, cursor, around, ch) {
                    self.apply_range_edit(text, range, op, false)
                } else {
                    self.clear_pending();
                    VimOutcome::handled(Vec::new())
                }
            }
            PendingKey::BracketNext => {
                let count = self.take_count();
                self.clear_pending();
                match ch {
                    'd' => VimOutcome::handled(vec![VimEffect::NextDiagnostic {
                        errors_only: false,
                    }]),
                    'e' => VimOutcome::handled(vec![VimEffect::NextDiagnostic { errors_only: true }]),
                    's' | ']' | 'c' => VimOutcome::handled(vec![VimEffect::NextSymbol]),
                    'b' => VimOutcome::handled(vec![VimEffect::NextTab]),
                    '}' => {
                        if let Some(target) = find_enclosing_delim(text, cursor, '{', '}', true, count) {
                            self.finish_jump_normal(text, cursor, target, true, false)
                        } else {
                            VimOutcome::handled(Vec::new())
                        }
                    }
                    ')' => {
                        if let Some(target) = find_enclosing_delim(text, cursor, '(', ')', true, count) {
                            self.finish_jump_normal(text, cursor, target, true, false)
                        } else {
                            VimOutcome::handled(Vec::new())
                        }
                    }
                    _ => VimOutcome::handled(Vec::new()),
                }
            }
            PendingKey::BracketPrev => {
                let count = self.take_count();
                self.clear_pending();
                match ch {
                    'd' => VimOutcome::handled(vec![VimEffect::PrevDiagnostic {
                        errors_only: false,
                    }]),
                    'e' => VimOutcome::handled(vec![VimEffect::PrevDiagnostic { errors_only: true }]),
                    's' | '[' | 'c' => VimOutcome::handled(vec![VimEffect::PrevSymbol]),
                    'b' => VimOutcome::handled(vec![VimEffect::PrevTab]),
                    '{' => {
                        if let Some(target) = find_enclosing_delim(text, cursor, '{', '}', false, count) {
                            self.finish_jump_normal(text, cursor, target, false, false)
                        } else {
                            VimOutcome::handled(Vec::new())
                        }
                    }
                    '(' => {
                        if let Some(target) = find_enclosing_delim(text, cursor, '(', ')', false, count) {
                            self.finish_jump_normal(text, cursor, target, false, false)
                        } else {
                            VimOutcome::handled(Vec::new())
                        }
                    }
                    _ => VimOutcome::handled(Vec::new()),
                }
            }
            PendingKey::Z => {
                self.clear_pending();
                match ch {
                    'z' => VimOutcome::handled(vec![VimEffect::AlignCursor(ScrollAlign::Center)]),
                    't' => VimOutcome::handled(vec![VimEffect::AlignCursor(ScrollAlign::Top)]),
                    'b' => VimOutcome::handled(vec![VimEffect::AlignCursor(ScrollAlign::Bottom)]),
                    _ => VimOutcome::handled(Vec::new()),
                }
            }
        }
    }

    fn handle_visual_key(&mut self, text: &str, cursor: usize, key: VimKeyEvent) -> VimOutcome {
        let cursor = clamp_char_boundary(text, self.visual_head.unwrap_or(cursor));
        if let Some(pending) = self.pending.take() {
            return self.complete_pending_visual(text, cursor, pending, key);
        }

        if key.ctrl && !key.alt {
            return match key.key.as_str() {
                "d" => VimOutcome::handled(vec![VimEffect::ScrollHalfPage {
                    down: true,
                    count: 1,
                }]),
                "u" => VimOutcome::handled(vec![VimEffect::ScrollHalfPage {
                    down: false,
                    count: 1,
                }]),
                _ => VimOutcome::ignored(),
            };
        }

        if let Some(ch) = key.text_char() {
            if let Some(digit) = ch.to_digit(10) {
                if digit > 0 || self.count.is_some() {
                    self.count = Some(
                        self.count
                            .unwrap_or(0)
                            .saturating_mul(10)
                            .saturating_add(digit as usize)
                            .min(100_000),
                    );
                    return VimOutcome::handled(Vec::new());
                }
            }

            let linewise = self.mode == VimMode::VisualLine;
            match ch {
                'v' if !linewise => {
                    return VimOutcome::handled(self.enter_normal_mode(text, cursor, false));
                }
                'v' if linewise => {
                    self.mode = VimMode::Visual;
                    return self.update_visual_selection(text, cursor);
                }
                'V' if linewise => {
                    return VimOutcome::handled(self.enter_normal_mode(text, cursor, false));
                }
                'V' if !linewise => {
                    self.mode = VimMode::VisualLine;
                    return self.update_visual_selection(text, cursor);
                }
                'o' => {
                    if let Some(anchor) = self.visual_anchor.replace(cursor) {
                        self.desired_col = col_at_byte(text, anchor);
                        return self.update_visual_selection(text, anchor);
                    }
                }
                'g' => {
                    self.pending = Some(PendingKey::G);
                    return VimOutcome::handled(Vec::new());
                }
                'f' => {
                    self.pending = Some(PendingKey::FindChar(FindDirection::Forward, false));
                    return VimOutcome::handled(Vec::new());
                }
                'F' => {
                    self.pending = Some(PendingKey::FindChar(FindDirection::Backward, false));
                    return VimOutcome::handled(Vec::new());
                }
                't' => {
                    self.pending = Some(PendingKey::FindChar(FindDirection::Forward, true));
                    return VimOutcome::handled(Vec::new());
                }
                'T' => {
                    self.pending = Some(PendingKey::FindChar(FindDirection::Backward, true));
                    return VimOutcome::handled(Vec::new());
                }
                'i' | 'a' => {
                    self.pending = Some(PendingKey::TextObject { around: ch == 'a' });
                    return VimOutcome::handled(Vec::new());
                }
                // Visual motions
                'h' => {
                    let target = move_left(text, cursor, self.take_count());
                    return self.update_visual_motion(text, target, false);
                }
                'l' => {
                    let target = move_right(text, cursor, self.take_count(), false);
                    return self.update_visual_motion(text, target, false);
                }
                'j' => {
                    let c = self.take_count() as isize;
                    let target = move_vertical(text, cursor, c, self.desired_col, true);
                    return self.update_visual_motion(text, target, true);
                }
                'k' => {
                    let c = self.take_count() as isize;
                    let target = move_vertical(text, cursor, -c, self.desired_col, true);
                    return self.update_visual_motion(text, target, true);
                }
                '0' => {
                    self.count = None;
                    let (ls, _) = line_bounds(text, cursor);
                    return self.update_visual_motion(text, ls, false);
                }
                '^' => {
                    self.count = None;
                    return self.update_visual_motion(text, line_first_non_ws(text, cursor), false);
                }
                '$' => {
                    let count = self.take_count();
                    let target_line = line_index_at(text, cursor) + count.saturating_sub(1);
                    let (_, le) = line_bounds(text, line_start_at_index(text, target_line));
                    return self.update_visual_motion(text, clamp_normal_cursor(text, le), false);
                }
                'w' | 'W' => {
                    let target = move_word_forward(
                        text,
                        cursor,
                        self.take_count(),
                        ch == 'W',
                        WordMotionPurpose::Move,
                    );
                    return self.update_visual_motion(text, target, false);
                }
                'b' | 'B' => {
                    let target = move_word_backward(text, cursor, self.take_count(), ch == 'B');
                    return self.update_visual_motion(text, target, false);
                }
                'e' | 'E' => {
                    let target = move_word_end(text, cursor, self.take_count(), ch == 'E');
                    return self.update_visual_motion(text, target, false);
                }
                'G' => {
                    let target_idx = match self.count.take() {
                        Some(n) => n.saturating_sub(1),
                        None => total_lines(text).saturating_sub(1),
                    };
                    let target = line_first_non_ws(text, line_start_at_index(text, target_idx));
                    return self.update_visual_motion(text, target, true);
                }
                '%' => {
                    self.count = None;
                    if let Some(target) = find_matching_bracket(text, cursor) {
                        return self.update_visual_motion(text, target, false);
                    }
                    return VimOutcome::handled(Vec::new());
                }
                '{' | '}' => {
                    let target = move_paragraph(text, cursor, self.take_count(), ch == '}');
                    return self.update_visual_motion(text, target, true);
                }
                ';' | ',' => {
                    let count = self.take_count();
                    if let Some(find) = self.last_find {
                        if let Some(target) =
                            repeat_char_find(text, cursor, find, ch == ',', count)
                        {
                            return self.update_visual_motion(text, target, false);
                        }
                    }
                    return VimOutcome::handled(Vec::new());
                }
                'n' | 'N' if !self.search_query.is_empty() => {
                    let forward = if ch == 'N' {
                        !self.search_forward
                    } else {
                        self.search_forward
                    };
                    if let Some(hit) = find_next_match(text, cursor, &self.search_query, forward, true)
                    {
                        return self.update_visual_motion(text, hit.start, false);
                    }
                    return VimOutcome::handled(Vec::new());
                }

                // Visual operators
                'd' | 'x' => {
                    if let Some((range, lw)) = self.visual_range(text, cursor) {
                        self.clear_visual();
                        self.mode = VimMode::Normal;
                        return self.apply_range_edit(text, range, Operator::Delete, lw);
                    }
                }
                'c' | 's' => {
                    if let Some((range, lw)) = self.visual_range(text, cursor) {
                        self.clear_visual();
                        return self.apply_range_edit(text, range, Operator::Change, lw);
                    }
                }
                'y' => {
                    if let Some((range, lw)) = self.visual_range(text, cursor) {
                        self.clear_visual();
                        self.mode = VimMode::Normal;
                        return self.apply_range_edit(text, range, Operator::Yank, lw);
                    }
                }
                '>' | '<' => {
                    if let Some((range, _)) = self.visual_range(text, cursor) {
                        self.clear_visual();
                        self.mode = VimMode::Normal;
                        let op = if ch == '>' {
                            Operator::Indent
                        } else {
                            Operator::Outdent
                        };
                        return self.apply_range_edit(text, range, op, true);
                    }
                }
                'u' | 'U' | '~' => {
                    if let Some((range, lw)) = self.visual_range(text, cursor) {
                        self.clear_visual();
                        self.mode = VimMode::Normal;
                        let op = match ch {
                            'u' => Operator::Lowercase,
                            'U' => Operator::Uppercase,
                            _ => Operator::ToggleCase,
                        };
                        return self.apply_range_edit(text, range, op, lw);
                    }
                }
                'J' => {
                    if let Some((range, _)) = self.visual_range(text, cursor) {
                        self.clear_visual();
                        self.mode = VimMode::Normal;
                        let start_line = line_index_at(text, range.start);
                        let end_pos = if range.end > range.start {
                            prev_char_start(text, range.end).max(range.start)
                        } else {
                            range.start
                        };
                        let end_line = line_index_at(text, end_pos);
                        let lines_to_join = end_line.saturating_sub(start_line).max(1);
                        return self.join_lines(text, range.start, lines_to_join);
                    }
                }
                'p' | 'P' => {
                    if let Some((range, _)) = self.visual_range(text, cursor) {
                        let replaced = self.register.text.clone();
                        let old = text[range.clone()].to_string();
                        self.register = VimRegister {
                            text: old,
                            linewise: false,
                        };
                        self.clear_visual();
                        self.mode = VimMode::Normal;
                        let new_cursor = range.start;
                        return VimOutcome::handled(vec![
                            VimEffect::ModeChanged(VimMode::Normal),
                            VimEffect::Replace {
                                range,
                                replacement: replaced,
                                new_cursor,
                            },
                        ]);
                    }
                }
                _ => {}
            }
        } else {
            match key.key.as_str() {
                "left" => {
                    let target = move_left(text, cursor, self.take_count());
                    return self.update_visual_motion(text, target, false);
                }
                "right" => {
                    let target = move_right(text, cursor, self.take_count(), false);
                    return self.update_visual_motion(text, target, false);
                }
                "up" => {
                    let c = self.take_count() as isize;
                    return self.update_visual_motion(
                        text,
                        move_vertical(text, cursor, -c, self.desired_col, true),
                        true,
                    );
                }
                "down" => {
                    let c = self.take_count() as isize;
                    return self.update_visual_motion(
                        text,
                        move_vertical(text, cursor, c, self.desired_col, true),
                        true,
                    );
                }
                _ => {}
            }
        }

        VimOutcome::handled(Vec::new())
    }

    fn complete_pending_visual(
        &mut self,
        text: &str,
        cursor: usize,
        pending: PendingKey,
        key: VimKeyEvent,
    ) -> VimOutcome {
        let Some(ch) = key.text_char() else {
            return VimOutcome::handled(Vec::new());
        };
        match pending {
            PendingKey::G if ch == 'g' => {
                let target_idx = self.count.take().map(|n| n.saturating_sub(1)).unwrap_or(0);
                let target = line_first_non_ws(text, line_start_at_index(text, target_idx));
                self.update_visual_motion(text, target, true)
            }
            PendingKey::G if ch == '_' => {
                let (ls, le) = line_bounds(text, cursor);
                let mut target = le;
                for (rel, c) in text[ls..le].char_indices().rev() {
                    if !c.is_whitespace() {
                        target = ls + rel;
                        break;
                    }
                }
                self.update_visual_motion(text, clamp_normal_cursor(text, target), false)
            }
            PendingKey::G if ch == '?' => VimOutcome::handled(vec![VimEffect::AskAi {
                explain: true,
                question: None,
            }]),
            PendingKey::FindChar(dir, till) => {
                let count = self.take_count();
                let find = CharFind {
                    ch,
                    direction: dir,
                    till,
                };
                self.last_find = Some(find);
                if let Some(target) = repeat_char_find(text, cursor, find, false, count) {
                    self.update_visual_motion(text, target, false)
                } else {
                    VimOutcome::handled(Vec::new())
                }
            }
            PendingKey::TextObject { around } => {
                self.count = None;
                if let Some(range) = text_object_range(text, cursor, around, ch) {
                    self.mode = VimMode::Visual;
                    let head = if range.end > range.start {
                        prev_char_start(text, range.end)
                    } else {
                        range.start
                    };
                    self.visual_anchor = Some(range.start);
                    self.desired_col = col_at_byte(text, head);
                    return self.update_visual_selection(text, head);
                }
                VimOutcome::handled(Vec::new())
            }
            _ => VimOutcome::handled(Vec::new()),
        }
    }

    fn update_visual_motion(
        &mut self,
        text: &str,
        head: usize,
        preserve_col: bool,
    ) -> VimOutcome {
        let head = clamp_normal_cursor(text, head);
        if !preserve_col {
            self.desired_col = col_at_byte(text, head);
        }
        self.update_visual_selection(text, head)
    }

    fn update_visual_selection(&mut self, text: &str, head: usize) -> VimOutcome {
        let head = clamp_normal_cursor(text, head);
        self.visual_head = Some(head);
        let anchor = clamp_normal_cursor(text, self.visual_anchor.unwrap_or(head));
        VimOutcome::handled(vec![
            VimEffect::ModeChanged(self.mode),
            VimEffect::SetSelection {
                anchor,
                head,
                linewise: self.mode == VimMode::VisualLine,
            },
        ])
    }

    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1).max(1)
    }

    fn commit_non_insert_change(&mut self) {
        if !self.replaying_dot && !self.recording_change.is_empty() {
            self.last_change = Some(std::mem::take(&mut self.recording_change));
        }
    }

    fn finish_jump_normal(
        &mut self,
        text: &str,
        cursor: usize,
        target: usize,
        inclusive: bool,
        preserve_col: bool,
    ) -> VimOutcome {
        let had_op = self.operator.is_some();
        let mut outcome = self.finish_motion_normal(text, cursor, target, inclusive, preserve_col);
        if !had_op {
            outcome.effects.insert(0, VimEffect::PushJumpPoint);
        }
        outcome
    }

    fn finish_motion_normal(
        &mut self,
        text: &str,
        cursor: usize,
        target: usize,
        inclusive: bool,
        preserve_col: bool,
    ) -> VimOutcome {
        if let Some(op) = self.operator.take() {
            let linewise = preserve_col && matches!(op, Operator::Delete | Operator::Change | Operator::Yank);
            let range = if linewise {
                let (lo, hi) = if cursor <= target {
                    (cursor, target)
                } else {
                    (target, cursor)
                };
                let (s, _) = line_bounds(text, lo);
                let e = line_end_with_newline(text, hi);
                s..e
            } else {
                let (lo, mut hi) = if cursor <= target {
                    (cursor, target)
                } else {
                    (target, cursor)
                };
                if inclusive && hi < text.len() {
                    hi = next_char_end(text, hi);
                }
                lo..hi.max(lo)
            };
            return self.apply_range_edit(text, range, op, linewise);
        }

        self.recording_change.clear();
        let target = clamp_normal_cursor(text, target);
        if !preserve_col {
            self.desired_col = col_at_byte(text, target);
        }
        VimOutcome::handled(vec![VimEffect::MoveCursor { pos: target }])
    }

    fn apply_linewise_operator(
        &mut self,
        text: &str,
        cursor: usize,
        op: Operator,
        count: usize,
    ) -> VimOutcome {
        let start_line = line_index_at(text, cursor);
        let end_line = (start_line + count.saturating_sub(1)).min(total_lines(text).saturating_sub(1));
        let start = line_start_at_index(text, start_line);
        let end_line_start = line_start_at_index(text, end_line);
        let end = line_end_with_newline(text, end_line_start);
        self.apply_range_edit(text, start..end, op, true)
    }

    fn apply_range_edit(
        &mut self,
        text: &str,
        range: Range<usize>,
        op: Operator,
        linewise: bool,
    ) -> VimOutcome {
        let range = range.start.min(text.len())..range.end.min(text.len());
        match op {
            Operator::Yank => {
                self.recording_change.clear();
                let mut content = text[range.clone()].to_string();
                if linewise && !content.ends_with('\n') {
                    content.push('\n');
                }
                let reg = VimRegister {
                    text: content,
                    linewise,
                };
                self.register = reg.clone();
                let target = clamp_normal_cursor(text, range.start);
                self.desired_col = col_at_byte(text, target);
                let line_msg = if linewise {
                    let count = reg.text.lines().count().max(1);
                    format!("{count} line(s) yanked")
                } else {
                    "Yanked".to_string()
                };
                self.status_message = Some(line_msg.clone());
                VimOutcome::handled(vec![
                    VimEffect::ModeChanged(VimMode::Normal),
                    VimEffect::WriteClipboard(reg),
                    VimEffect::MoveCursor { pos: target },
                    VimEffect::StatusMessage(line_msg),
                ])
            }
            Operator::Delete => {
                let mut content = text[range.clone()].to_string();
                let mut del_range = range.clone();
                let mut new_cursor = range.start;

                if linewise {
                    if !content.ends_with('\n') {
                        content.push('\n');
                    }
                    // If deleting the last line(s) without trailing newline, remove preceding `\n` too.
                    if del_range.end == text.len()
                        && !text[del_range.start..del_range.end].ends_with('\n')
                        && del_range.start > 0
                        && text.as_bytes()[del_range.start - 1] == b'\n'
                    {
                        del_range.start -= 1;
                    }
                }

                let reg = VimRegister {
                    text: content,
                    linewise,
                };
                self.register = reg.clone();

                let mut next_buf = String::with_capacity(text.len());
                next_buf.push_str(&text[..del_range.start]);
                next_buf.push_str(&text[del_range.end..]);
                if linewise {
                    let target = line_start_at_index(&next_buf, line_index_at(&next_buf, del_range.start.min(next_buf.len())));
                    new_cursor = line_first_non_ws(&next_buf, target);
                } else {
                    new_cursor = clamp_normal_cursor(&next_buf, new_cursor.min(next_buf.len()));
                }

                self.desired_col = col_at_byte(&next_buf, new_cursor);
                self.commit_non_insert_change();
                VimOutcome::handled(vec![
                    VimEffect::ModeChanged(VimMode::Normal),
                    VimEffect::WriteClipboard(reg),
                    VimEffect::Replace {
                        range: del_range,
                        replacement: String::new(),
                        new_cursor,
                    },
                ])
            }
            Operator::Change => {
                let mut content = text[range.clone()].to_string();
                let mut del_range = range.clone();
                let mut replacement = String::new();
                let mut new_cursor = range.start;

                if linewise {
                    if !content.ends_with('\n') {
                        content.push('\n');
                    }
                    // Keep the newline so `cc` leaves an empty/indented line ready for typing.
                    let (ls, le) = line_bounds(text, range.start);
                    let indent: String = text[ls..le]
                        .chars()
                        .take_while(|c| *c == ' ' || *c == '\t')
                        .collect();
                    let has_nl = text[range.clone()].ends_with('\n');
                    del_range = range.start..if has_nl { range.end - 1 } else { range.end };
                    replacement = indent.clone();
                    new_cursor = del_range.start + indent.len();
                }

                let reg = VimRegister {
                    text: content,
                    linewise,
                };
                self.register = reg.clone();
                self.mode = VimMode::Insert;
                VimOutcome::handled(vec![
                    VimEffect::WriteClipboard(reg),
                    VimEffect::Replace {
                        range: del_range,
                        replacement,
                        new_cursor,
                    },
                    VimEffect::ModeChanged(VimMode::Insert),
                ])
            }
            Operator::Indent | Operator::Outdent => {
                self.indent_range(text, range, op == Operator::Indent)
            }
            Operator::Lowercase | Operator::Uppercase | Operator::ToggleCase => {
                let transformed: String = text[range.clone()]
                    .chars()
                    .map(|c| match op {
                        Operator::Lowercase => c.to_lowercase().collect::<String>(),
                        Operator::Uppercase => c.to_uppercase().collect::<String>(),
                        Operator::ToggleCase => {
                            if c.is_lowercase() {
                                c.to_uppercase().collect::<String>()
                            } else if c.is_uppercase() {
                                c.to_lowercase().collect::<String>()
                            } else {
                                c.to_string()
                            }
                        }
                        _ => unreachable!(),
                    })
                    .collect();
                let new_cursor = clamp_normal_cursor(text, range.start);
                self.desired_col = col_at_byte(text, new_cursor);
                self.commit_non_insert_change();
                VimOutcome::handled(vec![
                    VimEffect::ModeChanged(VimMode::Normal),
                    VimEffect::Replace {
                        range,
                        replacement: transformed,
                        new_cursor,
                    },
                ])
            }
        }
    }

    fn indent_range(&mut self, text: &str, range: Range<usize>, indent: bool) -> VimOutcome {
        let lines = all_line_bounds(text);
        let start_line = line_index_at(text, range.start);
        let end_probe = if range.end > range.start {
            prev_char_start(text, range.end).max(range.start)
        } else {
            range.start
        };
        let end_line = line_index_at(text, end_probe);
        let block_start = lines[start_line].0;
        let block_end = lines[end_line].1;

        let mut rebuilt = String::new();
        for idx in start_line..=end_line {
            if idx > start_line {
                rebuilt.push('\n');
            }
            let (ls, le) = lines[idx];
            let line = &text[ls..le];
            if indent {
                if !line.is_empty() {
                    rebuilt.push_str("    ");
                    rebuilt.push_str(line);
                }
            } else if let Some(rest) = line.strip_prefix("    ") {
                rebuilt.push_str(rest);
            } else if let Some(rest) = line.strip_prefix('\t') {
                rebuilt.push_str(rest);
            } else {
                let stripped = line
                    .strip_prefix("   ")
                    .or_else(|| line.strip_prefix("  "))
                    .or_else(|| line.strip_prefix(' '))
                    .unwrap_or(line);
                rebuilt.push_str(stripped);
            }
        }

        let new_cursor = block_start + first_non_ws_offset(&rebuilt);
        self.commit_non_insert_change();
        VimOutcome::handled(vec![
            VimEffect::ModeChanged(VimMode::Normal),
            VimEffect::Replace {
                range: block_start..block_end,
                replacement: rebuilt,
                new_cursor,
            },
        ])
    }

    fn join_lines(&mut self, text: &str, cursor: usize, count: usize) -> VimOutcome {
        let lines = all_line_bounds(text);
        let start_line = line_index_at(text, cursor);
        if start_line + 1 >= lines.len() {
            return VimOutcome::handled(Vec::new());
        }
        let joins = count.max(1);
        let end_line = (start_line + joins).min(lines.len() - 1);
        let block_start = lines[start_line].0;
        let block_end = lines[end_line].1;

        let mut joined = text[lines[start_line].0..lines[start_line].1]
            .trim_end_matches([' ', '\t'])
            .to_string();
        let mut new_cursor = block_start + joined.len();
        for idx in (start_line + 1)..=end_line {
            let trimmed = text[lines[idx].0..lines[idx].1].trim_start_matches([' ', '\t']);
            if !trimmed.is_empty() {
                if !joined.is_empty() && !joined.ends_with(' ') {
                    new_cursor = block_start + joined.len();
                    joined.push(' ');
                }
                joined.push_str(trimmed);
            }
        }

        let mut next_buf = String::with_capacity(text.len());
        next_buf.push_str(&text[..block_start]);
        next_buf.push_str(&joined);
        next_buf.push_str(&text[block_end..]);
        let clamped_cursor = clamp_normal_cursor(&next_buf, new_cursor);
        self.desired_col = col_at_byte(&next_buf, clamped_cursor);

        VimOutcome::handled(vec![
            VimEffect::ModeChanged(VimMode::Normal),
            VimEffect::Replace {
                range: block_start..block_end,
                replacement: joined,
                new_cursor: clamped_cursor,
            },
        ])
    }

    /// Begin or end `.` replay guard.
    pub fn set_replaying_dot(&mut self, replaying: bool) {
        self.replaying_dot = replaying;
    }
}

fn first_non_ws_offset(s: &str) -> usize {
    let first_line_end = s.find('\n').unwrap_or(s.len());
    for (idx, ch) in s[..first_line_end].char_indices() {
        if !ch.is_whitespace() {
            return idx;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply_step(vim: &mut VimState, buf: &mut String, cur: &mut usize, key: VimKeyEvent) {
        let out = vim.handle_key(buf, *cur, key);
        for eff in out.effects {
            match eff {
                VimEffect::MoveCursor { pos } => *cur = pos,
                VimEffect::SetSelection { head, .. } => *cur = head,
                VimEffect::Replace {
                    range,
                    replacement,
                    new_cursor,
                } => {
                    buf.replace_range(range, &replacement);
                    *cur = new_cursor;
                }
                VimEffect::RequestPaste { before, count } => {
                    let reg = vim.register.clone();
                    if let Some(paste_out) = vim.apply_paste(buf, *cur, &reg, before, count) {
                        for p in paste_out.effects {
                            if let VimEffect::Replace {
                                range,
                                replacement,
                                new_cursor,
                            } = p
                            {
                                buf.replace_range(range, &replacement);
                                *cur = new_cursor;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn normal_motions_operators_and_linewise_paste() {
        let mut vim = VimState::new(false);
        let mut buf = "first line\nsecond line\nthird line".to_string();
        let mut cur = 0usize;

        // `dd` deletes first line and stores linewise register
        apply_step(&mut vim, &mut buf, &mut cur, VimKeyEvent::char_key("d"));
        apply_step(&mut vim, &mut buf, &mut cur, VimKeyEvent::char_key("d"));
        assert_eq!(buf, "second line\nthird line");
        assert!(vim.register.linewise);

        // `j` moves to third line, `p` pastes below
        apply_step(&mut vim, &mut buf, &mut cur, VimKeyEvent::char_key("j"));
        apply_step(&mut vim, &mut buf, &mut cur, VimKeyEvent::char_key("p"));
        assert_eq!(buf, "second line\nthird line\nfirst line");
    }

    #[test]
    fn text_objects_change_indent_and_visual_mode() {
        let mut vim = VimState::new(false);
        let mut buf = "fn greet(name: &str) {\n    println!(\"hi\");\n}".to_string();
        let mut cur = buf.find("name").unwrap();

        // `ci(` changes inside parens and enters Insert mode
        for k in ["c", "i", "("] {
            apply_step(&mut vim, &mut buf, &mut cur, VimKeyEvent::char_key(k));
        }
        assert_eq!(vim.mode, VimMode::Insert);
        assert_eq!(buf, "fn greet() {\n    println!(\"hi\");\n}");

        // `Esc` leaves Insert mode, `V` + `>` indents line 0..1
        apply_step(&mut vim, &mut buf, &mut cur, VimKeyEvent::named("escape"));
        assert_eq!(vim.mode, VimMode::Normal);

        let mut vim2 = VimState::new(false);
        let mut buf2 = "alpha\nbeta\ngamma".to_string();
        let mut cur2 = 0usize;
        apply_step(&mut vim2, &mut buf2, &mut cur2, VimKeyEvent::char_key("V"));
        apply_step(&mut vim2, &mut buf2, &mut cur2, VimKeyEvent::char_key("j"));
        apply_step(&mut vim2, &mut buf2, &mut cur2, VimKeyEvent::char_key(">"));
        assert_eq!(buf2, "    alpha\n    beta\ngamma");
    }

    #[test]
    fn replace_mode_search_ex_commands_and_code_nav() {
        let mut vim = VimState::new(false);
        let mut buf = "let alpha = 1;\nlet beta = alpha;".to_string();
        let mut cur = 4usize;

        // `R` overwrites characters one by one
        apply_step(&mut vim, &mut buf, &mut cur, VimKeyEvent::char_key("R"));
        assert_eq!(vim.mode, VimMode::Replace);
        for k in ["A", "B"] {
            apply_step(&mut vim, &mut buf, &mut cur, VimKeyEvent::char_key(k));
        }
        apply_step(&mut vim, &mut buf, &mut cur, VimKeyEvent::named("escape"));
        assert_eq!(buf, "let ABpha = 1;\nlet beta = alpha;");
        assert_eq!(vim.mode, VimMode::Normal);

        // `gd` emits GoToDefinition, `]d` emits NextDiagnostic, `:w` emits SaveFile
        let out_gd = {
            let _ = vim.handle_key(&buf, cur, VimKeyEvent::char_key("g"));
            vim.handle_key(&buf, cur, VimKeyEvent::char_key("d"))
        };
        assert!(out_gd.effects.contains(&VimEffect::GoToDefinition));

        let out_diag = {
            let _ = vim.handle_key(&buf, cur, VimKeyEvent::char_key("]"));
            vim.handle_key(&buf, cur, VimKeyEvent::char_key("d"))
        };
        assert!(out_diag
            .effects
            .contains(&VimEffect::NextDiagnostic { errors_only: false }));

        // `Ctrl+o` / `Ctrl+i` emit JumpBack / JumpForward
        let out_back = vim.handle_key(&buf, cur, VimKeyEvent::ctrl("o"));
        assert!(out_back.effects.contains(&VimEffect::JumpBack));
        let out_fwd = vim.handle_key(&buf, cur, VimKeyEvent::ctrl("i"));
        assert!(out_fwd.effects.contains(&VimEffect::JumpForward));

        // Complete Vim code-nav & tab-nav chords (`gD`, `gy`, `gI`, `gf`, `gi`, `ga`, `gO`, `2gt`, `gT`, `]s`, `[e`)
        let check_chord = |v: &mut VimState, a: &str, b: &str| {
            let _ = v.handle_key(&buf, cur, VimKeyEvent::char_key(a));
            v.handle_key(&buf, cur, VimKeyEvent::char_key(b)).effects
        };
        assert!(check_chord(&mut vim, "g", "D").contains(&VimEffect::GoToDeclaration));
        assert!(check_chord(&mut vim, "g", "y").contains(&VimEffect::GoToTypeDefinition));
        assert!(check_chord(&mut vim, "g", "I").contains(&VimEffect::GoToImplementation));
        assert!(check_chord(&mut vim, "g", "f").contains(&VimEffect::GoToFile));
        assert!(check_chord(&mut vim, "g", "a").contains(&VimEffect::CodeActions));
        assert!(check_chord(&mut vim, "g", "O").contains(&VimEffect::OpenOutline));
        assert!(check_chord(&mut vim, "g", "T").contains(&VimEffect::PrevTab));
        assert!(check_chord(&mut vim, "]", "s").contains(&VimEffect::NextSymbol));
        assert!(
            check_chord(&mut vim, "[", "e")
                .contains(&VimEffect::PrevDiagnostic { errors_only: true })
        );
        let _ = vim.handle_key(&buf, cur, VimKeyEvent::char_key("2"));
        assert!(check_chord(&mut vim, "g", "t").contains(&VimEffect::GoToTab(2)));

        // Ask AI chords & Ex commands (`g?`, Visual `g?`, `:explain`, `:ask <question>`)
        assert!(check_chord(&mut vim, "g", "?").contains(&VimEffect::AskAi {
            explain: true,
            question: None,
        }));
        let _ = vim.handle_key(&buf, cur, VimKeyEvent::char_key(":"));
        for k in ["a", "s", "k", " ", "w", "h", "y"] {
            let _ = vim.handle_key(&buf, cur, VimKeyEvent::char_key(k));
        }
        let out_ask = vim.handle_key(&buf, cur, VimKeyEvent::named("enter"));
        assert!(out_ask.effects.contains(&VimEffect::AskAi {
            explain: false,
            question: Some("why".to_string()),
        }));

        let _ = vim.handle_key(&buf, cur, VimKeyEvent::char_key("v"));
        assert!(check_chord(&mut vim, "g", "?").contains(&VimEffect::AskAi {
            explain: true,
            question: None,
        }));
        let _ = vim.handle_key(&buf, cur, VimKeyEvent::named("escape"));

        let _ = vim.handle_key(&buf, cur, VimKeyEvent::char_key(":"));
        let _ = vim.handle_key(&buf, cur, VimKeyEvent::char_key("w"));
        let out_ex = vim.handle_key(&buf, cur, VimKeyEvent::named("enter"));
        assert!(out_ex.effects.contains(&VimEffect::SaveFile));

        // `/` search jumps to matching occurrence
        cur = 0;
        let _ = vim.handle_key(&buf, cur, VimKeyEvent::char_key("/"));
        for k in ["b", "e", "t", "a"] {
            let _ = vim.handle_key(&buf, cur, VimKeyEvent::char_key(k));
        }
        let expected_beta = buf.find("beta").unwrap();
        let out_search = vim.handle_key(&buf, cur, VimKeyEvent::named("enter"));
        assert!(out_search
            .effects
            .contains(&VimEffect::MoveCursor { pos: expected_beta }));

        // Viewport scroll alignment (`zz`/`zt`/`zb`) and display Ex `:set` commands
        let check_z = |v: &mut VimState, a: &str, b: &str| {
            let _ = v.handle_key(&buf, cur, VimKeyEvent::char_key(a));
            v.handle_key(&buf, cur, VimKeyEvent::char_key(b)).effects
        };
        assert!(check_z(&mut vim, "z", "z").contains(&VimEffect::AlignCursor(ScrollAlign::Center)));
        assert!(check_z(&mut vim, "z", "t").contains(&VimEffect::AlignCursor(ScrollAlign::Top)));
        assert!(check_z(&mut vim, "z", "b").contains(&VimEffect::AlignCursor(ScrollAlign::Bottom)));

        let run_ex = |v: &mut VimState, cmd: &str| {
            let _ = v.handle_key(&buf, cur, VimKeyEvent::char_key(":"));
            for ch in cmd.chars() {
                let s = ch.to_string();
                let _ = v.handle_key(&buf, cur, VimKeyEvent::char_key(&s));
            }
            v.handle_key(&buf, cur, VimKeyEvent::named("enter")).effects
        };
        assert!(run_ex(&mut vim, "set scrolloff=8").contains(&VimEffect::SetScrollPadding(8)));
        assert!(run_ex(&mut vim, "set noinlay").contains(&VimEffect::SetInlayHints(false)));
        assert!(run_ex(&mut vim, "set inlay").contains(&VimEffect::SetInlayHints(true)));
        assert!(run_ex(&mut vim, "set noinlinedoc").contains(&VimEffect::SetInlineHoverDocs(false)));
    }
}
