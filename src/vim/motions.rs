use std::ops::Range;

/// Purpose for word motion so `w`/`W` behave authentically for movement vs `dw` vs `cw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordMotionPurpose {
    Move,
    Delete,
    Change,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharFind {
    pub ch: char,
    pub direction: FindDirection,
    pub till: bool,
}

/// Clamp `idx` down to a valid UTF-8 character boundary within `0..=text.len()`.
pub fn clamp_char_boundary(text: &str, mut idx: usize) -> usize {
    idx = idx.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Return the byte end of the character starting at `idx`, or `text.len()` if at end.
pub fn next_char_end(text: &str, idx: usize) -> usize {
    let idx = clamp_char_boundary(text, idx);
    if idx >= text.len() {
        return text.len();
    }
    match text[idx..].chars().next() {
        Some(ch) => idx + ch.len_utf8(),
        None => text.len(),
    }
}

/// Return the byte start of the character immediately preceding `idx`, or `0` if at start.
pub fn prev_char_start(text: &str, idx: usize) -> usize {
    let idx = clamp_char_boundary(text, idx);
    if idx == 0 {
        return 0;
    }
    match text[..idx].chars().next_back() {
        Some(ch) => idx - ch.len_utf8(),
        None => 0,
    }
}

/// Return `(line_start, line_end_excluding_crlf)` for the line containing `pos`.
pub fn line_bounds(text: &str, pos: usize) -> (usize, usize) {
    let pos = clamp_char_boundary(text, pos);
    let start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let mut end = text[pos..]
        .find('\n')
        .map(|rel| pos + rel)
        .unwrap_or(text.len());
    if end > start && text.as_bytes()[end - 1] == b'\r' {
        end -= 1;
    }
    (start, end)
}

/// Return byte offset immediately after the newline terminating `line_end`, or `text.len()`.
pub fn line_end_with_newline(text: &str, pos: usize) -> usize {
    let pos = clamp_char_boundary(text, pos);
    match text[pos..].find('\n') {
        Some(rel) => pos + rel + 1,
        None => text.len(),
    }
}

/// Clamp `pos` so that in Normal mode it never sits past the last character of a non-empty line.
pub fn clamp_normal_cursor(text: &str, pos: usize) -> usize {
    let pos = clamp_char_boundary(text, pos);
    let (start, end) = line_bounds(text, pos);
    if start == end {
        return start;
    }
    let last_char = prev_char_start(text, end);
    pos.clamp(start, last_char)
}

/// Return byte offset of first non-whitespace character on the line containing `pos`,
/// or `line_start` if the line is empty/all whitespace (wait: for `^`, if all spaces, last char on line).
pub fn line_first_non_ws(text: &str, pos: usize) -> usize {
    let (start, end) = line_bounds(text, pos);
    for (rel, ch) in text[start..end].char_indices() {
        if !ch.is_whitespace() {
            return start + rel;
        }
    }
    if start == end {
        start
    } else {
        prev_char_start(text, end)
    }
}

/// Return 0-based character column within current line for `pos`.
pub fn col_at_byte(text: &str, pos: usize) -> usize {
    let pos = clamp_char_boundary(text, pos);
    let (start, _) = line_bounds(text, pos);
    text[start..pos].chars().count()
}

/// Return byte offset on line `(start..end)` corresponding to `col` (clamped if Normal mode).
pub fn byte_at_line_col(text: &str, line_start: usize, col: usize, normal_clamp: bool) -> usize {
    let (start, end) = line_bounds(text, line_start);
    let mut offset = start;
    let mut seen = 0;
    for ch in text[start..end].chars() {
        if seen == col {
            break;
        }
        offset += ch.len_utf8();
        seen += 1;
    }
    if normal_clamp {
        clamp_normal_cursor(text, offset)
    } else {
        offset.min(end)
    }
}

/// Return all `(line_start, line_end)` byte bounds in `text`. Always yields at least 1 line.
pub fn all_line_bounds(text: &str) -> Vec<(usize, usize)> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (idx, b) in text.bytes().enumerate() {
        if b == b'\n' {
            let mut end = idx;
            if end > start && text.as_bytes()[end - 1] == b'\r' {
                end -= 1;
            }
            lines.push((start, end));
            start = idx + 1;
        }
    }
    let mut end = text.len();
    if end > start && text.as_bytes()[end - 1] == b'\r' {
        end -= 1;
    }
    lines.push((start, end));
    lines
}

