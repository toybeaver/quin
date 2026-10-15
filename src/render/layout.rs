//! Turning a line of text plus its highlight spans into positioned glyph cells.
//! Both drawing and mouse hit-testing go through this, so they always agree.

use crate::editor::StyleTable;
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
    pub height: f32,
    pub ascent: f32,
    pub width: f32,
}

impl LineLayout {
    /// X position where a cursor sitting on `char_index` is drawn.
    pub fn x_of(&self, char_index: usize) -> f32 {
        match self.cells.iter().find(|c| c.char_index == char_index) {
            Some(cell) => cell.x,
            None => self.width,
        }
    }

    /// Width of the cell at `char_index` (a full cell for the end-of-line cursor).
    pub fn width_of(&self, char_index: usize, cell_w: f32) -> f32 {
        match self.cells.iter().find(|c| c.char_index == char_index && !c.hidden) {
            Some(cell) => cell.width.max(cell_w),
            None => cell_w,
        }
    }

    /// Character index under a pixel position.
    pub fn char_at_x(&self, x: f32) -> usize {
        let mut last = 0;
        for cell in &self.cells {
            if cell.hidden {
                continue;
            }
            if x < cell.x + cell.width / 2.0 {
                return cell.char_index;
            }
            last = cell.char_index + 1;
        }
        last
    }
}

/// Lay out `line` (without its newline). `spans` are byte ranges from the
/// highlighter. With `reveal`, concealed and replaced tokens show as written
/// (used for the line the cursor is on).
pub fn layout_line(
    line: &str,
    spans: &[Span],
    styles: &StyleTable,
    base: Style,
    font: &FontSet,
    tab_width: usize,
    reveal: bool,
) -> LineLayout {
    let mut layout = LineLayout { cells: Vec::with_capacity(line.len()), height: font.cell_h, ascent: font.ascent, width: 0.0 };
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
    layout
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
            && cells[j].style.scale == cells[i].style.scale
        {
            j += 1;
        }
        if j - i >= 2 {
            let run: String = cells[i..j].iter().map(|c| c.ch).collect();
            if let Some(shaped) = font.shape(&run, cells[i].style.bold) {
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
        StyleTable { styles: vec![Style::plain(Color::WHITE), conceal, big], replacements: vec![None, None, Some("•".to_string())] }
    }

    #[test]
    fn conceal_and_replace() {
        let font = FontSet::builtin(12.0);
        let styles = table();
        let spans = [Span { start: 0, end: 2, scope: ScopeId(1) }, Span { start: 2, end: 4, scope: ScopeId(2) }];
        let layout = layout_line("**- x", &spans, &styles, Style::plain(Color::WHITE), &font, 4, false);
        assert!(layout.cells[0].hidden && layout.cells[1].hidden);
        assert_eq!(layout.cells[2].ch, '•');
        assert!(layout.cells[3].hidden);
        assert!(layout.height > font.cell_h);
        let revealed = layout_line("**- x", &spans, &styles, Style::plain(Color::WHITE), &font, 4, true);
        assert!(revealed.cells.iter().all(|c| !c.hidden));
        assert_eq!(revealed.char_at_x(0.0), 0);
        assert_eq!(revealed.char_at_x(revealed.width + 10.0), 5);
    }

    #[test]
    fn tabs_expand_to_stops() {
        let font = FontSet::builtin(12.0);
        let styles = table();
        let layout = layout_line("a\tb", &[], &styles, Style::plain(Color::WHITE), &font, 4, false);
        assert_eq!(layout.cells[1].width, font.cell_w * 3.0);
        assert_eq!(layout.cells[2].x, font.cell_w * 4.0);
    }
}
