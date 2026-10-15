//! Drawing the whole editor: tab bar, frames (text and terminals), status
//! line, prompt and picker overlays.

use crate::buffer::Buffer;
use crate::editor::{Editor, MessageKind, Mode, Overlay, Preview, StyleTable};
use crate::frame::{View, ViewId};
use crate::movement;
use crate::picker::{ItemData, PickerItem, PickerKind};
use crate::render::font::FontSet;
use crate::render::layout::{Cell, LineLayout, layout_line};
use crate::render::{Color, Painter, Rect};
use crate::syntax::highlight::{LineState, Span};
use crate::syntax::{Registry, Rule};
use crate::terminal::{Cell as TermCell, TermColor};
use crate::theme::Style;

/// Vertical padding inside the status line and tab bar.
const BAR_PAD: i32 = 3;

/// Everything needed to draw text consistently.
pub struct Ui<'a> {
    pub painter: Painter<'a>,
    pub font: &'a mut FontSet,
}

impl<'a> Ui<'a> {
    fn bar_height(&self) -> i32 {
        self.font.cell_h as i32 + 2 * BAR_PAD
    }

    /// Draw a UI string at a pixel position; returns the width drawn.
    fn text(&mut self, x: i32, y: i32, text: &str, color: Color, bold: bool) -> i32 {
        let mut pen = x as f32;
        let baseline = y as f32 + self.font.ascent;
        for ch in text.chars() {
            let cells = FontSet::char_cells(ch).max(1);
            if ch != ' ' {
                self.glyph(pen, baseline, ch, color, bold, false, 1.0);
            }
            pen += self.font.cell_w * cells as f32;
        }
        (pen - x as f32) as i32
    }

    #[allow(clippy::too_many_arguments)]
    fn glyph(&mut self, x: f32, baseline: f32, ch: char, color: Color, bold: bool, italic: bool, scale: f32) {
        let glyph = self.font.glyph(ch, bold, italic, scale);
        let gx = x.round() as i32 + glyph.xmin;
        let gy = baseline.round() as i32 - glyph.ymin - glyph.height as i32;
        self.painter.blit_mask(gx, gy, glyph.width, glyph.height, &glyph.mask, color);
    }

    /// Draw a laid-out cell: its ligature glyph when it starts one, nothing
    /// when a ligature covers it, its character otherwise.
    fn cell_glyph(&mut self, x: f32, baseline: f32, cell: &Cell, color: Color) {
        if cell.ligated {
            return;
        }
        let style = cell.style;
        match cell.glyph {
            Some(id) => {
                let glyph = self.font.glyph_by_id(id, style.bold, style.italic, style.scale);
                let gx = x.round() as i32 + glyph.xmin;
                let gy = baseline.round() as i32 - glyph.ymin - glyph.height as i32;
                self.painter.blit_mask(gx, gy, glyph.width, glyph.height, &glyph.mask, color);
            }
            None if cell.ch != ' ' && cell.ch != '\t' => self.glyph(x, baseline, cell.ch, color, style.bold, style.italic, style.scale),
            None => {}
        }
    }

    /// Width in pixels of a UI string.
    fn text_width(&self, text: &str) -> i32 {
        (text.chars().map(|c| FontSet::char_cells(c).max(1)).sum::<usize>() as f32 * self.font.cell_w) as i32
    }
}

/// Draw the editor into the painter. Also records frame rectangles and view
/// sizes on the editor, which commands need.
pub fn draw(ed: &mut Editor, ui: &mut Ui) {
    let theme_bg = ed.theme.ui("ui.background", Color::BLACK);
    ui.painter.reset_clip();
    ui.painter.fill(theme_bg);

    let width = ui.painter.width as i32;
    let height = ui.painter.height as i32;
    let bar = ui.bar_height();
    let mut top = 0;
    if ed.tabs.len() > 1 {
        draw_tab_bar(ed, ui, Rect::new(0, 0, width, bar));
        top = bar;
    }
    let frames_rect = Rect::new(0, top, width, (height - bar - top).max(0));
    let mut rects = Vec::new();
    ed.root().layout(frames_rect, &mut rects);
    ed.frame_rects = rects.clone();

    let separator = ed.theme.ui("ui.separator", Color::BLACK);
    let focused = ed.focused_view();
    for (view_id, rect) in rects {
        draw_view(ed, ui, view_id, rect, view_id == focused);
        // Separator lines on the right and bottom edges, inside the frames area.
        if rect.right() < frames_rect.right() {
            ui.painter.reset_clip();
            ui.painter.fill_rect(Rect::new(rect.right(), rect.y, crate::frame::SEPARATOR, rect.h), separator);
        }
        if rect.bottom() < frames_rect.bottom() {
            ui.painter.reset_clip();
            ui.painter.fill_rect(Rect::new(rect.x, rect.bottom(), rect.w, crate::frame::SEPARATOR), separator);
        }
    }
    ui.painter.reset_clip();
    // The floating frame (commit message) sits over the middle of the window.
    if let Some(float) = ed.float {
        let cell_w = ui.font.cell_w as i32;
        let cell_h = ui.font.cell_h as i32;
        let w = (frames_rect.w * 7 / 10).max(cell_w * 40).min(frames_rect.w);
        let h = (frames_rect.h / 2).max(cell_h * 8).min(frames_rect.h);
        let rect = Rect::new(frames_rect.x + (frames_rect.w - w) / 2, frames_rect.y + (frames_rect.h - h) / 2, w, h);
        let border = ed.theme.ui("ui.popup.border", Color::BLACK);
        ui.painter.fill_rect(Rect::new(rect.x - 3, rect.y - 3, rect.w + 6, rect.h + 6), ed.theme.ui("ui.prompt", border));
        ui.painter.fill_rect(rect, theme_bg);
        draw_view(ed, ui, float, rect, true);
        ui.painter.reset_clip();
        ed.frame_rects.insert(0, (float, rect));
    }
    draw_status_line(ed, ui, Rect::new(0, height - bar, width, bar));
    if let Overlay::Picker(_) = ed.overlay {
        draw_picker(ed, ui, frames_rect);
    }
}