/// 0-based line index for `pos`.
pub fn line_index_at(text: &str, pos: usize) -> usize {
    let pos = clamp_char_boundary(text, pos);
    text[..pos].bytes().filter(|&b| b == b'\n').count()
}

/// Total number of lines in `text` (at least 1).
pub fn total_lines(text: &str) -> usize {
    text.bytes().filter(|&b| b == b'\n').count() + 1
}

/// Return byte offset of start of 0-based line `line_idx` (clamped to last line).
pub fn line_start_at_index(text: &str, line_idx: usize) -> usize {
    let lines = all_line_bounds(text);
    let clamped = line_idx.min(lines.len().saturating_sub(1));
    lines[clamped].0
}

pub fn move_left(text: &str, pos: usize, count: usize) -> usize {
    let (start, _) = line_bounds(text, pos);
    let mut cur = clamp_normal_cursor(text, pos);
    for _ in 0..count {
        if cur <= start {
            break;
        }
        cur = prev_char_start(text, cur);
    }
    cur
}

pub fn move_right(text: &str, pos: usize, count: usize, allow_line_end: bool) -> usize {
    let (start, end) = line_bounds(text, pos);
    if start == end {
        return start;
    }
    let max_pos = if allow_line_end {
        end
    } else {
        prev_char_start(text, end)
    };
    let mut cur = clamp_char_boundary(text, pos).clamp(start, max_pos);
    for _ in 0..count {
        if cur >= max_pos {
            break;
        }
        cur = next_char_end(text, cur).min(max_pos);
    }
    cur
}

pub fn move_vertical(
    text: &str,
    pos: usize,
    delta: isize,
    desired_col: usize,
    normal_clamp: bool,
) -> usize {
    let lines = all_line_bounds(text);
    if lines.is_empty() {
        return 0;
    }
    let cur_line = line_index_at(text, pos);
    let target_line = if delta < 0 {
        cur_line.saturating_sub(delta.unsigned_abs())
    } else {
        (cur_line + delta as usize).min(lines.len() - 1)
    };
    byte_at_line_col(text, lines[target_line].0, desired_col, normal_clamp)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Whitespace,
    Newline,
    Word,
    Punct,
}

