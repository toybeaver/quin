//! Turning a line of text plus its highlight spans into positioned glyph cells.
//! Both drawing and mouse hit-testing go through this, so they always agree.

use crate::editor::StyleTable;
use crate::render::blocks::{PlacedImage, TableRow};
use crate::render::font::{FontSet, GlyphId, LIGATURE_CHARS};
use crate::syntax::highlight::Span;
use crate::theme::Style;

#[derive(Debug, Clone)]
pub struct Cell {
    /// Index of the source character within the line.
    pub char_index: usize,
    /// Horizontal position in pixels from the start of the line.
    pub x: f32,
    pub width: f32,
    /// What to draw (differs from the source for replacements like `•`).
    pub ch: char,
    pub style: Style,
    /// Concealed cells take no space and draw nothing.
    pub hidden: bool,
    /// Glyph to draw instead of `ch` when the cell starts a ligature.
    pub glyph: Option<GlyphId>,
    /// Covered by the ligature glyph of an earlier cell: keeps its width, draws nothing.
    pub ligated: bool,
}

impl Cell {
    fn new(char_index: usize, x: f32, width: f32, ch: char, style: Style, hidden: bool) -> Cell {
        Cell { char_index, x, width, ch, style, hidden, glyph: None, ligated: false }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LineLayout {
    pub cells: Vec<Cell>,
    /// Height of the whole line: `row_height` times the number of visual rows.
    pub height: f32,
    pub ascent: f32,
    /// Width of the line as if it were not wrapped (cells keep unwrapped x).
    pub width: f32,
    /// Visual rows when wrapping: the first character of each row and the
    /// unwrapped x it starts at. One row when not wrapping.
    pub rows: Vec<(usize, f32)>,
    pub row_height: f32,
    /// Space above the first visual row, inside `height` (table padding).
    pub text_y: f32,
    /// Images drawn below the text of the line (`blocks.rs`).
    pub images: Vec<PlacedImage>,
    /// Borders to draw when the line is a row of a table (`blocks.rs`).
    pub table: Option<TableRow>,
}

impl LineLayout {
    /// Unwrapped x position where a cursor sitting on `char_index` is drawn.
    pub fn x_of(&self, char_index: usize) -> f32 {
        match self.cells.iter().find(|c| c.char_index == char_index) {
            Some(cell) => cell.x,
            None => self.width,
        }
    }

    /// Index of the last character plus one.
    fn len(&self) -> usize {
        self.cells.last().map(|c| c.char_index + 1).unwrap_or(0)
    }

    /// First character of the row after `row` (one past the line for the last).
    fn row_end(&self, row: usize) -> usize {
        self.rows.get(row + 1).map(|r| r.0).unwrap_or(self.len() + 1)
    }

    /// The visual row holding `char_index` and the x inside that row.
    pub fn pos_of(&self, char_index: usize) -> (usize, f32) {
        let row = self.rows.iter().rposition(|(start, _)| *start <= char_index).unwrap_or(0);
        (row, self.x_of(char_index) - self.rows[row].1)
    }

    /// Horizontal extents of the characters `from..to` on every visual row
    /// they touch: (row, x0, x1) relative to the row's start. A range past
    /// the last character reaches `extra` beyond it.
    pub fn row_segments(&self, from: usize, to: usize, extra: f32) -> Vec<(usize, f32, f32)> {
        let len = self.len();
        let mut out = Vec::new();
        for (row, &(start, row_x)) in self.rows.iter().enumerate() {
            let (a, b) = (from.max(start), to.min(self.row_end(row)));
            if a >= b {
                continue;
            }
            let x0 = self.x_of(a) - row_x;
            let x1 = if b > len { self.width - row_x + extra } else { self.x_of(b) - row_x };
            out.push((row, x0, x1));
        }
        out
    }

    /// Width of the cell at `char_index` (a full cell for the end-of-line cursor).
    pub fn width_of(&self, char_index: usize, cell_w: f32) -> f32 {
        match self.cells.iter().find(|c| c.char_index == char_index && !c.hidden) {
            Some(cell) => cell.width.max(cell_w),
            None => cell_w,
        }
    }

    /// Character index under a pixel position on a visual row.
    pub fn char_at(&self, row: usize, x: f32) -> usize {
        let row = row.min(self.rows.len().saturating_sub(1));
        let (start, row_x) = self.rows[row];
        let end = self.row_end(row);
        let mut last = start;
        for cell in self.cells.iter().filter(|c| !c.hidden && c.char_index >= start && c.char_index < end) {
            if x < cell.x - row_x + cell.width / 2.0 {
                return cell.char_index;
            }
            last = cell.char_index + 1;
        }
        last
    }
}

/// Lay out `line` (without its newline). `spans` are byte ranges from the
/// highlighter. With `reveal`, concealed and replaced tokens show as written
/// (used for the line the cursor is on). `wrap` is the width to wrap at, if
/// the line should wrap.
#[allow(clippy::too_many_arguments)]
pub fn layout_line(
    line: &str,
    spans: &[Span],
    styles: &StyleTable,
    base: Style,
    font: &FontSet,
    tab_width: usize,
    reveal: bool,
    wrap: Option<f32>,
) -> LineLayout {
    let mut layout = LineLayout {
        cells: Vec::with_capacity(line.len()),
        height: font.cell_h,
        ascent: font.ascent,
        width: 0.0,
        rows: vec![(0, 0.0)],
        row_height: font.cell_h,
        text_y: 0.0,
        images: Vec::new(),
        table: None,
    };
    let mut x = 0.0f32;
    let mut column = 0usize; // in base cells, for tab stops
    let mut span_index = 0;
    let mut replaced_span: Option<(usize, usize)> = None; // (start, end) bytes of a span already replaced

    for (char_index, (byte, ch)) in line.char_indices().enumerate() {
        while span_index < spans.len() && spans[span_index].end <= byte {
            span_index += 1;
        }
        let span = spans.get(span_index).filter(|s| s.start <= byte);
        let mut style = match span {
            Some(s) => styles.styles[s.scope.0 as usize],
            None => base,
        };
        let replacement = span.and_then(|s| styles.replacements[s.scope.0 as usize].as_ref().map(|r| (s.start, s.end, r)));

        if style.scale != 1.0 {
            let (_, height, ascent) = font.scaled_metrics(style.scale);
            layout.height = layout.height.max(height);
            layout.ascent = layout.ascent.max(ascent);
        }

        // Replacement: draw the replacement text once, hide the rest of the span.
        if let (Some((start, end, text)), false) = (replacement, reveal) {
            if replaced_span != Some((start, end)) {
                replaced_span = Some((start, end));
                for rch in text.chars() {
                    let width = font.cell_w * style.scale * FontSet::char_cells(rch).max(1) as f32;
                    layout.cells.push(Cell::new(char_index, x, width, rch, style, false));
                    x += width;
                    column += 1;
                }
            } else {
                layout.cells.push(Cell::new(char_index, x, 0.0, ch, style, true));
            }
            continue;
        }

        if style.conceal && !reveal {
            layout.cells.push(Cell::new(char_index, x, 0.0, ch, style, true));
            continue;
        }
        if style.conceal {
            style.conceal = false;
        }

        let width = if ch == '\t' {
            let cells = tab_width - column % tab_width;
            column += cells;
            font.cell_w * cells as f32
        } else {
            let cells = FontSet::char_cells(ch).max(1);
            column += cells;
            font.cell_w * style.scale * cells as f32
        };
        layout.cells.push(Cell::new(char_index, x, width, ch, style, false));
        x += width;
    }
    layout.width = x;
    if font.ligatures {
        apply_ligatures(&mut layout.cells, font);
    }
    layout.row_height = layout.height;
    if let Some(width) = wrap {
        layout.rows = wrap_rows(&layout.cells, width);
        layout.height = layout.row_height * layout.rows.len() as f32;
    }
    layout
}

/// Where a line breaks into visual rows to fit `width`: before the cell
/// that would overflow, or at the last space before it when there is one.
pub fn wrap_rows(cells: &[Cell], width: f32) -> Vec<(usize, f32)> {
    let mut rows = vec![(0usize, 0.0f32)];
    let mut row_x = 0.0;
    let mut last_space: Option<usize> = None;
    for (i, cell) in cells.iter().enumerate() {
        if cell.hidden {
            continue;
        }
        if cell.x + cell.width - row_x > width && cell.x > row_x {
            let at = last_space.filter(|s| cells[*s].x > row_x).unwrap_or(i);
            row_x = cells[at].x;
            rows.push((cells[at].char_index, row_x));
            last_space = None;
        }
        if cell.ch == ' ' {
            last_space = Some(i + 1);
        }
    }
    rows
}

fn may_ligate(cell: &Cell) -> bool {
    !cell.hidden && cell.width > 0.0 && LIGATURE_CHARS.contains(cell.ch)
}

/// Shape every run of two or more ligature characters drawn in the same
/// style, and record the resulting glyphs on the cells.
fn apply_ligatures(cells: &mut [Cell], font: &FontSet) {
    let mut i = 0;
    while i < cells.len() {
        if !may_ligate(&cells[i]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < cells.len()
            && may_ligate(&cells[j])
            && cells[j].style.bold == cells[i].style.bold
            && cells[j].style.italic == cells[i].style.italic
            && cells[j].style.scale == cells[i].style.scale
        {
            j += 1;
        }
        if j - i >= 2 {
            let run: String = cells[i..j].iter().map(|c| c.ch).collect();
            if let Some(shaped) = font.shape(&run, cells[i].style.bold, cells[i].style.italic) {
                for glyph in shaped {
                    let first = i + glyph.char_offset;
                    cells[first].glyph = Some(glyph.glyph);
                    for cell in cells.iter_mut().take((first + glyph.chars).min(j)).skip(first + 1) {
                        cell.ligated = true;
                    }
                }
            }
        }
        i = j;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Color;
    use crate::syntax::ScopeId;

    fn table() -> StyleTable {
        let mut conceal = Style::plain(Color::WHITE);
        conceal.conceal = true;
        let mut big = Style::plain(Color::WHITE);
        big.scale = 2.0;
        StyleTable {
            styles: vec![Style::plain(Color::WHITE), conceal, big],
            replacements: vec![None, None, Some("•".to_string())],
            lines: vec![Default::default(); 3],
        }
    }

    #[test]
    fn conceal_and_replace() {
        let font = FontSet::builtin(12.0);
        let styles = table();
        let spans = [Span { start: 0, end: 2, scope: ScopeId(1) }, Span { start: 2, end: 4, scope: ScopeId(2) }];
        let layout = layout_line("**- x", &spans, &styles, Style::plain(Color::WHITE), &font, 4, false, None);
        assert!(layout.cells[0].hidden && layout.cells[1].hidden);
        assert_eq!(layout.cells[2].ch, '•');
        assert!(layout.cells[3].hidden);
        assert!(layout.height > font.cell_h);
        let revealed = layout_line("**- x", &spans, &styles, Style::plain(Color::WHITE), &font, 4, true, None);
        assert!(revealed.cells.iter().all(|c| !c.hidden));
        assert_eq!(revealed.char_at(0, 0.0), 0);
        assert_eq!(revealed.char_at(0, revealed.width + 10.0), 5);
    }

    #[test]
    fn tabs_expand_to_stops() {
        let font = FontSet::builtin(12.0);
        let styles = table();
        let layout = layout_line("a\tb", &[], &styles, Style::plain(Color::WHITE), &font, 4, false, None);
        assert_eq!(layout.cells[1].width, font.cell_w * 3.0);
        assert_eq!(layout.cells[2].x, font.cell_w * 4.0);
    }

    #[test]
    fn wraps_at_spaces_and_inside_long_words() {
        let font = FontSet::builtin(12.0);
        let styles = table();
        let w = font.cell_w;
        // Eight cells wide: "hello world foo" breaks after "hello " and "world ".
        let layout = layout_line("hello world foo", &[], &styles, Style::plain(Color::WHITE), &font, 4, false, Some(w * 8.0));
        assert_eq!(layout.rows.iter().map(|r| r.0).collect::<Vec<_>>(), vec![0, 6, 12]);
        assert_eq!(layout.height, font.cell_h * 3.0);
        assert_eq!(layout.pos_of(7), (1, w));
        assert_eq!(layout.char_at(2, w * 1.4), 13);
        assert_eq!(layout.row_segments(4, 8, w), vec![(0, w * 4.0, w * 6.0), (1, 0.0, w * 2.0)]);
        // A word longer than the width breaks inside.
        let long = layout_line("abcdefghijklmnop", &[], &styles, Style::plain(Color::WHITE), &font, 4, false, Some(w * 8.0));
        assert_eq!(long.rows.iter().map(|r| r.0).collect::<Vec<_>>(), vec![0, 8]);
        let plain = layout_line("hello world foo", &[], &styles, Style::plain(Color::WHITE), &font, 4, false, None);
        assert_eq!(plain.rows.len(), 1);
    }
}