fn draw_tab_bar(ed: &mut Editor, ui: &mut Ui, rect: Rect) {
    let bg = ed.theme.ui("ui.tabbar", Color::BLACK);
    let inactive = ed.theme.ui("ui.tab", Color::WHITE);
    let active = ed.theme.ui("ui.tab.active", Color::WHITE);
    let active_bg = ed.theme.ui("ui.tab.active.bg", bg);
    ui.painter.fill_rect(rect, bg);
    let mut x = rect.x;
    let mut tab_rects = Vec::new();
    for (i, tab) in ed.tabs.iter().enumerate() {
        let buffer = &ed.buffers[&ed.views[&tab.focus].buffer];
        let modified = if buffer.is_modified() { " +" } else { "" };
        let label = format!(" {}: {}{} ", i + 1, buffer.display_name(), modified);
        let w = ui.text_width(&label);
        let tab_rect = Rect::new(x, rect.y, w, rect.h);
        if i == ed.tab_index {
            ui.painter.fill_rect(tab_rect, active_bg);
        }
        ui.text(x, rect.y + BAR_PAD, &label, if i == ed.tab_index { active } else { inactive }, i == ed.tab_index);
        tab_rects.push(tab_rect);
        x += w;
    }
    ed.tab_rects = tab_rects;
}

/// Width of the line-number gutter in pixels.
fn gutter_width(ed: &Editor, buffer: &Buffer, cell_w: f32) -> i32 {
    if buffer.is_terminal() || buffer.tree {
        return cell_w as i32;
    }
    if !ed.config.line_numbers || buffer.welcome.is_some() {
        return (cell_w * 2.0) as i32;
    }
    let digits = buffer.line_count().max(1).to_string().len().max(3);
    (cell_w * (digits + 2) as f32) as i32
}

