//! Editing commands bound to keys (see `keymap.rs` for the bindings).
//!
//! Every command is a plain function `fn(&mut Editor, Ctx)`; the `COMMANDS`
//! table at the bottom gives each a name and a one-line description that the
//! command palette shows.

use ropey::Rope;

use crate::editor::{Editor, Mode, PendingChar};
use crate::frame::{self, Dir, Direction, Node};
use crate::history::Transaction;
use crate::movement;
use crate::overlays;
use crate::selection::{Range, Selection};

/// Everything a command receives besides the editor: the count typed before
/// it and whether to extend selections (select mode).
#[derive(Debug, Clone, Copy, Default)]
pub struct Ctx {
    pub count: Option<usize>,
    pub extend: bool,
}

impl Ctx {
    pub fn n(&self) -> usize {
        self.count.unwrap_or(1).max(1)
    }
}

pub struct Command {
    pub name: &'static str,
    pub doc: &'static str,
    pub run: fn(&mut Editor, Ctx),
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Command({})", self.name)
    }
}

pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Transform every range of the focused view's selection.
fn map_selection(ed: &mut Editor, mut f: impl FnMut(&Rope, Range) -> Range) {
    let (view, buffer) = ed.current();
    let len = buffer.len_chars();
    view.selection = view.selection.transform(|r| f(&buffer.text, r).clamp(len));
}

/// Where the block cursor of a range sits, clamped to the text.
fn cursor_of(text: &Rope, range: &Range) -> usize {
    range.head.min(text.len_chars())
}

fn move_horizontally(ed: &mut Editor, ctx: Ctx, delta: isize) {
    let n = ctx.n() as isize;
    map_selection(ed, |text, r| {
        let len = text.len_chars();
        let target = (cursor_of(text, &r) as isize + delta * n).clamp(0, len as isize) as usize;
        r.with_head(target, ctx.extend)
    });
}

fn move_vertically(ed: &mut Editor, ctx: Ctx, delta: isize) {
    let tab_width = ed.config.tab_width;
    let n = ctx.n() as isize;
    map_selection(ed, |text, r| {
        let (pos, col) = movement::move_vertically(text, cursor_of(text, &r), delta * n, r.goal_col, tab_width);
        let mut range = r.with_head(pos, ctx.extend);
        range.goal_col = Some(col);
        range
    });
}

/// Word motions (`w`, `b`, `e`...): in normal mode the selection becomes what
/// Helix would select; in select mode only the head moves.
fn word_motion(ed: &mut Editor, ctx: Ctx, target: movement::WordMotion, long: bool) {
    let n = ctx.n();
    map_selection(ed, |text, r| {
        let (anchor, head) = movement::word_move(text, cursor_of(text, &r), n, target, long);
        if ctx.extend { r.with_head(head, true) } else { Range::new(anchor, head) }
    });
}

/// Put texts on the system clipboard, joined with newlines. The pieces are
/// remembered so that pasting with the same number of cursors hands each
/// cursor its own piece again.
fn copy_to_clipboard(ed: &mut Editor, values: Vec<String>) {
    if let Err(err) = ed.clipboard.copy(&values.join("\n")) {
        ed.set_error(err);
    }
    ed.yanked = values;
}

/// Copy the selected text(s) to the clipboard.
fn yank_selections(ed: &mut Editor) {
    let (view, buffer) = ed.current();
    let values: Vec<String> = view.selection.iter().map(|r| r.slice(&buffer.text)).collect();
    copy_to_clipboard(ed, values);
}

/// What a paste inserts: the clipboard as one text, or one piece per cursor
/// when it still holds the last yank and there are as many cursors as pieces.
fn clipboard_values(ed: &mut Editor) -> Option<Vec<String>> {
    let text = match ed.clipboard.paste() {
        Ok(text) => text,
        Err(err) => {
            ed.set_error(err);
            return None;
        }
    };
    if text.is_empty() {
        ed.set_error("clipboard is empty");
        return None;
    }
    let cursors = ed.view().selection.len();
    if cursors > 1 && ed.yanked.len() == cursors && ed.yanked.join("\n") == text { Some(ed.yanked.clone()) } else { Some(vec![text]) }
}

/// Delete the selections; the cursor lands where the text was.
fn delete_ranges(ed: &mut Editor) {
    let (view, buffer) = ed.current();
    let mut tx = Transaction::new();
    let mut ranges = Vec::new();
    let mut delta = 0usize;
    for r in view.selection.iter() {
        let (from, to) = r.span(&buffer.text);
        tx.delete(from, to);
        ranges.push(Range::point(from - delta));
        delta += to - from;
    }
    let primary = view.selection.primary_index();
    ed.apply(tx, Selection::from_ranges(ranges, primary));
}

/// Replace every selection with `f(text)`; the selections cover the new text.
fn replace_each(ed: &mut Editor, mut f: impl FnMut(&str) -> String) {
    let (view, buffer) = ed.current();
    let mut tx = Transaction::new();
    let mut ranges = Vec::new();
    let mut delta: isize = 0;
    for r in view.selection.iter() {
        let (from, to) = r.span(&buffer.text);
        let new_text = f(&buffer.text.slice(from..to).to_string());
        let new_len = new_text.chars().count();
        tx.replace(from, to, new_text);
        let start = (from as isize + delta) as usize;
        ranges.push(Range::new(start, start + new_len.saturating_sub(1)));
        delta += new_len as isize - (to - from) as isize;
    }
    let primary = view.selection.primary_index();
    ed.apply(tx, Selection::from_ranges(ranges, primary));
}