fn char_class(ch: char, big_word: bool) -> CharClass {
    if ch == '\n' {
        CharClass::Newline
    } else if ch.is_whitespace() {
        CharClass::Whitespace
    } else if big_word || ch.is_alphanumeric() || ch == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

/// Move forward by `count` words (`w`/`W`), returning byte position (or exclusive end for `Delete`/`Change`).
pub fn move_word_forward(
    text: &str,
    pos: usize,
    count: usize,
    big_word: bool,
    purpose: WordMotionPurpose,
) -> usize {
    let mut cur = clamp_char_boundary(text, pos);
    if text.is_empty() {
        return 0;
    }

    for step in 0..count {
        if cur >= text.len() {
            return text.len();
        }
        let is_last = step + 1 == count;
        let first_ch = match text[cur..].chars().next() {
            Some(c) => c,
            None => return text.len(),
        };
        let first_class = char_class(first_ch, big_word);
        let (_, cur_line_end) = line_bounds(text, cur);

        if matches!(first_class, CharClass::Word | CharClass::Punct) {
            while cur < text.len() {
                if let Some(c) = text[cur..].chars().next() {
                    if char_class(c, big_word) == first_class {
                        cur += c.len_utf8();
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            if is_last && purpose == WordMotionPurpose::Change {
                return cur;
            }
        } else if first_class == CharClass::Newline {
            // Crossing newline onto next line.
            cur += first_ch.len_utf8();
        }

        // Skip whitespace; stop at empty lines for Move, or at current line end for Delete/Change if requested.
        let mut saw_newline = first_class == CharClass::Newline;
        while cur < text.len() {
            if is_last && purpose != WordMotionPurpose::Move && cur == cur_line_end {
                // `dw` on last word of line stops at end of line instead of pulling next line.
                if cur > pos {
                    return cur;
                }
            }
            let c = match text[cur..].chars().next() {
                Some(c) => c,
                None => break,
            };
            let cls = char_class(c, big_word);
            if cls == CharClass::Newline {
                if saw_newline {
                    // Empty line is its own stop in Move mode.
                    if purpose == WordMotionPurpose::Move {
                        return cur;
                    }
                }
                saw_newline = true;
                cur += c.len_utf8();
                if cur < text.len() && text[cur..].starts_with('\n') && purpose == WordMotionPurpose::Move {
                    return cur;
                }
            } else if cls == CharClass::Whitespace {
                cur += c.len_utf8();
            } else {
                break;
            }
        }
    }

    if purpose == WordMotionPurpose::Move {
        clamp_normal_cursor(text, cur)
    } else {
        cur.min(text.len())
    }
}

/// Move to the end of word (`e`/`E`) `count` times; returns cursor byte position on last char of word.
pub fn move_word_end(text: &str, pos: usize, count: usize, big_word: bool) -> usize {
    let mut cur = clamp_char_boundary(text, pos);
    if text.is_empty() {
        return 0;
    }

    for _ in 0..count {
        let next = next_char_end(text, cur);
        if next >= text.len() {
            return clamp_normal_cursor(text, text.len());
        }
        cur = next;

        // Skip whitespace / newlines to reach the start of the next word.
        while cur < text.len() {
            let c = match text[cur..].chars().next() {
                Some(c) => c,
                None => break,
            };
            if c.is_whitespace() {
                cur += c.len_utf8();
            } else {
                break;
            }
        }
        if cur >= text.len() {
            return clamp_normal_cursor(text, text.len());
        }

        let target_cls = match text[cur..].chars().next() {
            Some(c) => char_class(c, big_word),
            None => break,
        };
        while cur < text.len() {
            let ch = text[cur..].chars().next().unwrap();
            let after = cur + ch.len_utf8();
            if after >= text.len() {
                break;
            }
            let next_ch = text[after..].chars().next().unwrap();
            if char_class(next_ch, big_word) != target_cls {
                break;
            }
            cur = after;
        }
    }

    clamp_normal_cursor(text, cur)
}

/// Move backward by `count` words (`b`/`B`).
pub fn move_word_backward(text: &str, pos: usize, count: usize, big_word: bool) -> usize {
    let mut cur = clamp_char_boundary(text, pos);
    for _ in 0..count {
        if cur == 0 {
            return 0;
        }
        cur = prev_char_start(text, cur);

        // Skip whitespace backward; stop on empty line if encountered after passing a newline.
        let mut crossed_nl = false;
        while cur > 0 {
            let c = text[cur..].chars().next().unwrap();
            if c == '\n' {
                if crossed_nl && text[..cur].ends_with('\n') {
                    return cur;
                }
                crossed_nl = true;
                cur = prev_char_start(text, cur);
            } else if c.is_whitespace() {
                cur = prev_char_start(text, cur);
            } else {
                break;
            }
        }
        if cur == 0 {
            return 0;
        }

        let target_cls = match text[cur..].chars().next() {
            Some(c) if !c.is_whitespace() => char_class(c, big_word),
            _ => continue,
        };
        while cur > 0 {
            let prev = prev_char_start(text, cur);
            let prev_ch = text[prev..].chars().next().unwrap();
            if char_class(prev_ch, big_word) == target_cls {
                cur = prev;
            } else {
                break;
            }
        }
    }
    cur
}

/// Find character `ch` on the current line according to `find` (`f`/`F`/`t`/`T`).
pub fn find_char_on_line(text: &str, pos: usize, find: CharFind) -> Option<usize> {
    let (start, end) = line_bounds(text, pos);
    let pos = clamp_char_boundary(text, pos).clamp(start, end);
    match find.direction {
        FindDirection::Forward => {
            let search_from = next_char_end(text, pos).min(end);
            if search_from >= end {
                return None;
            }
            for (rel, c) in text[search_from..end].char_indices() {
                if c == find.ch {
                    let hit = search_from + rel;
                    if find.till {
                        let before = prev_char_start(text, hit);
                        return if before >= pos { Some(before) } else { None };
                    }
                    return Some(hit);
                }
            }
            None
        }
        FindDirection::Backward => {
            if pos <= start {
                return None;
            }
            for (rel, c) in text[start..pos].char_indices().rev() {
                if c == find.ch {
                    let hit = start + rel;
                    if find.till {
                        let after = next_char_end(text, hit);
                        return if after <= pos { Some(after) } else { None };
                    }
                    return Some(hit);
                }
            }
            None
        }
    }
}

/// Repeat `find_char_on_line` `count` times, optionally reversing direction (for `,`).
pub fn repeat_char_find(
    text: &str,
    pos: usize,
    mut find: CharFind,
    reverse: bool,
    count: usize,
) -> Option<usize> {
    if reverse {
        find.direction = match find.direction {
            FindDirection::Forward => FindDirection::Backward,
            FindDirection::Backward => FindDirection::Forward,
        };
    }
    let mut cur = pos;
    let mut moved = false;
    for _ in 0..count {
        if let Some(next) = find_char_on_line(text, cur, find) {
            cur = next;
            moved = true;
        } else {
            return if moved { Some(cur) } else { None };
        }
    }
    if moved { Some(cur) } else { None }
}

fn matching_bracket(ch: char) -> Option<(char, bool)> {
    match ch {
        '(' => Some((')', true)),
        ')' => Some(('(', false)),
        '[' => Some((']', true)),
        ']' => Some(('[', false)),
        '{' => Some(('}', true)),
        '}' => Some(('{', false)),
        '<' => Some(('>', true)),
        '>' => Some(('<', false)),
        _ => None,
    }
}

/// `%` motion: find matching delimiter starting at cursor or first bracket forward on current line.
pub fn find_matching_bracket(text: &str, pos: usize) -> Option<usize> {
    let (_, end) = line_bounds(text, pos);
    let pos = clamp_char_boundary(text, pos);
    if pos >= end {
        return None;
    }

    let mut origin = None;
    for (rel, ch) in text[pos..end].char_indices() {
        if let Some((partner, forward)) = matching_bracket(ch) {
            origin = Some((pos + rel, ch, partner, forward));
            break;
        }
    }
    let (start_idx, open_ch, close_ch, forward) = origin?;

    let mut depth = 0usize;
    if forward {
        for (rel, ch) in text[start_idx..].char_indices() {
            if ch == open_ch {
                depth += 1;
            } else if ch == close_ch {
                depth -= 1;
                if depth == 0 {
                    return Some(start_idx + rel);
                }
            }
        }
    } else {
        for (idx, ch) in text[..=start_idx].char_indices().rev() {
            if ch == open_ch {
                depth += 1;
            } else if ch == close_ch {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
        }
    }
    None
}

/// Paragraph motion (`}` forward, `{` backward) skipping consecutive blank lines.
pub fn move_paragraph(text: &str, pos: usize, count: usize, forward: bool) -> usize {
    let lines = all_line_bounds(text);
    if lines.is_empty() {
        return 0;
    }
    let mut idx = line_index_at(text, pos);
    let is_blank = |i: usize| {
        let (s, e) = lines[i];
        text[s..e].trim().is_empty()
    };

    for _ in 0..count {
        if forward {
            if idx + 1 >= lines.len() {
                idx = lines.len() - 1;
                break;
            }
            while idx + 1 < lines.len() && is_blank(idx) {
                idx += 1;
            }
            while idx + 1 < lines.len() && !is_blank(idx) {
                idx += 1;
            }
        } else {
            if idx == 0 {
                break;
            }
            while idx > 0 && is_blank(idx) {
                idx -= 1;
            }
            while idx > 0 && !is_blank(idx) {
                if idx == 1 && !is_blank(0) {
                    idx = 0;
                    break;
                }
                if is_blank(idx - 1) {
                    idx -= 1;
                    break;
                }
                idx -= 1;
            }
        }
    }
    lines[idx].0
}

/// Extract word (`is_alphanumeric() || '_'`) byte range and string under `pos`, if any.
pub fn word_under_cursor(text: &str, pos: usize) -> Option<(Range<usize>, &str)> {
    let pos = clamp_char_boundary(text, pos);
    let is_word = |c: char| c.is_alphanumeric() || c == '_';

    let probe = match text[pos..].chars().next() {
        Some(c) if is_word(c) => Some(pos),
        _ if pos > 0 => {
            let prev = prev_char_start(text, pos);
            text[prev..].chars().next().filter(|&c| is_word(c)).map(|_| prev)
        }
        _ => None,
    }?;

    let mut start = probe;
    while start > 0 {
        let prev = prev_char_start(text, start);
        if text[prev..].chars().next().is_some_and(is_word) {
            start = prev;
        } else {
            break;
        }
    }
    let mut end = next_char_end(text, probe);
    while end < text.len() {
        if let Some(c) = text[end..].chars().next() {
            if is_word(c) {
                end += c.len_utf8();
                continue;
            }
        }
        break;
    }
    Some((start..end, &text[start..end]))
}

/// Find all non-overlapping literal matches for `pattern` in `text`.
/// Uses smart-case (case-insensitive unless `pattern` contains an uppercase character).
pub fn find_all_matches(text: &str, pattern: &str) -> Vec<Range<usize>> {
    if pattern.is_empty() {
        return Vec::new();
    }
    let case_sensitive = pattern.chars().any(|c| c.is_uppercase());
    if case_sensitive {
        let mut matches = Vec::new();
        let mut idx = 0;
        while let Some(rel) = text[idx..].find(pattern) {
            let start = idx + rel;
            let end = start + pattern.len();
            matches.push(start..end);
            idx = end.max(idx + 1);
        }
        return matches;
    }

    // ASCII / char-by-char lowercase fallback preserving exact UTF-8 byte offsets.
    let pat_lower: Vec<char> = pattern.chars().flat_map(|c| c.to_lowercase()).collect();
    if pat_lower.is_empty() {
        return Vec::new();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut matches = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let mut matched_end = None;
        let mut pi = 0;
        let mut ci = i;
        while pi < pat_lower.len() && ci < chars.len() {
            let mut ok = true;
            for lc in chars[ci].1.to_lowercase() {
                if pi >= pat_lower.len() || lc != pat_lower[pi] {
                    ok = false;
                    break;
                }
                pi += 1;
            }
            if !ok {
                break;
            }
            ci += 1;
            if pi == pat_lower.len() {
                let end_byte = if ci < chars.len() {
                    chars[ci].0
                } else {
                    text.len()
                };
                matched_end = Some((chars[i].0..end_byte, ci));
                break;
            }
        }
        if let Some((range, next_ci)) = matched_end {
            matches.push(range);
            i = next_ci.max(i + 1);
        } else {
            i += 1;
        }
    }
    matches
}

pub fn find_next_match(
    text: &str,
    from: usize,
    pattern: &str,
    forward: bool,
    wrap: bool,
) -> Option<Range<usize>> {
    let all = find_all_matches(text, pattern);
    if all.is_empty() {
        return None;
    }
    let from = clamp_char_boundary(text, from);
    if forward {
        all.iter()
            .find(|r| r.start > from)
            .cloned()
            .or_else(|| if wrap { all.first().cloned() } else { None })
    } else {
        all.iter()
            .rev()
            .find(|r| r.start < from)
            .cloned()
            .or_else(|| if wrap { all.last().cloned() } else { None })
    }
}

/// Resolve Vim text object (`iw`, `aw`, `i"`, `a"`, `i(`, `a(`, etc.) into a byte range.
pub fn text_object_range(
    text: &str,
    pos: usize,
    around: bool,
    obj: char,
) -> Option<Range<usize>> {
    let pos = clamp_char_boundary(text, pos);
    match obj {
        'w' | 'W' => word_text_object(text, pos, around, obj == 'W'),
        '"' | '\'' | '`' => quote_text_object(text, pos, around, obj),
        '(' | ')' | 'b' => pair_text_object(text, pos, around, '(', ')'),
        '[' | ']' => pair_text_object(text, pos, around, '[', ']'),
        '{' | '}' | 'B' => pair_text_object(text, pos, around, '{', '}'),
        '<' | '>' => pair_text_object(text, pos, around, '<', '>'),
        _ => None,
    }
}

fn word_text_object(text: &str, pos: usize, around: bool, big_word: bool) -> Option<Range<usize>> {
    if text.is_empty() {
        return None;
    }
    let pos = clamp_normal_cursor(text, pos);
    let ch = text[pos..].chars().next()?;
    if ch == '\n' {
        return None;
    }
    let cls = char_class(ch, big_word);
    let mut start = pos;
    while start > 0 {
        let prev = prev_char_start(text, start);
        let prev_ch = text[prev..].chars().next().unwrap();
        if prev_ch != '\n' && char_class(prev_ch, big_word) == cls {
            start = prev;
        } else {
            break;
        }
    }
    let mut end = next_char_end(text, pos);
    while end < text.len() {
        let next_ch = text[end..].chars().next().unwrap();
        if next_ch != '\n' && char_class(next_ch, big_word) == cls {
            end += next_ch.len_utf8();
        } else {
            break;
        }
    }

    if around && cls != CharClass::Whitespace {
        let mut after_ws = end;
        while after_ws < text.len() {
            let c = text[after_ws..].chars().next().unwrap();
            if c.is_whitespace() && c != '\n' {
                after_ws += c.len_utf8();
            } else {
                break;
            }
        }
        if after_ws > end {
            end = after_ws;
        } else {
            let mut before_ws = start;
            while before_ws > 0 {
                let prev = prev_char_start(text, before_ws);
                let c = text[prev..].chars().next().unwrap();
                if c.is_whitespace() && c != '\n' {
                    before_ws = prev;
                } else {
                    break;
                }
            }
            start = before_ws;
        }
    }

    Some(start..end)
}

fn quote_text_object(text: &str, pos: usize, around: bool, quote: char) -> Option<Range<usize>> {
    let (line_start, line_end) = line_bounds(text, pos);
    let pos = pos.clamp(line_start, line_end);
    let mut quotes = Vec::new();
    let mut escaped = false;
    for (rel, ch) in text[line_start..line_end].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != '`' {
            escaped = true;
            continue;
        }
        if ch == quote {
            quotes.push(line_start + rel);
        }
    }
    if quotes.len() < 2 {
        return None;
    }

    // Prefer a pair enclosing `pos`; otherwise first pair starting at/after `pos`.
    for pair in quotes.chunks_exact(2) {
        let (open, close) = (pair[0], pair[1]);
        if (pos >= open && pos <= close) || open >= pos {
            return if around {
                Some(open..close + quote.len_utf8())
            } else {
                Some(open + quote.len_utf8()..close)
            };
        }
    }
    None
}

fn pair_text_object(
    text: &str,
    pos: usize,
    around: bool,
    open_ch: char,
    close_ch: char,
) -> Option<Range<usize>> {
    let pos = clamp_char_boundary(text, pos);

    // Search backward from `pos` for unmatched `open_ch`, unless `pos` sits right on `open_ch`.
    let on_open = text[pos..].chars().next() == Some(open_ch);
    let open_idx = if on_open {
        Some(pos)
    } else {
        let mut balance = 0usize;
        let mut found = None;
        for (idx, ch) in text[..pos].char_indices().rev() {
            if ch == close_ch {
                balance += 1;
            } else if ch == open_ch {
                if balance == 0 {
                    found = Some(idx);
                    break;
                }
                balance -= 1;
            }
        }
        if found.is_some() {
            found
        } else {
            // If not inside brackets, look forward on the current line.
            let (_, line_end) = line_bounds(text, pos);
            text[pos..line_end]
                .char_indices()
                .find(|&(_, ch)| ch == open_ch)
                .map(|(rel, _)| pos + rel)
        }
    }?;

    // Find matching `close_ch` forward from `open_idx`.
    let mut depth = 0usize;
    let mut close_idx = None;
    for (rel, ch) in text[open_idx..].char_indices() {
        if ch == open_ch {
            depth += 1;
        } else if ch == close_ch {
            depth -= 1;
            if depth == 0 {
                close_idx = Some(open_idx + rel);
                break;
            }
        }
    }
    let close_idx = close_idx?;
    if around {
        Some(open_idx..close_idx + close_ch.len_utf8())
    } else {
        Some(open_idx + open_ch.len_utf8()..close_idx)
    }
}

/// Find the enclosing unmatched `open_ch` (`[{`, `[(`) or `close_ch` (`]}`, `])`) around `pos`.
pub fn find_enclosing_delim(
    text: &str,
    pos: usize,
    open_ch: char,
    close_ch: char,
    find_close: bool,
    count: usize,
) -> Option<usize> {
    let mut cur = clamp_char_boundary(text, pos);
    let mut last_hit = None;
    for _ in 0..count.max(1) {
        if find_close {
            let search_from = next_char_end(text, cur).min(text.len());
            let mut depth = 0usize;
            let mut found = None;
            for (rel, ch) in text[search_from..].char_indices() {
                if ch == open_ch {
                    depth += 1;
                } else if ch == close_ch {
                    if depth == 0 {
                        found = Some(search_from + rel);
                        break;
                    }
                    depth -= 1;
                }
            }
            let hit = found?;
            cur = hit;
            last_hit = Some(hit);
        } else {
            let mut depth = 0usize;
            let mut found = None;
            for (idx, ch) in text[..cur].char_indices().rev() {
                if ch == close_ch {
                    depth += 1;
                } else if ch == open_ch {
                    if depth == 0 {
                        found = Some(idx);
                        break;
                    }
                    depth -= 1;
                }
            }
            let hit = found?;
            cur = hit;
            last_hit = Some(hit);
        }
    }
    last_hit
}

/// Extract a file path or module identifier under `pos` for `gf`.
pub fn extract_file_path_under_cursor(text: &str, pos: usize) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let pos = clamp_char_boundary(text, pos);
    let is_path_char = |c: char| {
        c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\' | ':')
    };

    let probe = match text[pos..].chars().next() {
        Some(c) if is_path_char(c) => Some(pos),
        _ if pos > 0 => {
            let prev = prev_char_start(text, pos);
            text[prev..]
                .chars()
                .next()
                .filter(|&c| is_path_char(c))
                .map(|_| prev)
        }
        _ => None,
    }?;

    let mut start = probe;
    while start > 0 {
        let prev = prev_char_start(text, start);
        if text[prev..].chars().next().is_some_and(is_path_char) {
            start = prev;
        } else {
            break;
        }
    }
    let mut end = next_char_end(text, probe);
    while end < text.len() {
        if let Some(c) = text[end..].chars().next() {
            if is_path_char(c) {
                end += c.len_utf8();
                continue;
            }
        }
        break;
    }
    let raw = text[start..end].trim_matches(':');
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_motions_and_utf8_boundaries() {
        let text = "let alpha_1 = \"café\";\n\nfn beta() {}";
        assert_eq!(move_word_forward(text, 0, 1, false, WordMotionPurpose::Move), 4);
        assert_eq!(move_word_forward(text, 4, 1, false, WordMotionPurpose::Move), 12);
        // `cw` on `alpha_1` stops immediately at the end of `alpha_1` without trailing space.
        assert_eq!(
            move_word_forward(text, 4, 1, false, WordMotionPurpose::Change),
            11
        );
        // `e` lands on last char `1` of `alpha_1`.
        let end_alpha = move_word_end(text, 4, 1, false);
        assert_eq!(&text[end_alpha..next_char_end(text, end_alpha)], "1");
        assert_eq!(move_word_backward(text, 12, 1, false), 4);
    }

    #[test]
    fn char_find_bracket_match_and_text_objects() {
        let text = "fn demo(arg: &str) { let msg = \"hello world\"; }";
        let f_paren = find_char_on_line(
            text,
            0,
            CharFind {
                ch: '(',
                direction: FindDirection::Forward,
                till: false,
            },
        )
        .unwrap();
        assert_eq!(&text[f_paren..f_paren + 1], "(");
        let close_paren = find_matching_bracket(text, f_paren).unwrap();
        assert_eq!(&text[close_paren..close_paren + 1], ")");

        let inside_parens = text_object_range(text, f_paren + 2, false, '(').unwrap();
        assert_eq!(&text[inside_parens], "arg: &str");

        let msg_pos = text.find("hello").unwrap();
        let inside_quotes = text_object_range(text, msg_pos, false, '"').unwrap();
        assert_eq!(&text[inside_quotes], "hello world");
        let around_quotes = text_object_range(text, msg_pos, true, '"').unwrap();
        assert_eq!(&text[around_quotes], "\"hello world\"");
    }

    #[test]
    fn smart_case_search_and_word_under_cursor() {
        let text = "foo Bar Foo_Bar foo";
        let (r, w) = word_under_cursor(text, 9).unwrap();
        assert_eq!(w, "Foo_Bar");
        assert_eq!(r, 8..15);

        // Lowercase query is case-insensitive.
        let lower_hits = find_all_matches(text, "foo");
        assert_eq!(lower_hits.len(), 3);
        // Mixed-case query is case-sensitive.
        let upper_hits = find_all_matches(text, "Foo");
        assert_eq!(upper_hits, vec![8..11]);
    }

    #[test]
    fn enclosing_delims_and_gf_path_extraction() {
        let code = "fn outer() { if cond { call(a, b); } }\nuse crate::vim::motions;";
        let inside_if = code.find("call").unwrap();
        let open_if = find_enclosing_delim(code, inside_if, '{', '}', false, 1).unwrap();
        let open_outer = find_enclosing_delim(code, inside_if, '{', '}', false, 2).unwrap();
        let close_if = find_enclosing_delim(code, inside_if, '{', '}', true, 1).unwrap();
        assert_eq!(code[open_outer..=open_outer].chars().next(), Some('{'));
        assert!(open_outer < open_if && open_if < inside_if && inside_if < close_if);

        let mod_pos = code.find("motions").unwrap();
        assert_eq!(
            extract_file_path_under_cursor(code, mod_pos).as_deref(),
            Some("crate::vim::motions")
        );
    }
}