fn draw_view(ed: &mut Editor, ui: &mut Ui, view_id: ViewId, rect: Rect, focused: bool) {
    let buffer_id = ed.views[&view_id].buffer;
    let is_terminal = ed.buffers[&buffer_id].is_terminal();
    let show_numbers = ed.config.line_numbers && ed.buffers[&buffer_id].welcome.is_none() && !ed.buffers[&buffer_id].tree;
    let cell_w = ui.font.cell_w;
    let cell_h = ui.font.cell_h;
    let gutter = gutter_width(ed, &ed.buffers[&buffer_id], cell_w);
    // The welcome page carries the logo above its text.
    let offset = welcome_offset(&ed.buffers[&buffer_id]);
    if offset > 0 {
        ui.painter.set_clip(rect);
        let size = crate::welcome::LOGO_SIZE;
        ui.painter.blit_rgba(rect.x + gutter, rect.y + 12, size, size, crate::welcome::LOGO_RGBA);
    }
    let rect = Rect::new(rect.x, rect.y + offset, rect.w, (rect.h - offset).max(0));
    let text_rect = Rect::new(rect.x + gutter, rect.y, (rect.w - gutter).max(0), rect.h);
    // Soft wrap: lines fold into visual rows that fit the frame.
    let wrap = if ed.config.wrap { Some(text_rect_width(rect, gutter)) } else { None };

    {
        let view = ed.views.get_mut(&view_id).expect("view");
        view.rows = (rect.h as f32 / cell_h).floor().max(1.0) as usize;
        view.cols = (text_rect.w as f32 / cell_w).floor().max(1.0) as usize;
    }

    if is_terminal {
        draw_terminal_view(ed, ui, view_id, rect, text_rect, focused);
        return;
    }

    let text_color = ed.theme.ui("ui.text", Color::WHITE);
    let base = Style::plain(text_color);
    let tab_width = ed.config.tab_width;
    let cursorline_color = ed.theme.ui("ui.cursorline", Color::BLACK);
    let selection_color = ed.theme.ui("ui.selection", Color::BLACK);
    let primary_color = ed.theme.ui("ui.selection.primary", selection_color);
    let bracket_color = ed.theme.ui("ui.match.bracket", selection_color);
    let linenr = ed.theme.ui("ui.linenr", Color::WHITE);
    let linenr_current = ed.theme.ui("ui.linenr.current", Color::WHITE);
    let cursor_color =
        if ed.mode.is_editing() { ed.theme.ui("ui.cursor.insert", Color::WHITE) } else { ed.theme.ui("ui.cursor", Color::WHITE) };
    let secondary_cursor = ed.theme.ui("ui.cursor.secondary", cursor_color);
    let inactive_cursor = ed.theme.ui("ui.cursor.inactive", cursor_color);

    // Lay out visible lines; scroll down if the cursor line does not fit.
    let mut layouts = loop {
        let first = ed.views[&view_id].scroll_line;
        let layouts = layout_visible_lines(ed, ui.font, view_id, first, text_rect.h as f32, base, tab_width, wrap);
        let view = &ed.views[&view_id];
        let cursor_line = ed.buffers[&buffer_id].line_of(view.cursor());
        let shown = layouts.iter().filter(|l| l.y + l.layout.height <= text_rect.h as f32).count();
        if focused && cursor_line >= view.scroll_line + shown.max(1) && view.scroll_line < cursor_line {
            ed.views.get_mut(&view_id).expect("view").scroll_line += 1;
            continue;
        }
        break layouts;
    };
    // While a scroll slides, draw from where the view appears to be instead.
    let (first_line, slide) = animated_scroll(ed, view_id, cell_h);
    if first_line != ed.views[&view_id].scroll_line || slide != 0 {
        layouts = layout_visible_lines(ed, ui.font, view_id, first_line, text_rect.h as f32 + cell_h, base, tab_width, wrap);
    }
    // Git: hunk headers in the status buffer stand out; the preview split
    // colours added, modified and deleted lines of the file against HEAD.
    let theme_bg = ed.theme.ui("ui.background", Color::BLACK);
    let marks = crate::git::marks_for(ed, view_id);
    if ed.buffers[&buffer_id].git {
        let hunk_bg = theme_bg.mix(ed.theme.color("diff.hunk"), 0.3);
        for visible in &mut layouts {
            if ed.buffers[&buffer_id].line_text(visible.line).starts_with("    @@") {
                visible.background = Some(hunk_bg);
            }
        }
    }
    let (add_color, modified_color, remove_color) =
        (ed.theme.color("diff.add"), ed.theme.color("diff.modified"), ed.theme.color("diff.remove"));
    if let Some(marks) = &marks {
        for visible in &mut layouts {
            if let Some(change) = marks.lines.get(&visible.line) {
                visible.background = Some(theme_bg.mix(if change.added { add_color } else { modified_color }, 0.2));
            }
        }
    }
    // The file tree colours changed and new files by their git status.
    if ed.buffers[&buffer_id].tree && !ed.buffers[&buffer_id].is_modified() {
        let modified = ed.theme.color("tree.modified");
        let new = ed.theme.color("tree.new");
        for visible in &mut layouts {
            let status = ed.tree.as_ref().and_then(|t| t.entries.get(visible.line)).and_then(|e| ed.git.state.as_ref()?.status(&e.path));
            let color = match status {
                Some(crate::git::FileStatus::Modified) => modified,
                Some(crate::git::FileStatus::New) => new,
                None => continue,
            };
            for cell in &mut visible.layout.cells {
                cell.style.fg = color;
            }
        }
    }

    let view = &ed.views[&view_id];
    let buffer = &ed.buffers[&buffer_id];
    let cursor = view.cursor().min(buffer.len_chars());
    let cursor_line = buffer.line_of(cursor);
    let bracket_match = if focused && cursor < buffer.len_chars() && "()[]{}<>".contains(buffer.text.char(cursor)) {
        movement::matching_bracket(&buffer.text, cursor)
    } else {
        None
    };
    let scroll_x = view.scroll_col as f32 * cell_w;
    let mode = ed.mode;
    let selection = view.selection.clone();

    for visible in &layouts {
        let (line, line_start, layout) = (&visible.line, &visible.start, &visible.layout);
        let y_px = text_rect.y + visible.y as i32 - slide;
        let line_rect = Rect::new(rect.x, y_px, rect.w, layout.height as i32);
        if line_rect.y >= rect.bottom() {
            break;
        }
        ui.painter.set_clip(rect);
        if ed.config.cursorline && focused && *line == cursor_line && mode != Mode::Select {
            ui.painter.fill_rect(line_rect, cursorline_color);
        }
        // Gutter.
        if show_numbers {
            let number = if ed.config.relative_line_numbers && *line != cursor_line {
                (*line as isize - cursor_line as isize).unsigned_abs()
            } else {
                line + 1
            };
            let label = number.to_string();
            let color = if *line == cursor_line { linenr_current } else { linenr };
            let x = rect.x + gutter - ui.text_width(&label) - cell_w as i32;
            let baseline_offset = (layout.row_height - cell_h).max(0.0) as i32;
            ui.text(x, y_px + baseline_offset, &label, color, false);
        }

        ui.painter.set_clip(text_rect);
        let origin_x = text_rect.x as f32 - scroll_x;
        let line_len = layout.cells.last().map(|c| c.char_index + 1).unwrap_or(0);

        // Lines inside a block with a background (fenced code) get it across the frame.
        if let Some(bg) = visible.background {
            ui.painter.fill_rect(Rect::new(text_rect.x, y_px, text_rect.w, layout.height as i32), bg);
        }

        // A range of characters filled on every visual row it touches.
        let row_h = layout.row_height as i32;
        let fill_chars = |ui: &mut Ui, from: usize, to: usize, color: Color| {
            for (row, x0, x1) in layout.row_segments(from, to, cell_w) {
                let x = (origin_x + x0) as i32;
                ui.painter.fill_rect(Rect::new(x, y_px + row as i32 * row_h, (x1 - x0).max(0.0) as i32, row_h), color);
            }
        };

        // Git preview: the exact changed span of a modified line, and a red
        // marker where lines were deleted.
        if let Some(marks) = &marks {
            if let Some(change) = marks.lines.get(line) {
                for (from, to) in &change.spans {
                    fill_chars(ui, *from, *to, theme_bg.mix(modified_color, 0.55));
                }
            }
            if marks.deleted_before.contains(line) {
                ui.painter.fill_rect(Rect::new(text_rect.x, y_px, text_rect.w, 2), remove_color);
            }
        }

        // Selection backgrounds.
        for (i, range) in selection.iter().enumerate() {
            let (from, to) = range.span(&buffer.text);
            if to <= *line_start || from > line_start + line_len {
                continue;
            }
            let color = if i == selection.primary_index() { primary_color } else { selection_color };
            let is_cursor_only = from + 1 >= to;
            if is_cursor_only {
                continue;
            }
            let start = from.saturating_sub(*line_start);
            let end = (to - line_start).min(line_len + 1);
            fill_chars(ui, start, end, color);
        }

        // Text, row by row.
        let mut row = 0;
        for cell in &layout.cells {
            if cell.hidden {
                continue;
            }
            while row + 1 < layout.rows.len() && cell.char_index >= layout.rows[row + 1].0 {
                row += 1;
            }
            let x = origin_x + cell.x - layout.rows[row].1;
            if x + cell.width < text_rect.x as f32 || x > text_rect.right() as f32 {
                continue;
            }
            let y_row = y_px + row as i32 * row_h;
            let baseline = y_row as f32 + layout.ascent;
            if let Some(bg) = cell.style.bg {
                ui.painter.fill_rect(Rect::new(x as i32, y_row, cell.width.ceil() as i32, row_h), bg);
            }
            if Some(line_start + cell.char_index) == bracket_match {
                ui.painter.fill_rect(Rect::new(x as i32, y_row, cell.width as i32, row_h), bracket_color);
            }
            ui.cell_glyph(x, baseline, cell, cell.style.fg);
            if cell.style.underline {
                ui.painter.hline(x as i32, baseline as i32 + 2, cell.width as i32, cell.style.fg);
            }
            if cell.style.strike {
                ui.painter.hline(x as i32, (baseline - layout.ascent * 0.35) as i32, cell.width as i32, cell.style.fg);
            }
        }

        // Cursors.
        for (i, range) in selection.iter().enumerate() {
            let head = range.head.min(buffer.len_chars());
            if head < *line_start || head > line_start + line_len {
                continue;
            }
            // The end-of-text cursor belongs to the last line only.
            if head == line_start + line_len && head < buffer.len_chars() && buffer.text.char(head) != '\n' {
                continue;
            }
            let col = head - line_start;
            let (cursor_row, x_in_row) = layout.pos_of(col);
            let mut x = origin_x + x_in_row;
            let mut y = y_px + cursor_row as i32 * row_h;
            let color = if !focused {
                inactive_cursor
            } else if i == selection.primary_index() {
                cursor_color
            } else {
                secondary_cursor
            };
            // The primary cursor of the focused frame slides to its new place.
            let mut arrived = true;
            if focused && i == selection.primary_index() {
                let ms = ed.config.cursor_animation;
                let view = ed.views.get_mut(&view_id).expect("view");
                let target = (x - rect.x as f32, (y - rect.y) as f32);
                let shown = view.cursor_position(target, ms);
                arrived = view.cursor_anim.is_none();
                x = rect.x as f32 + shown.0;
                y = rect.y + shown.1.round() as i32;
            }
            if mode == Mode::Insert && focused {
                ui.painter.fill_rect(Rect::new(x as i32, y, 2, row_h), color);
            } else {
                let w = layout.width_of(col, cell_w);
                let cursor_rect = Rect::new(x as i32, y, w as i32, row_h);
                if focused {
                    ui.painter.fill_rect(cursor_rect, color);
                    // Redraw the character in the background color so it stays readable.
                    if arrived && let Some(cell) = layout.cells.iter().find(|c| c.char_index == col && !c.hidden) {
                        let fg = if color.is_dark() { Color::WHITE } else { ed.theme.ui("ui.background", Color::BLACK) };
                        ui.cell_glyph(x, y as f32 + layout.ascent, cell, fg);
                    }
                } else {
                    ui.painter.rect_outline(cursor_rect, color);
                }
            }
        }
    }
    ui.painter.reset_clip();
}