/// Lines (first..=last) touched by each selection, merged and sorted.
fn selected_lines(ed: &Editor) -> Vec<usize> {
    let (view, buffer) = (ed.view(), ed.buffer());
    let mut lines = Vec::new();
    for r in view.selection.iter() {
        let (from, to) = r.span(&buffer.text);
        let first = buffer.line_of(from);
        let last = buffer.line_of(to.saturating_sub(1).max(from));
        for line in first..=last {
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
    }
    lines.sort_unstable();
    lines
}

/// Apply a transaction built from line edits, keeping the selection in place.
fn apply_keeping_selection(ed: &mut Editor, tx: Transaction) {
    let mapped = tx.map_selection(&ed.view().selection);
    ed.apply(tx, mapped);
}

fn enter_insert(ed: &mut Editor) {
    ed.set_mode(Mode::Insert);
}

/// Typing on the welcome page swaps it for an empty scratch buffer first.
fn leave_welcome(ed: &mut Editor) {
    if ed.buffer().welcome.is_some() {
        crate::welcome::leave_for_scratch(ed);
    }
}

fn window_move(ed: &mut Editor, dir: Direction) {
    let focused = ed.focused_view();
    if let Some(target) = frame::neighbor(&ed.frame_rects, focused, dir) {
        ed.focus_view(target);
    }
}

fn window_swap(ed: &mut Editor, dir: Direction) {
    let focused = ed.focused_view();
    let Some(target) = frame::neighbor(&ed.frame_rects, focused, dir) else { return };
    let placeholder = frame::ViewId(0);
    let root = &mut ed.tab_mut().root;
    root.replace(focused, placeholder);
    root.replace(target, focused);
    root.replace(placeholder, target);
}

// ---------------------------------------------------------------------------
// Movement
// ---------------------------------------------------------------------------

fn move_char_left(ed: &mut Editor, ctx: Ctx) {
    move_horizontally(ed, ctx, -1);
}

fn move_char_right(ed: &mut Editor, ctx: Ctx) {
    move_horizontally(ed, ctx, 1);
}

fn move_line_down(ed: &mut Editor, ctx: Ctx) {
    move_vertically(ed, ctx, 1);
}

fn move_line_up(ed: &mut Editor, ctx: Ctx) {
    move_vertically(ed, ctx, -1);
}

fn move_next_word_start(ed: &mut Editor, ctx: Ctx) {
    word_motion(ed, ctx, movement::WordMotion::NextWordStart, false);
}

fn move_prev_word_start(ed: &mut Editor, ctx: Ctx) {
    word_motion(ed, ctx, movement::WordMotion::PrevWordStart, false);
}

fn move_next_word_end(ed: &mut Editor, ctx: Ctx) {
    word_motion(ed, ctx, movement::WordMotion::NextWordEnd, false);
}

fn move_next_long_word_start(ed: &mut Editor, ctx: Ctx) {
    word_motion(ed, ctx, movement::WordMotion::NextWordStart, true);
}

fn move_prev_long_word_start(ed: &mut Editor, ctx: Ctx) {
    word_motion(ed, ctx, movement::WordMotion::PrevWordStart, true);
}

fn move_next_long_word_end(ed: &mut Editor, ctx: Ctx) {
    word_motion(ed, ctx, movement::WordMotion::NextWordEnd, true);
}

fn find_next_char(ed: &mut Editor, ctx: Ctx) {
    ed.count = ctx.count;
    ed.pending_char = Some(PendingChar::FindChar { forward: true, till: false });
}

fn find_till_char(ed: &mut Editor, ctx: Ctx) {
    ed.count = ctx.count;
    ed.pending_char = Some(PendingChar::FindChar { forward: true, till: true });
}

fn find_prev_char(ed: &mut Editor, ctx: Ctx) {
    ed.count = ctx.count;
    ed.pending_char = Some(PendingChar::FindChar { forward: false, till: false });
}

fn find_till_prev_char(ed: &mut Editor, ctx: Ctx) {
    ed.count = ctx.count;
    ed.pending_char = Some(PendingChar::FindChar { forward: false, till: true });
}

fn goto_line_start(ed: &mut Editor, ctx: Ctx) {
    map_selection(ed, |text, r| {
        let line = text.char_to_line(cursor_of(text, &r));
        r.with_head(text.line_to_char(line), ctx.extend)
    });
}

/// `gl`: the last character of the line (Helix semantics).
fn goto_line_end(ed: &mut Editor, ctx: Ctx) {
    map_selection(ed, |text, r| {
        let line = text.char_to_line(cursor_of(text, &r));
        let start = text.line_to_char(line);
        let end = movement::line_end(text, line);
        r.with_head(end.saturating_sub(1).max(start), ctx.extend)
    });
}

fn goto_first_nonwhitespace(ed: &mut Editor, ctx: Ctx) {
    map_selection(ed, |text, r| {
        let line = text.char_to_line(cursor_of(text, &r));
        let start = text.line_to_char(line);
        let end = movement::line_end(text, line);
        let mut pos = start;
        while pos < end && matches!(text.char(pos), ' ' | '\t') {
            pos += 1;
        }
        r.with_head(pos, ctx.extend)
    });
}

/// Last line of a buffer, ignoring the empty line after a trailing newline.
fn last_line(buffer: &crate::buffer::Buffer) -> usize {
    let count = buffer.line_count();
    if count > 1 && buffer.text.line(count - 1).len_chars() == 0 { count - 2 } else { count - 1 }
}

pub fn goto_line_number(ed: &mut Editor, line: usize, extend: bool) {
    let (view, buffer) = ed.current();
    let line = line.min(buffer.line_count() - 1);
    let pos = buffer.first_nonblank(line);
    let primary = view.selection.primary().with_head(pos, extend);
    view.selection = Selection::single(primary);
}

/// `gg`: the first line, or line `<count>`.
fn goto_file_start(ed: &mut Editor, ctx: Ctx) {
    ed.view_mut().save_jump();
    let last = last_line(ed.buffer());
    let line = ctx.count.map(|n| n.saturating_sub(1).min(last)).unwrap_or(0);
    goto_line_number(ed, line, ctx.extend);
}

fn goto_file_end(ed: &mut Editor, ctx: Ctx) {
    ed.view_mut().save_jump();
    let len = ed.buffer().len_chars();
    let (view, _) = ed.current();
    let primary = view.selection.primary().with_head(len, ctx.extend);
    view.selection = Selection::single(primary);
}

/// Scroll the view by `lines`, moving the cursor along with it (and keeping
/// it inside the scroll-off band, so the view really moves by `lines`).
fn scroll_by(ed: &mut Editor, lines: isize) {
    let tab_width = ed.config.tab_width;
    let animation = ed.config.scroll_animation;
    let scrolloff = ed.config.scrolloff;
    let (view, buffer) = ed.current();
    let last = buffer.line_count() - 1;
    let rows = view.rows.max(1);
    let max_scroll = last.saturating_sub(rows - 1);
    let scroll = (view.scroll_line as isize + lines).clamp(0, max_scroll as isize) as usize;
    view.scroll_to(scroll, animation);
    let scrolloff = scrolloff.min((rows - 1) / 2);
    let top = (scroll + scrolloff).min(last);
    let bottom = (scroll + rows - 1).saturating_sub(scrolloff).min(last).max(top);
    let cursor_line = buffer.line_of(view.cursor());
    let target = ((cursor_line as isize + lines).clamp(0, last as isize) as usize).clamp(top, bottom);
    if target != cursor_line {
        let primary = view.selection.primary();
        let (pos, col) =
            movement::move_vertically(&buffer.text, primary.head, target as isize - cursor_line as isize, primary.goal_col, tab_width);
        let mut range = Range::point(pos);
        range.goal_col = Some(col);
        view.selection = Selection::single(range);
    }
}

fn half_page_down(ed: &mut Editor, _ctx: Ctx) {
    let rows = ed.view().rows.max(2) as isize;
    scroll_by(ed, rows / 2);
}

fn half_page_up(ed: &mut Editor, _ctx: Ctx) {
    let rows = ed.view().rows.max(2) as isize;
    scroll_by(ed, -(rows / 2));
}

fn jump_to(ed: &mut Editor, index: usize) {
    let view = ed.view_mut();
    let Some(jump) = view.jumps.get(index).cloned() else { return };
    view.jump_index = index;
    if ed.buffers.contains_key(&jump.buffer) {
        ed.show_buffer(jump.buffer);
        let len = ed.buffer().len_chars();
        ed.view_mut().selection = jump.selection.clamp(len);
    }
}

fn jump_backward(ed: &mut Editor, ctx: Ctx) {
    let view = ed.view_mut();
    if view.jump_index == view.jumps.len() {
        view.save_jump();
        view.jump_index = view.jumps.len() - 1;
    }
    let index = view.jump_index.saturating_sub(ctx.n());
    jump_to(ed, index);
}

fn jump_forward(ed: &mut Editor, ctx: Ctx) {
    let view = ed.view();
    let index = (view.jump_index + ctx.n()).min(view.jumps.len().saturating_sub(1));
    jump_to(ed, index);
}

fn cycle_buffer(ed: &mut Editor, delta: isize) {
    let ids = ed.buffer_ids();
    let current = ed.view().buffer;
    let Some(i) = ids.iter().position(|id| *id == current) else { return };
    let next = (i as isize + delta).rem_euclid(ids.len() as isize) as usize;
    ed.show_buffer(ids[next]);
}

fn goto_next_buffer(ed: &mut Editor, _ctx: Ctx) {
    cycle_buffer(ed, 1);
}

fn goto_previous_buffer(ed: &mut Editor, _ctx: Ctx) {
    cycle_buffer(ed, -1);
}

fn goto_last_modification(ed: &mut Editor, ctx: Ctx) {
    let Some(pos) = ed.buffer().last_change else {
        ed.set_error("no changes yet");
        return;
    };
    let (view, buffer) = ed.current();
    let pos = pos.min(buffer.len_chars());
    view.selection = Selection::single(view.selection.primary().with_head(pos, ctx.extend));
}

fn goto_file(ed: &mut Editor, _ctx: Ctx) {
    let name = ed.primary_text_or_word();
    if name.is_empty() {
        ed.set_error("no file name under the cursor");
        return;
    }
    let base = ed.buffer().path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let mut candidates = vec![ed.cwd.join(&name)];
    if let Some(base) = base {
        candidates.insert(0, base.join(&name));
    }
    match candidates.into_iter().find(|p| p.is_file()) {
        Some(path) => match ed.open_file(&path) {
            Ok(id) => {
                ed.view_mut().save_jump();
                ed.show_buffer(id);
            }
            Err(err) => ed.set_error(err),
        },
        None => ed.set_error(format!("file not found: {name}")),
    }
}

// ---------------------------------------------------------------------------
// Selection manipulation
// ---------------------------------------------------------------------------

fn extend_line_below(ed: &mut Editor, ctx: Ctx) {
    let n = ctx.n();
    map_selection(ed, |text, r| {
        let (from, to) = r.span(text);
        let first = text.char_to_line(from);
        let mut last = text.char_to_line(to.saturating_sub(1).max(from));
        let last_line = text.len_lines() - 1;
        let covers_lines = from == text.line_to_char(first) && r.to() > movement::line_end(text, last);
        if covers_lines {
            last = (last + n).min(last_line);
        } else {
            last = (last + n - 1).min(last_line);
        }
        Range::new(text.line_to_char(first), movement::line_end(text, last))
    });
}

/// `X`: like `x`, but the selection grows upwards (the cursor ends on the top line).
fn extend_line_above(ed: &mut Editor, ctx: Ctx) {
    let n = ctx.n();
    map_selection(ed, |text, r| {
        let (from, to) = r.span(text);
        let mut first = text.char_to_line(from);
        let last = text.char_to_line(to.saturating_sub(1).max(from));
        let covers_lines = from == text.line_to_char(first) && r.to() > movement::line_end(text, last);
        first = first.saturating_sub(if covers_lines { n } else { n - 1 });
        Range::new(movement::line_end(text, last), text.line_to_char(first))
    });
}

/// `J`: pull the line below onto the end of the current line, vim style: one
/// space in between and the next line's indentation dropped. A selection over
/// several lines joins all of them; a count joins that many lines.
fn join_lines(ed: &mut Editor, ctx: Ctx) {
    let lines = selected_lines(ed);
    let (view, buffer) = ed.current();
    let text = &buffer.text;
    let first = lines[0];
    let last = if lines.len() > 1 { *lines.last().expect("non-empty") } else { first + ctx.n() };
    let last = last.min(text.len_lines().saturating_sub(1));
    let mut tx = Transaction::new();
    for line in first..last {
        let newline = movement::line_end(text, line);
        let mut next = text.line_to_char(line + 1);
        let next_end = movement::line_end(text, line + 1);
        while next < next_end && matches!(text.char(next), ' ' | '\t') {
            next += 1;
        }
        let current_empty = newline == text.line_to_char(line);
        let next_empty = next == next_end;
        let separator = if current_empty || next_empty { "" } else { " " };
        tx.replace(newline, next, separator);
    }
    if tx.is_empty() {
        return;
    }
    let cursor = tx.map(movement::line_end(text, first));
    view.selection = Selection::point(cursor);
    let selection = view.selection.clone();
    ed.apply(tx, selection);
}

/// Grow every selection to the whole lines it touches (used by linewise paste).
fn extend_to_line_bounds(ed: &mut Editor) {
    map_selection(ed, |text, r| {
        let (from, to) = r.span(text);
        let first = text.char_to_line(from);
        let last = text.char_to_line(to.saturating_sub(1).max(from));
        Range::new(text.line_to_char(first), movement::line_end(text, last))
    });
}

fn select_all(ed: &mut Editor, _ctx: Ctx) {
    let len = ed.buffer().len_chars();
    ed.view_mut().selection = Selection::single(Range::new(0, len));
}

fn collapse_selection(ed: &mut Editor, _ctx: Ctx) {
    map_selection(ed, |_, r| r.collapsed());
}

fn flip_selections(ed: &mut Editor, _ctx: Ctx) {
    map_selection(ed, |_, r| r.flipped());
}

fn keep_primary_selection(ed: &mut Editor, _ctx: Ctx) {
    let view = ed.view_mut();
    view.selection = view.selection.keep_primary();
}

fn copy_selection_on_line(ed: &mut Editor, ctx: Ctx, delta: isize) {
    let tab_width = ed.config.tab_width;
    let (view, buffer) = ed.current();
    let text = &buffer.text;
    let last_line = text.len_lines() - 1;
    let mut ranges: Vec<Range> = view.selection.ranges().to_vec();
    let mut new_primary = view.selection.primary_index();
    for (i, r) in view.selection.iter().enumerate() {
        let anchor_col = movement::column_of(text, r.anchor, tab_width);
        let head_col = movement::column_of(text, r.head, tab_width);
        let needed = anchor_col.max(head_col);
        let mut line = text.char_to_line(cursor_of(text, r)) as isize;
        let mut copies = 0;
        while copies < ctx.n() {
            line += delta;
            if line < 0 || line as usize > last_line {
                break;
            }
            let l = line as usize;
            let width = movement::column_of(text, movement::line_end(text, l), tab_width);
            if width < needed && !(width == 0 && needed == 0) {
                continue;
            }
            let anchor = movement::pos_at_column(text, l, anchor_col, tab_width);
            let head = movement::pos_at_column(text, l, head_col, tab_width);
            ranges.push(Range::new(anchor, head));
            if i == view.selection.primary_index() {
                new_primary = ranges.len() - 1;
            }
            copies += 1;
        }
    }
    let primary_range = ranges[new_primary];
    let mut selection = Selection::from_ranges(ranges, new_primary);
    let index = selection.iter().position(|r| r.head == primary_range.head);
    if let Some(i) = index {
        selection.set_primary(i);
    }
    view.selection = selection;
}

fn copy_selection_on_next_line(ed: &mut Editor, ctx: Ctx) {
    copy_selection_on_line(ed, ctx, 1);
}

fn copy_selection_on_prev_line(ed: &mut Editor, ctx: Ctx) {
    copy_selection_on_line(ed, ctx, -1);
}

fn select_regex(ed: &mut Editor, _ctx: Ctx) {
    overlays::open_prompt(ed, crate::picker::PromptKind::SelectRegex, "select:");
}

fn select_mode(ed: &mut Editor, _ctx: Ctx) {
    let mode = if ed.mode == Mode::Select { Mode::Normal } else { Mode::Select };
    ed.set_mode(mode);
}

fn normal_mode(ed: &mut Editor, _ctx: Ctx) {
    if ed.mode == Mode::Terminal {
        let buffer = ed.view().buffer;
        ed.refresh_terminal_text(buffer);
    }
    ed.set_mode(Mode::Normal);
}

// ---------------------------------------------------------------------------
// Match menu: brackets and text objects
// ---------------------------------------------------------------------------

fn match_brackets(ed: &mut Editor, ctx: Ctx) {
    map_selection(ed, |text, r| match movement::goto_matching_bracket(text, cursor_of(text, &r)) {
        Some(pos) => r.with_head(pos, ctx.extend),
        None => r,
    });
}

fn select_textobject_inner(ed: &mut Editor, ctx: Ctx) {
    ed.count = ctx.count;
    ed.pending_char = Some(PendingChar::TextObject { around: false });
}

fn select_textobject_around(ed: &mut Editor, ctx: Ctx) {
    ed.count = ctx.count;
    ed.pending_char = Some(PendingChar::TextObject { around: true });
}

/// The pair of delimiters a text-object character stands for.
fn pair_for(c: char) -> (char, char) {
    match c {
        '(' | ')' | 'b' => ('(', ')'),
        '[' | ']' => ('[', ']'),
        '{' | '}' | 'B' => ('{', '}'),
        '<' | '>' => ('<', '>'),
        other => (other, other),
    }
}

/// Text object range (inclusive) for the object `c` around `pos`.
fn text_object(text: &Rope, pos: usize, c: char, around: bool, count: usize) -> Option<(usize, usize)> {
    match c {
        'w' | 'W' => {
            let (start, mut end) = movement::word_at(text, pos, c == 'W')?;
            if around {
                while end + 1 < text.len_chars() && matches!(text.char(end + 1), ' ' | '\t') {
                    end += 1;
                }
            }
            Some((start, end))
        }
        'p' => {
            let (start, mut end) = movement::paragraph_at(text, pos);
            if around {
                let len = text.len_chars();
                while end + 1 < len && text.char(end + 1).is_whitespace() {
                    end += 1;
                }
            }
            Some((start, end))
        }
        'm' => {
            // Closest surrounding pair of any bracket kind.
            let mut best: Option<(usize, usize)> = None;
            for (open, close) in movement::BRACKETS {
                if let Some((o, cl)) = movement::surrounding_pair(text, pos, open, close, count)
                    && best.is_none_or(|(bo, _)| o > bo)
                {
                    best = Some((o, cl));
                }
            }
            let (open, close) = best?;
            Some(if around { (open, close) } else { (open + 1, close.checked_sub(1)?) })
        }
        _ => {
            let (open, close) = pair_for(c);
            let (o, cl) = movement::surrounding_pair(text, pos, open, close, count)?;
            if around {
                Some((o, cl))
            } else if cl > o + 1 {
                Some((o + 1, cl - 1))
            } else {
                Some((o + 1, o)) // empty: a cursor between the delimiters
            }
        }
    }
}

/// Handle the key typed after `f`, `r`, `mi`...
pub fn pending_char(ed: &mut Editor, pending: PendingChar, c: char, ctx: Ctx) {
    match pending {
        PendingChar::FindChar { forward, till } => {
            let n = ctx.n();
            map_selection(ed, |text, r| match movement::find_char(text, cursor_of(text, &r), c, forward, till, n) {
                Some(pos) => Range::new(r.anchor, pos),
                None => r,
            });
        }
        PendingChar::ReplaceChar => {
            // Vim's `r`: only the character under each cursor changes.
            let (view, buffer) = ed.current();
            let len = buffer.len_chars();
            let mut tx = Transaction::new();
            let mut ranges = Vec::new();
            for r in view.selection.iter() {
                let at = r.head.min(len);
                if at < len && buffer.text.char(at) != '\n' {
                    tx.replace(at, at + 1, c.to_string());
                }
                ranges.push(Range::point(at));
            }
            let primary = view.selection.primary_index();
            ed.apply(tx, Selection::from_ranges(ranges, primary));
        }
        PendingChar::TextObject { around } => {
            let n = ctx.n();
            map_selection(ed, |text, r| match text_object(text, cursor_of(text, &r), c, around, n) {
                Some((start, end)) if start <= end => Range::new(start, end),
                Some((start, _)) => Range::point(start),
                None => r,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Changes
// ---------------------------------------------------------------------------

fn insert_mode(ed: &mut Editor, _ctx: Ctx) {
    leave_welcome(ed);
    map_selection(ed, |text, r| Range::point(r.span(text).0));
    enter_insert(ed);
}

fn append_mode(ed: &mut Editor, _ctx: Ctx) {
    leave_welcome(ed);
    map_selection(ed, |text, r| Range::point(r.span(text).1));
    enter_insert(ed);
}

fn insert_at_line_start(ed: &mut Editor, _ctx: Ctx) {
    leave_welcome(ed);
    map_selection(ed, |text, r| {
        let line = text.char_to_line(r.span(text).0);
        let start = text.line_to_char(line);
        let end = movement::line_end(text, line);
        let mut pos = start;
        while pos < end && matches!(text.char(pos), ' ' | '\t') {
            pos += 1;
        }
        Range::point(pos)
    });
    enter_insert(ed);
}

fn insert_at_line_end(ed: &mut Editor, _ctx: Ctx) {
    leave_welcome(ed);
    map_selection(ed, |text, r| {
        let (from, to) = r.span(text);
        let line = text.char_to_line(to.saturating_sub(1).max(from));
        Range::point(movement::line_end(text, line))
    });
    enter_insert(ed);
}

fn open_line(ed: &mut Editor, below: bool) {
    leave_welcome(ed);
    if ed.buffer().readonly {
        ed.set_error("buffer is read-only");
        return;
    }
    let (view, buffer) = ed.current();
    let mut tx = Transaction::new();
    let mut ranges = Vec::new();
    let mut delta = 0usize;
    for r in view.selection.iter() {
        let (from, to) = r.span(&buffer.text);
        let line = buffer.line_of(if below { to.saturating_sub(1).max(from) } else { from });
        let mut indent = buffer.line_indent(line);
        // In the tree, `o` on an unfolded directory starts the line inside it:
        // the new entry takes the indentation of the directory's first child.
        if below && buffer.tree && line + 1 < buffer.line_count() && buffer.line_indent(line + 1).len() > indent.len() {
            indent = buffer.line_indent(line + 1);
        }
        let (at, text) =
            if below { (buffer.line_end(line), format!("\n{indent}")) } else { (buffer.line_start(line), format!("{indent}\n")) };
        let cursor = if below { at + text.chars().count() } else { at + indent.chars().count() };
        tx.insert(at, text.clone());
        ranges.push(Range::point(cursor + delta));
        delta += text.chars().count();
    }
    let primary = view.selection.primary_index();
    ed.apply(tx, Selection::from_ranges(ranges, primary));
    enter_insert(ed);
}

fn open_below(ed: &mut Editor, _ctx: Ctx) {
    open_line(ed, true);
}

fn open_above(ed: &mut Editor, _ctx: Ctx) {
    open_line(ed, false);
}

fn delete_selection(ed: &mut Editor, _ctx: Ctx) {
    if ed.buffer().readonly {
        ed.set_error("buffer is read-only");
        return;
    }
    yank_selections(ed);
    delete_ranges(ed);
}

fn delete_selection_noyank(ed: &mut Editor, _ctx: Ctx) {
    delete_ranges(ed);
}

fn change_selection(ed: &mut Editor, ctx: Ctx) {
    leave_welcome(ed);
    if ed.buffer().readonly {
        ed.set_error("buffer is read-only");
        return;
    }
    delete_selection(ed, ctx);
    enter_insert(ed);
}

fn change_selection_noyank(ed: &mut Editor, _ctx: Ctx) {
    if ed.buffer().readonly {
        ed.set_error("buffer is read-only");
        return;
    }
    delete_ranges(ed);
    enter_insert(ed);
}

fn yank(ed: &mut Editor, _ctx: Ctx) {
    yank_selections(ed);
    let count = ed.yanked.len();
    ed.set_message(format!("yanked {count} selection{} to the clipboard", if count == 1 { "" } else { "s" }));
}

/// Vim-style paste. In normal mode the clipboard goes in next to the
/// selection without touching it: `p` after it, `P` before it; whole lines on
/// the clipboard (text ending in a newline) go on the line below or above. In
/// select mode the clipboard replaces the selection like vim's visual-mode
/// paste, and `p` (not `P`) puts the replaced text on the clipboard.
fn paste_with(ed: &mut Editor, ctx: Ctx, after: bool) {
    if ed.buffer().is_terminal() {
        // A terminal buffer: type the clipboard into the shell.
        if let Ok(text) = ed.clipboard.paste()
            && let Some(terminal) = &mut ed.buffer_mut().terminal
        {
            terminal.send_text(&text);
        }
        ed.set_mode(Mode::Terminal);
        return;
    }
    if ed.buffer().readonly {
        ed.set_error("buffer is read-only");
        return;
    }
    let Some(values) = clipboard_values(ed) else { return };
    let linewise = values.iter().all(|v| v.ends_with('\n'));
    if ed.mode == Mode::Select {
        if linewise {
            extend_to_line_bounds(ed);
        }
        let replaced: Vec<String> = {
            let (view, buffer) = ed.current();
            view.selection.iter().map(|r| r.slice(&buffer.text)).collect()
        };
        let mut i = 0;
        replace_each(ed, |_| {
            let value = values[i % values.len()].repeat(ctx.n());
            i += 1;
            value
        });
        if after {
            copy_to_clipboard(ed, replaced);
        }
        ed.set_mode(Mode::Normal);
        return;
    }
    let (view, buffer) = ed.current();
    let mut tx = Transaction::new();
    let mut ranges = Vec::new();
    let mut delta = 0usize;
    for (i, r) in view.selection.iter().enumerate() {
        let mut value = values[i % values.len()].repeat(ctx.n());
        let (from, to) = r.span(&buffer.text);
        let at = match (linewise, after) {
            (true, true) => {
                let line = buffer.line_of(to.saturating_sub(1).max(from));
                let end = buffer.len_chars();
                if line + 1 < buffer.line_count() {
                    buffer.line_start(line + 1)
                } else {
                    // Below the last line, which has no newline of its own yet.
                    if end > 0 && buffer.text.char(end - 1) != '\n' {
                        value = format!("\n{}", value.strip_suffix('\n').unwrap_or(&value));
                    }
                    end
                }
            }
            (true, false) => buffer.line_start(buffer.line_of(from)),
            (false, true) => to,
            (false, false) => from,
        };
        let len = value.chars().count();
        let start = at + delta;
        ranges.push(Range::new(start, start + len.saturating_sub(1)));
        tx.insert(at, value);
        delta += len;
    }
    let primary = view.selection.primary_index();
    ed.apply(tx, Selection::from_ranges(ranges, primary));
}

fn paste(ed: &mut Editor, ctx: Ctx) {
    paste_with(ed, ctx, true);
}

fn paste_noyank(ed: &mut Editor, ctx: Ctx) {
    paste_with(ed, ctx, false);
}

fn replace_char(ed: &mut Editor, _ctx: Ctx) {
    ed.pending_char = Some(PendingChar::ReplaceChar);
}

fn replace_mode(ed: &mut Editor, _ctx: Ctx) {
    map_selection(ed, |_, r| Range::point(r.head));
    ed.set_mode(Mode::Replace);
}

fn indent(ed: &mut Editor, ctx: Ctx) {
    let unit = ed.buffer().indent_unit().repeat(ctx.n());
    let lines = selected_lines(ed);
    let buffer = ed.buffer();
    let mut tx = Transaction::new();
    for line in lines {
        if buffer.line_end(line) > buffer.line_start(line) {
            tx.insert(buffer.line_start(line), unit.clone());
        }
    }
    apply_keeping_selection(ed, tx);
}

fn unindent(ed: &mut Editor, ctx: Ctx) {
    let width = ed.buffer().indent.width;
    let lines = selected_lines(ed);
    let buffer = ed.buffer();
    let mut tx = Transaction::new();
    for line in lines {
        let start = buffer.line_start(line);
        let end = buffer.line_end(line);
        let mut pos = start;
        for _ in 0..ctx.n() {
            if pos < end && buffer.text.char(pos) == '\t' {
                pos += 1;
            } else {
                let mut spaces = 0;
                while pos < end && spaces < width && buffer.text.char(pos) == ' ' {
                    pos += 1;
                    spaces += 1;
                }
            }
        }
        tx.delete(start, pos);
    }
    apply_keeping_selection(ed, tx);
}

fn undo(ed: &mut Editor, ctx: Ctx) {
    for _ in 0..ctx.n() {
        let selection = ed.buffer_mut().undo();
        match selection {
            Some(selection) => {
                let len = ed.buffer().len_chars();
                ed.view_mut().selection = selection.clamp(len);
                clamp_other_views(ed);
            }
            None => {
                ed.set_error("already at oldest change");
                break;
            }
        }
    }
}

fn redo(ed: &mut Editor, ctx: Ctx) {
    for _ in 0..ctx.n() {
        let selection = ed.buffer_mut().redo();
        match selection {
            Some(selection) => {
                let len = ed.buffer().len_chars();
                ed.view_mut().selection = selection.clamp(len);
                clamp_other_views(ed);
            }
            None => {
                ed.set_error("already at newest change");
                break;
            }
        }
    }
}

fn clamp_other_views(ed: &mut Editor) {
    let buffer = ed.view().buffer;
    let len = ed.buffer().len_chars();
    for view in ed.views.values_mut() {
        if view.buffer == buffer {
            view.selection = view.selection.clamp(len);
        }
    }
}

fn toggle_comments(ed: &mut Editor, _ctx: Ctx) {
    let token = ed.buffer().language.and_then(|l| ed.registry.get(l).line_comment.clone()).unwrap_or_else(|| "//".to_string());
    let lines = selected_lines(ed);
    let buffer = ed.buffer();
    let text = &buffer.text;
    // Comment out unless every non-blank line is already commented.
    let mut min_indent = usize::MAX;
    let mut all_commented = true;
    let mut targets = Vec::new();
    for &line in &lines {
        let start = buffer.first_nonblank(line);
        let end = buffer.line_end(line);
        if start == end {
            continue;
        }
        let indent_len = start - buffer.line_start(line);
        min_indent = min_indent.min(indent_len);
        let rest: String = text.slice(start..end).to_string();
        if !rest.starts_with(&token) {
            all_commented = false;
        }
        targets.push((line, start, rest));
    }
    if targets.is_empty() {
        return;
    }
    let mut tx = Transaction::new();
    for (line, start, rest) in targets {
        if all_commented {
            let mut remove = token.chars().count();
            if rest[token.len()..].starts_with(' ') {
                remove += 1;
            }
            tx.delete(start, start + remove);
        } else {
            tx.insert(buffer.line_start(line) + min_indent, format!("{token} "));
        }
    }
    apply_keeping_selection(ed, tx);
}

fn repeat_last_insert(ed: &mut Editor, _ctx: Ctx) {
    ed.replay_last_insert();
}

// ---------------------------------------------------------------------------
// Insert and replace mode
// ---------------------------------------------------------------------------

fn auto_pairs(ed: &Editor) -> Vec<(char, char)> {
    if !ed.config.auto_pairs {
        return Vec::new();
    }
    let from_language = ed.buffer().language.map(|l| ed.registry.get(l).auto_pairs.clone()).unwrap_or_default();
    if from_language.is_empty() { vec![('(', ')'), ('[', ']'), ('{', '}'), ('"', '"'), ('\'', '\'')] } else { from_language }
}

/// Type a character at every cursor, with auto-pairing of brackets and quotes.
pub fn insert_char(ed: &mut Editor, c: char) {
    let pairs = auto_pairs(ed);
    let (view, buffer) = ed.current();
    let text = &buffer.text;
    let len = text.len_chars();
    let mut tx = Transaction::new();
    let mut ranges = Vec::new();
    let mut delta = 0usize;
    for r in view.selection.iter() {
        let at = r.head.min(len);
        let next = if at < len { Some(text.char(at)) } else { None };
        let prev = if at > 0 { Some(text.char(at - 1)) } else { None };
        // Typing a closing delimiter that is already there just steps over it.
        if pairs.iter().any(|(_, close)| *close == c) && next == Some(c) {
            ranges.push(Range::point(at + delta + 1));
            continue;
        }
        let opens_pair = pairs.iter().find(|(open, _)| *open == c).map(|(_, close)| *close);
        let next_is_boundary = next.is_none_or(|n| n.is_whitespace() || ")]}>,;".contains(n));
        let prev_is_word = prev.is_some_and(|p| p.is_alphanumeric());
        let insert = match opens_pair {
            // Quotes after a word character (e.g. `don't`) are not paired.
            Some(close) if next_is_boundary && !(c == close && prev_is_word) => format!("{c}{close}"),
            _ => c.to_string(),
        };
        tx.insert(at, insert.clone());
        ranges.push(Range::point(at + delta + 1));
        delta += insert.chars().count();
    }
    let primary = view.selection.primary_index();
    ed.apply(tx, Selection::from_ranges(ranges, primary));
}

/// Type a character in replace mode: it overwrites the character under the
/// cursor, or is inserted at the end of a line.
pub fn overwrite_char(ed: &mut Editor, c: char) {
    let (view, buffer) = ed.current();
    let text = &buffer.text;
    let len = text.len_chars();
    let mut tx = Transaction::new();
    let mut ranges = Vec::new();
    let mut delta = 0usize;
    for r in view.selection.iter() {
        let at = r.head.min(len);
        let overwrites = at < len && text.char(at) != '\n';
        if overwrites {
            tx.replace(at, at + 1, c.to_string());
        } else {
            tx.insert(at, c.to_string());
        }
        ranges.push(Range::point(at + delta + 1));
        if !overwrites {
            delta += 1;
        }
    }
    let primary = view.selection.primary_index();
    ed.apply(tx, Selection::from_ranges(ranges, primary));
}

fn insert_newline(ed: &mut Editor, _ctx: Ctx) {
    let unit = ed.buffer().indent_unit();
    let (view, buffer) = ed.current();
    let text = &buffer.text;
    let len = text.len_chars();
    let mut tx = Transaction::new();
    let mut ranges = Vec::new();
    let mut delta = 0usize;
    for r in view.selection.iter() {
        let at = r.head.min(len);
        let line = text.char_to_line(at);
        let mut indent = buffer.line_indent(line);
        // Do not copy indentation that lies after the cursor.
        let indent_available = at - text.line_to_char(line);
        if indent.chars().count() > indent_available {
            indent = indent.chars().take(indent_available).collect();
        }
        let prev = if at > 0 { Some(text.char(at - 1)) } else { None };
        let next = if at < len { Some(text.char(at)) } else { None };
        let opens_block = matches!(prev, Some('{') | Some('(') | Some('['));
        let closes_block = matches!((prev, next), (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']')));
        let mut insert = format!("\n{indent}");
        if opens_block {
            insert.push_str(&unit);
        }
        let cursor = at + delta + insert.chars().count();
        if closes_block {
            // Put the closing bracket on its own line below the cursor.
            insert.push('\n');
            insert.push_str(&indent);
        }
        tx.insert(at, insert.clone());
        ranges.push(Range::point(cursor));
        delta += insert.chars().count();
    }
    let primary = view.selection.primary_index();
    ed.apply(tx, Selection::from_ranges(ranges, primary));
}

fn insert_tab(ed: &mut Editor, _ctx: Ctx) {
    let unit = ed.buffer().indent_unit();
    ed.insert_text(&unit);
}

/// Delete a range relative to each cursor; `f(text, cursor) -> (from, to)`.
fn delete_around_cursors(ed: &mut Editor, f: impl Fn(&Rope, usize) -> (usize, usize)) {
    let (view, buffer) = ed.current();
    let text = &buffer.text;
    let mut tx = Transaction::new();
    let mut ranges = Vec::new();
    let mut delta = 0usize;
    for r in view.selection.iter() {
        let at = r.head.min(text.len_chars());
        let (from, to) = f(text, at);
        let (from, to) = (from.min(to), to.max(from));
        tx.delete(from, to);
        ranges.push(Range::point(from - delta));
        delta += to - from;
    }
    let primary = view.selection.primary_index();
    ed.apply(tx, Selection::from_ranges(ranges, primary));
}

fn delete_char_backward(ed: &mut Editor, _ctx: Ctx) {
    let pairs = auto_pairs(ed);
    let width = ed.buffer().indent.width.max(1);
    delete_around_cursors(ed, |text, at| {
        if at == 0 {
            return (0, 0);
        }
        let prev = text.char(at - 1);
        let next = if at < text.len_chars() { Some(text.char(at)) } else { None };
        // Deleting the opener of an empty pair removes the closer too.
        if pairs.iter().any(|(open, close)| *open == prev && Some(*close) == next) {
            return (at - 1, at + 1);
        }
        // Inside leading indentation, delete a whole indent level of spaces.
        let line_start = text.line_to_char(text.char_to_line(at));
        let before: String = text.slice(line_start..at).to_string();
        if prev == ' ' && before.chars().all(|c| c == ' ') && !before.is_empty() {
            let remove = ((before.len() - 1) % width) + 1;
            return (at - remove, at);
        }
        (at - 1, at)
    });
}

fn delete_char_forward(ed: &mut Editor, _ctx: Ctx) {
    delete_around_cursors(ed, |text, at| (at, (at + 1).min(text.len_chars())));
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

fn search(ed: &mut Editor, ctx: Ctx) {
    overlays::open_prompt(ed, crate::picker::PromptKind::Search { reverse: false, extend: ctx.extend }, "search:");
}

fn rsearch(ed: &mut Editor, ctx: Ctx) {
    overlays::open_prompt(ed, crate::picker::PromptKind::Search { reverse: true, extend: ctx.extend }, "rsearch:");
}

fn search_next(ed: &mut Editor, ctx: Ctx) {
    for _ in 0..ctx.n() {
        overlays::search_step(ed, false, ctx.extend, true);
    }
}

fn search_prev(ed: &mut Editor, ctx: Ctx) {
    for _ in 0..ctx.n() {
        overlays::search_step(ed, true, ctx.extend, true);
    }
}

fn search_selection(ed: &mut Editor, _ctx: Ctx) {
    let text = ed.view().selection.primary().slice(&ed.buffer().text);
    if text.is_empty() {
        return;
    }
    let pattern = regex::escape(&text);
    match crate::search::compile(&pattern) {
        Ok(regex) => {
            ed.search.regex = Some(regex);
            ed.search.history.push(pattern.clone());
            ed.set_message(format!("search pattern set to {pattern}"));
        }
        Err(err) => ed.set_error(err),
    }
}

// ---------------------------------------------------------------------------
// Pickers, frames, tabs
// ---------------------------------------------------------------------------

fn command_palette(ed: &mut Editor, _ctx: Ctx) {
    overlays::open_command_palette(ed);
}

fn file_picker(ed: &mut Editor, _ctx: Ctx) {
    overlays::open_file_picker(ed, false);
}

fn file_picker_all(ed: &mut Editor, _ctx: Ctx) {
    overlays::open_file_picker(ed, true);
}

fn buffer_picker(ed: &mut Editor, _ctx: Ctx) {
    overlays::open_buffer_picker(ed);
}

fn global_search(ed: &mut Editor, _ctx: Ctx) {
    overlays::open_global_search(ed);
}

fn cycle_window(ed: &mut Editor, delta: isize) {
    let leaves = ed.root().leaves();
    let focused = ed.focused_view();
    let i = leaves.iter().position(|v| *v == focused).unwrap_or(0) as isize;
    let next = (i + delta).rem_euclid(leaves.len() as isize) as usize;
    ed.focus_view(leaves[next]);
}

fn window_next(ed: &mut Editor, _ctx: Ctx) {
    cycle_window(ed, 1);
}

fn window_prev(ed: &mut Editor, _ctx: Ctx) {
    cycle_window(ed, -1);
}

fn vsplit(ed: &mut Editor, _ctx: Ctx) {
    ed.split(Dir::Horizontal, None);
}

fn hsplit(ed: &mut Editor, _ctx: Ctx) {
    ed.split(Dir::Vertical, None);
}

fn vsplit_new(ed: &mut Editor, _ctx: Ctx) {
    let buffer = ed.new_scratch_buffer();
    ed.split(Dir::Horizontal, Some(buffer));
}

fn hsplit_new(ed: &mut Editor, _ctx: Ctx) {
    let buffer = ed.new_scratch_buffer();
    ed.split(Dir::Vertical, Some(buffer));
}

fn window_close(ed: &mut Editor, _ctx: Ctx) {
    let focused = ed.focused_view();
    if ed.root().leaves().len() == 1 && ed.tabs.len() == 1 {
        ed.set_error("cannot close the last frame (use :quit)");
        return;
    }
    ed.close_view(focused);
}

fn window_only(ed: &mut Editor, _ctx: Ctx) {
    let focused = ed.focused_view();
    let others: Vec<_> = ed.root().leaves().into_iter().filter(|v| *v != focused).collect();
    for view in others {
        ed.views.remove(&view);
    }
    ed.tab_mut().root = Node::Leaf(focused);
}

fn window_left(ed: &mut Editor, _ctx: Ctx) {
    window_move(ed, Direction::Left);
}

fn window_right(ed: &mut Editor, _ctx: Ctx) {
    window_move(ed, Direction::Right);
}

fn window_up(ed: &mut Editor, _ctx: Ctx) {
    window_move(ed, Direction::Up);
}

fn window_down(ed: &mut Editor, _ctx: Ctx) {
    window_move(ed, Direction::Down);
}

fn window_swap_left(ed: &mut Editor, _ctx: Ctx) {
    window_swap(ed, Direction::Left);
}

fn window_swap_right(ed: &mut Editor, _ctx: Ctx) {
    window_swap(ed, Direction::Right);
}

fn window_swap_up(ed: &mut Editor, _ctx: Ctx) {
    window_swap(ed, Direction::Up);
}

fn window_swap_down(ed: &mut Editor, _ctx: Ctx) {
    window_swap(ed, Direction::Down);
}

fn tab_new(ed: &mut Editor, _ctx: Ctx) {
    ed.tab_new(None);
}

fn tab_close(ed: &mut Editor, _ctx: Ctx) {
    ed.tab_close();
}

fn tab_next(ed: &mut Editor, ctx: Ctx) {
    let n = ed.tabs.len();
    let target = (ed.tab_index + ctx.n()) % n;
    ed.tab_goto(target);
}

fn tab_prev(ed: &mut Editor, ctx: Ctx) {
    let n = ed.tabs.len();
    let target = (ed.tab_index + n - ctx.n() % n) % n;
    ed.tab_goto(target);
}

/// Resize the focused frame by a twentieth of its split per press.
fn resize(ed: &mut Editor, dir: Dir, delta: f32) {
    let focused = ed.focused_view();
    if ed.tab_mut().root.resize(focused, dir, delta) != Some(true) {
        ed.set_error("no frame to take the space from in that direction");
    }
}

fn resize_taller(ed: &mut Editor, ctx: Ctx) {
    resize(ed, Dir::Vertical, 0.05 * ctx.n() as f32);
}

fn resize_shorter(ed: &mut Editor, ctx: Ctx) {
    resize(ed, Dir::Vertical, -0.05 * ctx.n() as f32);
}

fn resize_wider(ed: &mut Editor, ctx: Ctx) {
    resize(ed, Dir::Horizontal, 0.05 * ctx.n() as f32);
}

fn resize_narrower(ed: &mut Editor, ctx: Ctx) {
    resize(ed, Dir::Horizontal, -0.05 * ctx.n() as f32);
}

fn resize_reset(ed: &mut Editor, _ctx: Ctx) {
    ed.tab_mut().root.reset_weights();
}

fn tree_toggle(ed: &mut Editor, _ctx: Ctx) {
    crate::tree::toggle(ed);
}

/// `ret` in normal mode: opens the entry under the cursor on the welcome
/// page (the tree and git buffers have keys of their own), nothing elsewhere.
fn activate_line(ed: &mut Editor, _ctx: Ctx) {
    if ed.buffer().welcome.is_some() {
        crate::welcome::activate(ed);
    }
}

fn tree_activate(ed: &mut Editor, _ctx: Ctx) {
    crate::tree::activate(ed);
}

fn tree_fold(ed: &mut Editor, _ctx: Ctx) {
    crate::tree::toggle_directory(ed);
}

fn git_status(ed: &mut Editor, _ctx: Ctx) {
    crate::git::open_status(ed);
}

fn tree_parent(ed: &mut Editor, _ctx: Ctx) {
    crate::tree::go_to_parent(ed);
}

fn tree_cd(ed: &mut Editor, _ctx: Ctx) {
    crate::tree::cd_under_cursor(ed);
}

fn tree_hidden(ed: &mut Editor, _ctx: Ctx) {
    crate::tree::toggle_hidden(ed);
}

fn git_activate(ed: &mut Editor, _ctx: Ctx) {
    crate::git::activate(ed);
}

fn git_stage(ed: &mut Editor, _ctx: Ctx) {
    crate::git::stage(ed);
}

/// `esc` in the git buffer: close the preview split, else the usual escape.
fn git_close_preview(ed: &mut Editor, ctx: Ctx) {
    if !crate::git::close_file_view(ed) {
        normal_mode(ed, ctx);
    }
}

fn git_pull(ed: &mut Editor, _ctx: Ctx) {
    crate::git::pull(ed);
}

fn git_push(ed: &mut Editor, _ctx: Ctx) {
    crate::git::push(ed);
}

fn git_commit(ed: &mut Editor, _ctx: Ctx) {
    crate::git::open_commit(ed, false);
}

fn git_commit_amend(ed: &mut Editor, _ctx: Ctx) {
    crate::git::open_commit(ed, true);
}

/// `tab` in the git buffer: fold or unfold a file's diff.
fn toggle_fold(ed: &mut Editor, _ctx: Ctx) {
    crate::git::toggle_diff(ed);
}

fn zoom(ed: &mut Editor, delta: f32) {
    ed.config.font_size = (ed.config.font_size + delta).clamp(5.0, 72.0);
    ed.font_dirty = true;
    ed.set_message(format!("font size {}", ed.config.font_size));
}

fn zoom_in(ed: &mut Editor, _ctx: Ctx) {
    zoom(ed, 1.0);
}

fn zoom_out(ed: &mut Editor, _ctx: Ctx) {
    zoom(ed, -1.0);
}

// ---------------------------------------------------------------------------
// Table
// ---------------------------------------------------------------------------

macro_rules! commands {
    ($($name:ident: $doc:expr),* $(,)?) => {
        pub static COMMANDS: &[Command] = &[
            $(Command { name: stringify!($name), doc: $doc, run: $name }),*
        ];
    };
}

commands! {
    move_char_left: "Move left",
    move_char_right: "Move right",
    move_line_up: "Move up",
    move_line_down: "Move down",
    move_next_word_start: "Select to next word start",
    move_prev_word_start: "Select to previous word start",
    move_next_word_end: "Select to next word end",
    move_next_long_word_start: "Select to next WORD start",
    move_prev_long_word_start: "Select to previous WORD start",
    move_next_long_word_end: "Select to next WORD end",
    find_next_char: "Select to next occurrence of a character",
    find_till_char: "Select till next occurrence of a character",
    find_prev_char: "Select to previous occurrence of a character",
    find_till_prev_char: "Select till previous occurrence of a character",
    half_page_up: "Scroll half a page up",
    half_page_down: "Scroll half a page down",
    jump_backward: "Jump backward in the jump list",
    jump_forward: "Jump forward in the jump list",
    goto_file_start: "Go to file start (or line <count>)",
    goto_file_end: "Go to file end",
    goto_line_start: "Go to line start",
    goto_line_end: "Go to the last character of the line",
    goto_first_nonwhitespace: "Go to first non-blank character of the line",
    goto_next_buffer: "Go to the next buffer",
    goto_previous_buffer: "Go to the previous buffer",
    goto_last_modification: "Go to the last change",
    goto_file: "Open the file whose name is under the cursor",
    select_all: "Select the whole buffer",
    extend_line_below: "Select the current line, extending downwards",
    extend_line_above: "Select the current line, extending upwards",
    join_lines: "Join the line below onto this one (vim's J)",
    collapse_selection: "Collapse selections to cursors",
    flip_selections: "Swap anchor and cursor of each selection",
    keep_primary_selection: "Keep only the primary selection",
    copy_selection_on_next_line: "Add a cursor on the next line",
    copy_selection_on_prev_line: "Add a cursor on the previous line",
    select_regex: "Select every regex match inside the selections",
    select_mode: "Toggle select (extend) mode",
    normal_mode: "Back to normal mode",
    match_brackets: "Go to the matching bracket",
    select_textobject_inner: "Select inside a text object (w, p, ( [ { < \" ' `)",
    select_textobject_around: "Select around a text object",
    insert_mode: "Insert before the selection",
    append_mode: "Insert after the selection",
    insert_at_line_start: "Insert at the start of the line",
    insert_at_line_end: "Insert at the end of the line",
    open_below: "Open a new line below",
    open_above: "Open a new line above",
    delete_selection: "Delete the selection (to the clipboard)",
    delete_selection_noyank: "Delete the selection without yanking",
    change_selection: "Change the selection (delete and insert)",
    change_selection_noyank: "Change the selection without yanking",
    yank: "Yank the selection to the clipboard",
    paste: "Paste after the selection (whole lines below it); in select mode replace it, yanking what was there",
    paste_noyank: "Paste before the selection (whole lines above it); in select mode replace it",
    replace_char: "Replace the character under the cursor with a typed one",
    replace_mode: "Replace mode: typing overwrites",
    indent: "Indent the selected lines",
    unindent: "Unindent the selected lines",
    undo: "Undo",
    redo: "Redo",
    toggle_comments: "Comment or uncomment the selected lines",
    repeat_last_insert: "Repeat the last insertion",
    insert_newline: "Insert a newline (auto-indented)",
    insert_tab: "Insert an indent",
    delete_char_backward: "Delete the character before the cursor",
    delete_char_forward: "Delete the character under the cursor",
    search: "Search forward (regex)",
    rsearch: "Search backward (regex)",
    search_next: "Go to the next search match",
    search_prev: "Go to the previous search match",
    search_selection: "Use the selection as the search pattern",
    command_palette: "Open the command palette",
    file_picker: "Open a file (fuzzy search in the working directory, honours .gitignore)",
    file_picker_all: "Open a file, including ignored and hidden ones",
    buffer_picker: "Switch to an open buffer",
    global_search: "Search a regex in all files",
    window_next: "Focus the next frame",
    window_prev: "Focus the previous frame",
    vsplit: "Split the frame vertically (side by side)",
    hsplit: "Split the frame horizontally (stacked)",
    vsplit_new: "Vertical split with a new scratch buffer",
    hsplit_new: "Horizontal split with a new scratch buffer",
    window_close: "Close the frame",
    window_only: "Close every other frame",
    window_left: "Focus the frame to the left",
    window_right: "Focus the frame to the right",
    window_up: "Focus the frame above",
    window_down: "Focus the frame below",
    window_swap_left: "Swap with the frame to the left",
    window_swap_right: "Swap with the frame to the right",
    window_swap_up: "Swap with the frame above",
    window_swap_down: "Swap with the frame below",
    tab_new: "Open a new tab",
    tab_close: "Close the tab",
    tab_next: "Go to the next tab",
    tab_prev: "Go to the previous tab",
    resize_taller: "Make the frame taller",
    resize_shorter: "Make the frame shorter",
    resize_wider: "Make the frame wider",
    resize_narrower: "Make the frame narrower",
    resize_reset: "Give every frame of the tab its default size",
    tree_toggle: "Show or hide the file tree sidebar",
    activate_line: "On the welcome page: open the entry under the cursor",
    tree_activate: "Open the file, or fold or unfold the directory, under the cursor",
    tree_fold: "Fold or unfold the directory under the cursor",
    git_status: "Open the git status buffer in its own tab",
    toggle_fold: "Show or hide the diff of the file under the cursor",
    tree_cd: "Make the directory under the cursor (or the file's directory) the working directory",
    tree_parent: "Show the parent directory; the working directory stays",
    tree_hidden: "Show or hide dot-files and dot-directories",
    git_activate: "Open the file (at the hunk's line) in the preview split on the right",
    git_stage: "Stage the file or hunk under the cursor (unstage it in the staged section)",
    git_close_preview: "Close the preview split",
    git_pull: "Fetch and merge from the upstream (in the background)",
    git_push: "Push the branch (sets origin as upstream the first time)",
    git_commit: "Commit the staged changes: write the message, then :write",
    git_commit_amend: "Amend the last commit: edit its message, then :write",
    zoom_in: "Bigger font",
    zoom_out: "Smaller font",
}