/// Width left for text once the gutter is taken off a frame.
fn text_rect_width(rect: Rect, gutter: i32) -> f32 {
    (rect.w - gutter).max(0) as f32
}

/// Pixels the welcome page's text sits below its logo.
fn welcome_offset(buffer: &Buffer) -> i32 {
    if buffer.welcome.is_some() { crate::welcome::LOGO_SIZE as i32 + 24 } else { 0 }
}

/// One line of a frame, laid out and ready to draw.
struct VisibleLine {
    line: usize,
    /// Character index where the line starts in the buffer.
    start: usize,
    /// Vertical offset from the top of the frame.
    y: f32,
    layout: LineLayout,
    /// Background of the whole line when it sits inside a block that has one
    /// (fenced code in Markdown).
    background: Option<Color>,
}

/// Background color of the innermost open region that has one.
fn region_background(registry: &Registry, styles: &StyleTable, state: &LineState) -> Option<Color> {
    state.stack.iter().rev().find_map(|frame| match &registry.get(frame.lang).rules[frame.rule] {
        Rule::Region { scope, .. } => styles.styles[scope.0 as usize].bg,
        Rule::Match { .. } => None,
    })
}

/// Lay out the lines visible in a view.
/// The first line to draw and how many pixels the lines are shifted up, for
/// a view that is sliding to its scroll position.
fn animated_scroll(ed: &mut Editor, view_id: ViewId, cell_h: f32) -> (usize, i32) {
    let ms = ed.config.scroll_animation;
    let view = ed.views.get_mut(&view_id).expect("view");
    let shown = view.displayed_scroll(ms);
    let first = shown.floor() as usize;
    (first, ((shown - first as f32) * cell_h).round() as i32)
}

#[allow(clippy::too_many_arguments)]
fn layout_visible_lines(
    ed: &mut Editor,
    font: &FontSet,
    view_id: ViewId,
    scroll_line: usize,
    height: f32,
    base: Style,
    tab_width: usize,
    wrap: Option<f32>,
) -> Vec<VisibleLine> {
    let view = &ed.views[&view_id];
    let buffer_id = view.buffer;
    let cursor_line = ed.buffers[&buffer_id].line_of(view.cursor());
    let is_focused = ed.focused_view() == view_id;
    let registry = &ed.registry;
    let highlighter = &mut ed.highlighter;
    let styles = &ed.styles;
    let buffer = ed.buffers.get_mut(&buffer_id).expect("buffer");
    let line_count = buffer.line_count();

    let mut out = Vec::new();
    let mut y = 0.0;
    let mut spans: Vec<Span> = Vec::new();
    let mut state = None;
    let mut line = scroll_line;
    while line < line_count && y < height {
        let text = buffer.line_text(line);
        spans.clear();
        let mut background = None;
        if let Some(lang) = buffer.language {
            let state = state.get_or_insert_with(|| {
                buffer.highlight.ensure(line.saturating_sub(1), &buffer.text, lang, registry, highlighter);
                buffer.highlight.state_before(line)
            });
            // A block's background covers the lines inside it and both fence lines.
            let before = region_background(registry, styles, state);
            highlighter.highlight_line(registry, lang, &text, state, &mut spans);
            background = before.or_else(|| region_background(registry, styles, state));
        }
        let reveal = is_focused && line == cursor_line;
        let layout = layout_line(&text, &spans, styles, base, font, tab_width, reveal, wrap);
        let start = buffer.line_start(line);
        let h = layout.height;
        out.push(VisibleLine { line, start, y, layout, background });
        y += h;
        line += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Terminal frames
// ---------------------------------------------------------------------------

fn term_color(ed: &Editor, color: TermColor, bold: bool, foreground: bool) -> Color {
    const NAMES: [&str; 16] = [
        "term.black",
        "term.red",
        "term.green",
        "term.yellow",
        "term.blue",
        "term.magenta",
        "term.cyan",
        "term.white",
        "term.bright_black",
        "term.bright_red",
        "term.bright_green",
        "term.bright_yellow",
        "term.bright_blue",
        "term.bright_magenta",
        "term.bright_cyan",
        "term.bright_white",
    ];
    match color {
        TermColor::Default => {
            if foreground {
                ed.theme.ui("ui.text", Color::WHITE)
            } else {
                ed.theme.ui("ui.background", Color::BLACK)
            }
        }
        TermColor::Index(i) if i < 16 => {
            let i = if bold && foreground && i < 8 { i + 8 } else { i };
            ed.theme.ui(NAMES[i as usize], Color::WHITE)
        }
        TermColor::Index(i) if i < 232 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color::rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        TermColor::Index(i) => {
            let v = 8 + (i - 232) * 10;
            Color::rgb(v, v, v)
        }
        TermColor::Rgb(r, g, b) => Color::rgb(r, g, b),
    }
}

fn draw_terminal_view(ed: &mut Editor, ui: &mut Ui, view_id: ViewId, rect: Rect, text_rect: Rect, focused: bool) {
    let cell_w = ui.font.cell_w;
    let cell_h = ui.font.cell_h;
    let rows = ed.views[&view_id].rows;
    let cols = ed.views[&view_id].cols;
    let buffer_id = ed.views[&view_id].buffer;
    let terminal_mode = focused && ed.mode == Mode::Terminal;

    {
        let buffer = ed.buffers.get_mut(&buffer_id).expect("buffer");
        let terminal = buffer.terminal.as_mut().expect("terminal buffer");
        terminal.resize(cols as u16, rows as u16);
        let total = terminal.total_rows();
        let view = ed.views.get_mut(&view_id).expect("view");
        if terminal_mode && !view.scrolled_back {
            // Follow the live screen.
            view.scroll_line = total.saturating_sub(rows);
        }
        view.scroll_line = view.scroll_line.min(total.saturating_sub(1));
        view.scroll_col = 0;
    }

    let view: &View = &ed.views[&view_id];
    let buffer = &ed.buffers[&buffer_id];
    let terminal = buffer.terminal.as_ref().expect("terminal buffer");
    let selection = view.selection.clone();
    let selection_color = ed.theme.ui("ui.selection", Color::BLACK);
    let primary_color = ed.theme.ui("ui.selection.primary", selection_color);
    let cursor_color = ed.theme.ui("ui.cursor", Color::WHITE);
    let inactive_cursor = ed.theme.ui("ui.cursor.inactive", cursor_color);
    let background = ed.theme.ui("ui.background", Color::BLACK);

    ui.painter.set_clip(rect);
    let total = terminal.total_rows();
    let mut y = text_rect.y;
    let mut row = view.scroll_line;
    while row < total && y < rect.bottom() {
        let cells: &[TermCell] = terminal.row(row);
        // Char index (in the text mirror) of each cell, for selections in normal mode.
        let row_start = if row < buffer.line_count() { buffer.line_start(row) } else { usize::MAX };
        // Trailing blank cells have no character in the mirror (rows are trimmed).
        let row_len = if row < buffer.line_count() { buffer.line_end(row) - buffer.line_start(row) } else { 0 };
        let mut char_index = 0usize;
        let mut x = text_rect.x as f32;
        for (col, cell) in cells.iter().enumerate() {
            if col >= cols {
                break;
            }
            let width = if cell.wide { cell_w * 2.0 } else { cell_w };
            if cell.spacer {
                continue;
            }
            let (mut fg, mut bg) = (term_color(ed, cell.fg, cell.bold, true), term_color(ed, cell.bg, false, false));
            if cell.inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            let pos = row_start.saturating_add(char_index);
            if !terminal_mode && row_start != usize::MAX && char_index <= row_len {
                for (i, range) in selection.iter().enumerate() {
                    let (from, to) = range.span(&buffer.text);
                    if from < to && from <= pos && pos < to && from + 1 < to {
                        bg = if i == selection.primary_index() { primary_color } else { selection_color };
                    }
                }
            }
            if bg != background {
                ui.painter.fill_rect(Rect::new(x as i32, y, width.ceil() as i32, cell_h as i32), bg);
            }
            if cell.ch != ' ' {
                ui.glyph(x, y as f32 + ui.font.ascent, cell.ch, fg, cell.bold, cell.italic, 1.0);
            }
            if cell.underline {
                ui.painter.hline(x as i32, y + ui.font.ascent as i32 + 2, width as i32, fg);
            }
            x += width;
            char_index += 1;
        }
        y += cell_h as i32;
        row += 1;
    }

    // Cursor: the terminal's own in terminal mode, the view's selection otherwise.
    if terminal_mode {
        if terminal.cursor_visible() {
            let (crow, ccol) = terminal.cursor();
            if crow >= view.scroll_line {
                let x = text_rect.x + (ccol as f32 * cell_w) as i32;
                let y = text_rect.y + ((crow - view.scroll_line) as f32 * cell_h) as i32;
                ui.painter.fill_rect(Rect::new(x, y, cell_w as i32, cell_h as i32), cursor_color);
                if let Some(cell) = terminal.row(crow).get(ccol)
                    && cell.ch != ' '
                {
                    ui.glyph(x as f32, y as f32 + ui.font.ascent, cell.ch, background, cell.bold, false, 1.0);
                }
            }
        }
    } else {
        for (i, range) in selection.iter().enumerate() {
            let head = range.head.min(buffer.len_chars());
            let line = buffer.line_of(head);
            if line < view.scroll_line {
                continue;
            }
            let col = head - buffer.line_start(line);
            let x = text_rect.x + (col as f32 * cell_w) as i32;
            let y = text_rect.y + ((line - view.scroll_line) as f32 * cell_h) as i32;
            let cursor_rect = Rect::new(x, y, cell_w as i32, cell_h as i32);
            let color = if !focused {
                inactive_cursor
            } else if i == selection.primary_index() {
                cursor_color
            } else {
                ed.theme.ui("ui.cursor.secondary", cursor_color)
            };
            if focused {
                ui.painter.fill_rect(cursor_rect, color);
                let ch = buffer.text.get_char(head).unwrap_or(' ');
                if ch != ' ' && ch != '\n' {
                    ui.glyph(x as f32, y as f32 + ui.font.ascent, ch, background, false, false, 1.0);
                }
            } else {
                ui.painter.rect_outline(cursor_rect, color);
            }
        }
    }
    ui.painter.reset_clip();
}

// ---------------------------------------------------------------------------
// Status line, prompt, picker
// ---------------------------------------------------------------------------

fn draw_status_line(ed: &Editor, ui: &mut Ui, rect: Rect) {
    let bg = ed.theme.ui("ui.statusline", Color::BLACK);
    let fg = ed.theme.ui("ui.statusline.text", Color::WHITE);
    let dim = ed.theme.ui("ui.dim", fg);
    ui.painter.fill_rect(rect, bg);
    let y = rect.y + BAR_PAD;
    let cell_w = ui.font.cell_w as i32;

    if let Overlay::Prompt(prompt) = &ed.overlay {
        let prompt_color = ed.theme.ui("ui.prompt", fg);
        let mut x = rect.x + cell_w;
        x += ui.text(x, y, &prompt.label, prompt_color, true) + cell_w;
        let text_x = x;
        ui.text(x, y, &prompt.text, fg, false);
        let before: String = prompt.text.chars().take(prompt.cursor).collect();
        let cursor_x = text_x + ui.text_width(&before);
        ui.painter.fill_rect(Rect::new(cursor_x, y, 2, ui.font.cell_h as i32), ed.theme.ui("ui.cursor.insert", fg));
        return;
    }

    // Mode badge.
    let mode_scope = match ed.mode {
        Mode::Normal => "ui.mode.normal",
        Mode::Insert => "ui.mode.insert",
        Mode::Replace => "ui.mode.replace",
        Mode::Select => "ui.mode.select",
        Mode::Terminal => "ui.mode.terminal",
    };
    let badge_bg = ed.theme.ui(mode_scope, fg);
    let badge_fg = ed.theme.ui("ui.statusline.mode", bg);
    let badge = format!(" {} ", ed.mode.name());
    let badge_w = ui.text_width(&badge);
    ui.painter.fill_rect(Rect::new(rect.x, rect.y, badge_w, rect.h), badge_bg);
    ui.text(rect.x, y, &badge, badge_fg, true);
    let mut x = rect.x + badge_w;
    // Git badge: the branch, starred when the tree has changes.
    if let Some(git) = &ed.git.state {
        let label = format!(" {} ", git.label());
        let w = ui.text_width(&label);
        ui.painter.fill_rect(Rect::new(x, rect.y, w, rect.h), ed.theme.ui("ui.git", badge_bg));
        ui.text(x, y, &label, badge_fg, true);
        x += w;
        // Commits ahead and behind the upstream; a spinner while pulling or pushing.
        let sync = match (&ed.git.remote, &git.sync) {
            (Some(op), _) => Some(op.indicator()),
            (None, Some(sync)) => sync.label(),
            (None, None) => None,
        };
        if let Some(text) = sync {
            let label = format!(" {text} ");
            let w = ui.text_width(&label);
            ui.painter.fill_rect(Rect::new(x, rect.y, w, rect.h), ed.theme.ui("ui.git.sync", badge_bg));
            ui.text(x, y, &label, badge_fg, true);
            x += w;
        }
    }
    x += cell_w;

    // Right side: pending keys, selections, position, language.
    let view = ed.view();
    let buffer = ed.buffer();
    let cursor = view.cursor().min(buffer.len_chars());
    let line = buffer.line_of(cursor);
    let col = movement::column_of(&buffer.text, cursor, ed.config.tab_width);
    let mut right = Vec::new();
    if !ed.pending_keys.is_empty() {
        right.push(ed.pending_keys.iter().map(|k| k.to_string()).collect::<Vec<_>>().join(" "));
    }
    if let Some(count) = ed.count {
        right.push(count.to_string());
    }
    if view.selection.len() > 1 {
        right.push(format!("{} sels", view.selection.len()));
    }
    right.push(format!("{}:{}", line + 1, col + 1));
    if let Some(lang) = buffer.language {
        right.push(ed.registry.get(lang).name.clone());
    }
    let text = right.join("  ");
    let w = ui.text_width(&text);
    let right_x = rect.right() - w - cell_w;
    ui.text(right_x, y, &text, dim, false);

    // Left side: buffer name and message, clipped before the right side.
    ui.painter.set_clip(Rect::new(rect.x, rect.y, (right_x - cell_w - rect.x).max(0), rect.h));
    let mut name = buffer.display_name();
    if buffer.is_modified() {
        name.push_str(" [+]");
    }
    if (buffer.readonly || view.readonly) && !buffer.is_terminal() {
        name.push_str(" [ro]");
    }
    x += ui.text(x, y, &name, fg, false) + 2 * cell_w;
    if let Some(message) = &ed.message {
        let color = match message.kind {
            MessageKind::Info => fg,
            MessageKind::Error => ed.theme.ui("ui.error", fg),
        };
        ui.text(x, y, &message.text, color, false);
    }
    ui.painter.reset_clip();
}

fn draw_picker(ed: &mut Editor, ui: &mut Ui, area: Rect) {
    let Overlay::Picker(mut picker) = std::mem::replace(&mut ed.overlay, Overlay::None) else { return };
    let cell_w = ui.font.cell_w as i32;
    let row_h = ui.font.cell_h as i32 + 2;
    // File-like pickers show a preview of the highlighted item on the right.
    let has_preview = matches!(picker.kind, PickerKind::Files | PickerKind::Buffers | PickerKind::Locations);
    let width = if has_preview { area.w * 9 / 10 } else { (area.w * 4 / 5).min(cell_w * 110).max(cell_w * 20) };
    let max_rows = ((area.h * 3 / 5) / row_h).max(3) as usize;
    let rows = if has_preview { max_rows } else { picker.matches.len().clamp(1, max_rows) };
    let height = row_h * (rows as i32 + 1) + 2 * BAR_PAD + 2;
    let x = area.x + (area.w - width) / 2;
    let y = area.y + area.h / 8;
    let rect = Rect::new(x, y, width, height);
    let list_w = if has_preview { width * 2 / 5 } else { width };

    let bg = ed.theme.ui("ui.popup", Color::BLACK);
    let border = ed.theme.ui("ui.popup.border", Color::BLACK);
    let fg = ed.theme.ui("ui.text", Color::WHITE);
    let dim = ed.theme.ui("ui.dim", fg);
    let selected_bg = ed.theme.ui("ui.popup.selected", bg);
    let match_color = ed.theme.ui("ui.popup.match", fg);
    let prompt_color = ed.theme.ui("ui.prompt", fg);

    ui.painter.reset_clip();
    ui.painter.fill_rect(rect, bg);
    ui.painter.rect_outline(rect, border);
    ui.painter.set_clip(rect.inset(1));

    // Query line.
    let mut px = rect.x + cell_w;
    let py = rect.y + BAR_PAD + 1;
    let title = format!("{}:", picker.title);
    px += ui.text(px, py, &title, prompt_color, true) + cell_w;
    px += ui.text(px, py, &picker.query, fg, false);
    ui.painter.fill_rect(Rect::new(px, py, 2, ui.font.cell_h as i32), ed.theme.ui("ui.cursor.insert", fg));
    let count = format!("{}/{}", picker.matches.len(), picker.items.len());
    let cw = ui.text_width(&count);
    ui.text(rect.right() - cw - cell_w, py, &count, dim, false);
    ui.painter.hline(rect.x + 1, py + row_h, rect.w - 2, border);

    // Keep the selected row visible.
    if picker.selected < picker.scroll {
        picker.scroll = picker.selected;
    } else if picker.selected >= picker.scroll + rows {
        picker.scroll = picker.selected + 1 - rows;
    }
    picker.scroll = picker.scroll.min(picker.matches.len().saturating_sub(rows));

    let label_width = (list_w - 3 * cell_w) * 3 / 5;
    for (i, (item_index, positions)) in picker.matches.iter().enumerate().skip(picker.scroll).take(rows) {
        let item = &picker.items[*item_index];
        let ry = py + row_h * (1 + (i - picker.scroll) as i32) + 2;
        if i == picker.selected {
            ui.painter.fill_rect(Rect::new(rect.x + 1, ry - 1, list_w - 2, row_h), selected_bg);
        }
        let mut lx = rect.x + cell_w;
        let max_label_cells = if item.detail.is_empty() { (list_w - 2 * cell_w) / cell_w } else { label_width / cell_w } as usize;
        for (ci, ch) in item.label.chars().enumerate() {
            if ci >= max_label_cells {
                ui.text(lx, ry, "…", dim, false);
                break;
            }
            let color = if positions.contains(&ci) { match_color } else { fg };
            if ch != ' ' {
                ui.glyph(lx as f32, ry as f32 + ui.font.ascent, ch, color, positions.contains(&ci), false, 1.0);
            }
            lx += cell_w * FontSet::char_cells(ch).max(1) as i32;
        }
        if !item.detail.is_empty() {
            let dx = rect.x + cell_w + label_width + cell_w;
            let available = ((rect.x + list_w - cell_w - dx) / cell_w).max(0) as usize;
            let detail: String = item.detail.chars().take(available).collect();
            ui.text(dx, ry, &detail, dim, false);
        }
    }

    let top = py + row_h + 1;
    let list_rect = Rect::new(rect.x, top, list_w, rect.bottom() - top - 1);
    let mut preview_rect = Rect::new(rect.right(), top, 0, 0);
    if has_preview {
        preview_rect = Rect::new(rect.x + list_w, top, rect.w - list_w - 1, rect.bottom() - top - 1);
        ui.painter.vline(rect.x + list_w, top, preview_rect.h, border);
        let item = picker.selected_item().cloned();
        draw_preview(ed, ui, preview_rect, item);
    }
    ui.painter.reset_clip();
    ed.picker_rects = Some((list_rect, preview_rect));
    ed.overlay = Overlay::Picker(picker);
}

/// Most bytes a file is read for the preview.
const PREVIEW_LIMIT: u64 = 8 * 1024 * 1024;

/// Build the preview for an item, unless it is the one already built.
fn ensure_preview(ed: &mut Editor, item: &PickerItem, rows: usize) {
    let (key, center) = match &item.data {
        ItemData::Path(path) => (format!("file:{}", path.display()), None),
        ItemData::Location { path, line } => (format!("file:{}:{line}", path.display()), Some(*line)),
        ItemData::Buffer(id) => (format!("buffer:{}", id.0), None),
        _ => (format!("none:{}", item.label), None),
    };
    if ed.preview.as_ref().is_some_and(|p| p.key == key) {
        return;
    }
    let mut preview = Preview { key, file: None, buffer_id: None, center_line: center, scroll: 0, note: None };
    match &item.data {
        ItemData::Path(path) | ItemData::Location { path, .. } => match std::fs::metadata(path) {
            Ok(meta) if meta.len() > PREVIEW_LIMIT => preview.note = Some("(file too large to preview)".to_string()),
            Ok(_) => match std::fs::read(path) {
                Ok(bytes) if bytes.iter().take(1024).any(|&b| b == 0) => preview.note = Some("(binary file)".to_string()),
                Ok(bytes) => {
                    let mut buffer = Buffer::scratch(crate::buffer::BufferId(0));
                    buffer.path = Some(path.clone());
                    buffer.set_text(&String::from_utf8_lossy(&bytes).replace("\r\n", "\n"));
                    buffer.detect_language(&ed.registry);
                    preview.file = Some(buffer);
                }
                Err(err) => preview.note = Some(format!("({err})")),
            },
            Err(_) => preview.note = Some("(new file)".to_string()),
        },
        ItemData::Buffer(id) => preview.buffer_id = Some(*id),
        ItemData::Dir => preview.note = Some("(directory)".to_string()),
        _ => {}
    }
    // Start with the matched line in the middle of the pane.
    if let Some(center) = preview.center_line {
        preview.scroll = center.saturating_sub(rows / 2);
    }
    ed.preview = Some(preview);
}

/// The preview pane of a picker: the start of the file, or the matched line
/// in the middle, highlighted.
fn draw_preview(ed: &mut Editor, ui: &mut Ui, rect: Rect, item: Option<PickerItem>) {
    let Some(item) = item else { return };
    let cell_w = ui.font.cell_w;
    let cell_h = ui.font.cell_h;
    let rows = (rect.h as f32 / cell_h).floor().max(1.0) as usize;
    ensure_preview(ed, &item, rows);
    let fg = ed.theme.ui("ui.text", Color::WHITE);
    let dim = ed.theme.ui("ui.dim", fg);
    let popup_bg = ed.theme.ui("ui.popup", Color::BLACK);
    let line_bg = ed.theme.ui("ui.popup.match", fg).blend_over(popup_bg, 64);
    let base = Style::plain(fg);
    let tab_width = ed.config.tab_width;
    ui.painter.set_clip(rect);

    let Editor { preview, buffers, registry, highlighter, styles, .. } = ed;
    let Some(preview) = preview else { return };
    let buffer = match (&mut preview.file, preview.buffer_id) {
        (Some(file), _) => Some(file),
        (None, Some(id)) => buffers.get_mut(&id),
        _ => None,
    };
    let Some(buffer) = buffer else {
        if let Some(note) = &preview.note {
            ui.text(rect.x + cell_w as i32, rect.y + 2, note, dim, false);
        }
        return;
    };
    if buffer.is_terminal() {
        ui.text(rect.x + cell_w as i32, rect.y + 2, "(terminal)", dim, false);
        return;
    }

    let line_count = buffer.line_count();
    preview.scroll = preview.scroll.min(line_count.saturating_sub(rows));
    let first = preview.scroll;
    let gutter = (line_count.max(1).to_string().len().max(3) + 2) as f32 * cell_w;
    let mut spans: Vec<Span> = Vec::new();
    let mut state = None;
    let mut y = rect.y as f32 + 2.0;
    for line in first..line_count.min(first + rows) {
        let text = buffer.line_text(line);
        spans.clear();
        if let Some(lang) = buffer.language {
            let state = state.get_or_insert_with(|| {
                buffer.highlight.ensure(line.saturating_sub(1), &buffer.text, lang, registry, highlighter);
                buffer.highlight.state_before(line)
            });
            highlighter.highlight_line(registry, lang, &text, state, &mut spans);
        }
        let layout = layout_line(&text, &spans, styles, base, ui.font, tab_width, false, None);
        if Some(line) == preview.center_line {
            ui.painter.fill_rect(Rect::new(rect.x, y as i32, rect.w, cell_h as i32), line_bg);
        }
        let label = (line + 1).to_string();
        let lx = rect.x + gutter as i32 - ui.text_width(&label) - cell_w as i32;
        ui.text(lx, y as i32, &label, dim, false);
        let baseline = y + ui.font.ascent;
        for cell in &layout.cells {
            if cell.hidden {
                continue;
            }
            let x = rect.x as f32 + gutter + cell.x;
            if x > rect.right() as f32 {
                break;
            }
            ui.cell_glyph(x, baseline, cell, cell.style.fg);
        }
        y += cell_h;
    }
}

/// Which terminal cell (column, screen row) lies under a pixel of a terminal frame.
pub fn terminal_cell_at(font: &FontSet, rect: Rect, px: i32, py: i32) -> (usize, usize) {
    let gutter = font.cell_w as i32;
    let col = ((px - rect.x - gutter).max(0) as f32 / font.cell_w) as usize;
    let row = ((py - rect.y).max(0) as f32 / font.cell_h) as usize;
    (col, row)
}

/// Which text position lies under a pixel in a frame (for mouse clicks).
pub fn position_at(ed: &mut Editor, font: &FontSet, view_id: ViewId, rect: Rect, px: i32, py: i32) -> Option<usize> {
    let buffer_id = ed.views[&view_id].buffer;
    let buffer = &ed.buffers[&buffer_id];
    let gutter = gutter_width(ed, buffer, font.cell_w);
    let offset = welcome_offset(buffer);
    let rect = Rect::new(rect.x, rect.y + offset, rect.w, (rect.h - offset).max(0));
    let text_x = rect.x + gutter;
    if buffer.is_terminal() {
        let view = &ed.views[&view_id];
        let row = view.scroll_line + ((py - rect.y).max(0) as f32 / font.cell_h) as usize;
        let col = ((px - text_x).max(0) as f32 / font.cell_w) as usize;
        let line = row.min(buffer.line_count() - 1);
        return Some((buffer.line_start(line) + col).min(buffer.line_end(line)));
    }
    let base = Style::plain(ed.theme.ui("ui.text", Color::WHITE));
    let tab_width = ed.config.tab_width;
    let scroll_x = ed.views[&view_id].scroll_col as f32 * font.cell_w;
    let (first_line, slide) = animated_scroll(ed, view_id, font.cell_h);
    let wrap = if ed.config.wrap { Some(text_rect_width(rect, gutter)) } else { None };
    let layouts = layout_visible_lines(ed, font, view_id, first_line, rect.h as f32 + font.cell_h, base, tab_width, wrap);
    let y = (py - rect.y) as f32 + slide as f32;
    let buffer = &ed.buffers[&buffer_id];
    for visible in &layouts {
        if y >= visible.y && y < visible.y + visible.layout.height {
            let x = (px - text_x) as f32 + scroll_x;
            let row = ((y - visible.y) / visible.layout.row_height) as usize;
            let col = visible.layout.char_at(row, x);
            return Some((visible.start + col).min(buffer.line_end(visible.line)));
        }
    }
    layouts.last().map(|visible| buffer.line_end(visible.line))
}
