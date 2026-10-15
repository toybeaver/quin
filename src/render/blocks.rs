//! Line rendering that goes beyond single tokens, driven by the `grid`,
//! `center` and `image` style attributes of syntax files: rows of pipe tables
//! aligned into a grid, centered lines, and images below their line. All of
//! it works on finished line layouts and only moves or hides cells, so drawing
//! and mouse hit-testing (both read the layouts) still agree.

use std::path::{Path, PathBuf};

use crate::editor::StyleTable;
use crate::images::Images;
use crate::render::layout::{LineLayout, wrap_rows};
use crate::syntax::highlight::Span;
use crate::terminal::WakeFn;

/// Space between an image and what is around it, in pixels.
const IMAGE_GAP: f32 = 6.0;

/// An image to draw, relative to its line: `x` is an unwrapped position like
/// a cell's, `y` is measured from the top of the line.
#[derive(Debug, Clone)]
pub struct PlacedImage {
    pub path: PathBuf,
    pub x: f32,
    pub y: f32,
    pub width: usize,
    pub height: usize,
}

/// Borders of a table row: vertical lines at `borders` (unwrapped x), a
/// horizontal line through the middle for the `|---|` row, and lines above
/// the first row and below the last. Those two rows get half a row of
/// padding on their outer side, the gap the `|---|` line leaves.
#[derive(Debug, Clone, Default)]
pub struct TableRow {
    pub borders: Vec<f32>,
    pub separator: bool,
    pub top: bool,
    pub bottom: bool,
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

/// Show the images named on a line below its text. Once an image is loaded
/// its token (`![alt](path)`, `<img ...>`) is hidden, unless `reveal`; a line
/// left with nothing but blanks loses its text row and is just the images.
#[allow(clippy::too_many_arguments)]
pub fn place_images(
    layout: &mut LineLayout,
    line: &str,
    spans: &[Span],
    styles: &StyleTable,
    images: &mut Images,
    wake: Option<&WakeFn>,
    dir: &Path,
    (max_w, max_h): (f32, f32),
    reveal: bool,
    wrap: Option<f32>,
) {
    let mut ready = Vec::new();
    for (start, end) in image_tokens(spans, styles) {
        let token = &line[start..end];
        let Some(path) = image_path(token).and_then(|p| resolve(dir, p)) else { continue };
        let Some(natural) = images.size(&path, wake) else { continue };
        let (w, h) = display_size(natural, token, max_w, max_h);
        if w > 0 && h > 0 {
            ready.push((start, end, path, w, h));
        }
    }
    if ready.is_empty() {
        return;
    }

    if !reveal {
        let chars: Vec<usize> = line.char_indices().map(|(byte, _)| byte).collect();
        for cell in &mut layout.cells {
            let byte = chars.get(cell.char_index).copied().unwrap_or(line.len());
            if ready.iter().any(|(start, end, ..)| byte >= *start && byte < *end) {
                cell.hidden = true;
            }
        }
        reflow(layout, wrap);
    }
    let blank = layout.cells.iter().all(|c| c.hidden || c.ch.is_whitespace());
    let mut y = if blank && !reveal {
        for cell in &mut layout.cells {
            cell.hidden = true;
        }
        reflow(layout, None);
        IMAGE_GAP
    } else {
        layout.height + IMAGE_GAP
    };

    // Side by side, starting a new row when the next one does not fit.
    let (mut x, mut row_h) = (0.0f32, 0usize);
    for (_, _, path, width, height) in ready {
        if x > 0.0 && x + width as f32 > max_w {
            y += row_h as f32 + IMAGE_GAP;
            x = 0.0;
            row_h = 0;
        }
        layout.images.push(PlacedImage { path, x, y, width, height });
        x += width as f32 + IMAGE_GAP;
        row_h = row_h.max(height);
    }
    layout.height = y + row_h as f32 + IMAGE_GAP;
}

/// Byte ranges of the image tokens of a line: runs of touching spans whose
/// scope has the `image` attribute.
fn image_tokens(spans: &[Span], styles: &StyleTable) -> Vec<(usize, usize)> {
    let is_image = |span: &Span| styles.lines[span.scope.0 as usize].image;
    let mut tokens: Vec<(usize, usize)> = Vec::new();
    for span in spans.iter().filter(|s| is_image(s)) {
        match tokens.last_mut() {
            Some(last) if last.1 == span.start => last.1 = span.end,
            _ => tokens.push((span.start, span.end)),
        }
    }
    tokens
}

/// The path an image token points to: `![alt](path "title")`,
/// `[![alt](path)](link)` or `<img src="path">`.
pub fn image_path(token: &str) -> Option<&str> {
    if token.starts_with('<') {
        let (_, rest) = token.split_once("src=")?;
        let quote = rest.chars().next()?;
        let path = if quote == '"' || quote == '\'' { rest[1..].split(quote).next()? } else { rest.split([' ', '>']).next()? };
        return Some(path);
    }
    let (_, rest) = token.split_once("](")?;
    match rest.strip_prefix('<') {
        Some(inside) => inside.split('>').next(),
        None => rest.split([')', ' ']).next(),
    }
}

/// Where a local image lives, relative to the directory of the file that
/// shows it. Remote images are not supported.
fn resolve(dir: &Path, path: &str) -> Option<PathBuf> {
    if path.is_empty() || path.contains("://") || path.starts_with("data:") {
        return None;
    }
    let path = path.split(['#', '?']).next()?.replace("%20", " ");
    let path = Path::new(&path);
    if path.is_absolute() && path.exists() {
        return Some(path.to_path_buf());
    }
    // A leading `/` means the root of the repository on forges; the file's
    // directory is the best guess here.
    Some(dir.join(path.strip_prefix("/").unwrap_or(path)))
}

/// Size to draw an image at: the `width`/`height` attributes of an `<img>`
/// tag when given (keeping the aspect ratio when only one is), else its own
/// size, shrunk to fit the frame.
fn display_size((nw, nh): (usize, usize), token: &str, max_w: f32, max_h: f32) -> (usize, usize) {
    let attr = |name: &str| -> Option<f32> {
        let (_, rest) = token.split_once(&format!("{name}="))?;
        let digits: String = rest.trim_start_matches(['"', '\'']).chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok().filter(|v: &f32| *v > 0.0)
    };
    let (nw, nh) = (nw as f32, nh as f32);
    let (w, h) = match (attr("width"), attr("height")) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w * nh / nw),
        (None, Some(h)) => (h * nw / nh, h),
        (None, None) => (nw, nh),
    };
    let scale = (max_w / w).min(max_h / h).min(1.0);
    ((w * scale) as usize, (h * scale) as usize)
}

/// Put cells back side by side after some were hidden, and wrap again.
fn reflow(layout: &mut LineLayout, wrap: Option<f32>) {
    let mut x = 0.0;
    for cell in &mut layout.cells {
        if cell.hidden {
            cell.width = 0.0;
        }
        cell.x = x;
        x += cell.width;
    }
    layout.width = x;
    layout.rows = match wrap {
        Some(width) => wrap_rows(&layout.cells, width),
        None => vec![(0, 0.0)],
    };
    layout.height = layout.row_height * layout.rows.len() as f32;
}

// ---------------------------------------------------------------------------
// Centered lines
// ---------------------------------------------------------------------------

/// Center a line's images, and its text unless `reveal` (the line is then
/// shown as written), in `width` pixels. Wrapped text already fills the
/// width and stays as it is.
pub fn center(layout: &mut LineLayout, width: f32, reveal: bool) {
    if !reveal && layout.rows.len() == 1 && layout.width < width {
        let shift = ((width - layout.width) / 2.0).floor();
        for cell in &mut layout.cells {
            cell.x += shift;
        }
        layout.width += shift;
    }
    // Each row of images on its own.
    let mut i = 0;
    while i < layout.images.len() {
        let y = layout.images[i].y;
        let row: Vec<usize> = (i..layout.images.len()).take_while(|&k| layout.images[k].y == y).collect();
        let last = &layout.images[*row.last().expect("row has an image")];
        let shift = ((width - (last.x + last.width as f32)) / 2.0).max(0.0).floor();
        for &k in &row {
            layout.images[k].x += shift;
        }
        i += row.len();
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A table row as written: where its cells are and how wide their content is.
#[derive(Debug, Clone)]
pub struct TableLine {
    /// Character index of every `|` that separates cells.
    pipes: Vec<usize>,
    /// Character range of each cell's content, blanks around it excluded.
    cells: Vec<(usize, usize)>,
    /// Width of each cell's content as laid out (hidden markers take none).
    pub widths: Vec<f32>,
    /// Column alignments when this is the `|---|:--:|` row under the header.
    pub separator: Option<Vec<Align>>,
}

/// Measure a table row. `layout` must not reveal concealed text, so that a
/// table keeps its shape when the cursor enters one of its rows.
pub fn measure_row(layout: &LineLayout, line: &str) -> TableLine {
    let chars: Vec<char> = line.chars().collect();
    // Separating pipes: not escaped, not inside `code`.
    let mut pipes = Vec::new();
    let mut in_code = false;
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '`' => in_code = !in_code,
            '|' if !in_code && (i == 0 || chars[i - 1] != '\\') => pipes.push(i),
            _ => {}
        }
    }
    let mut bounds: Vec<(usize, usize)> = pipes.windows(2).map(|w| (w[0] + 1, w[1])).collect();
    // The closing pipe is optional.
    if let Some(&last) = pipes.last()
        && chars[last + 1..].iter().any(|c| !c.is_whitespace())
    {
        bounds.push((last + 1, chars.len()));
    }
    let cells: Vec<(usize, usize)> = bounds
        .iter()
        .map(|&(mut a, mut b)| {
            while a < b && chars[a].is_whitespace() {
                a += 1;
            }
            while b > a && chars[b - 1].is_whitespace() {
                b -= 1;
            }
            (a, b)
        })
        .collect();

    let is_separator = cells.iter().all(|&(a, b)| b > a && chars[a..b].iter().all(|c| *c == '-' || *c == ':'))
        && cells.iter().any(|&(a, b)| chars[a..b].contains(&'-'));
    let separator = is_separator.then(|| {
        cells
            .iter()
            .map(|&(a, b)| match (chars[a] == ':', chars[b - 1] == ':') {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            })
            .collect()
    });
    let widths = cells
        .iter()
        .map(|&(a, b)| {
            if is_separator {
                return 0.0;
            }
            layout.cells.iter().filter(|c| !c.hidden && c.char_index >= a && c.char_index < b).map(|c| c.width).sum()
        })
        .collect();
    TableLine { pipes, cells, widths, separator }
}

/// What every row of one table shares.
#[derive(Debug, Default)]
pub struct Table {
    /// Width of the widest content of each column.
    pub columns: Vec<f32>,
    pub aligns: Vec<Align>,
    /// Lines of the first and last rows, and of the `|---|` row.
    pub first: usize,
    pub last: usize,
    pub separator: Option<usize>,
}

impl Table {
    pub fn new(line: usize) -> Table {
        Table { first: line, last: line, ..Table::default() }
    }

    /// Take a row of the table into account.
    pub fn add(&mut self, line: usize, row: &TableLine) {
        self.first = self.first.min(line);
        self.last = self.last.max(line);
        if self.columns.len() < row.widths.len() {
            self.columns.resize(row.widths.len(), 0.0);
        }
        for (column, width) in self.columns.iter_mut().zip(&row.widths) {
            *column = column.max(*width);
        }
        if let Some(aligns) = &row.separator {
            self.aligns = aligns.clone();
            self.separator = Some(line);
        }
    }
}

/// Move the cells of a row into the table's columns: pipes become borders,
/// blanks around cell content disappear, content is aligned inside its
/// column, and header rows (above `|---|`) turn bold.
pub fn align_row(layout: &mut LineLayout, line: usize, row: &TableLine, table: &Table, cell_w: f32) {
    let (border_w, pad) = (cell_w, cell_w);
    let mut column_x = vec![0.0f32];
    for width in &table.columns {
        column_x.push(column_x.last().expect("starts with one") + border_w + 2.0 * pad + width);
    }
    let header = table.separator.is_some_and(|sep| line < sep);
    let is_separator = row.separator.is_some();

    let mut x = 0.0;
    let mut entered: Option<usize> = None; // the cell whose content we are in
    for cell in &mut layout.cells {
        let c = cell.char_index;
        if header {
            // Bold glyphs differ from the regular ones that were shaped.
            cell.style.bold = true;
            cell.glyph = None;
            cell.ligated = false;
        }
        if let Some(k) = row.pipes.iter().position(|&p| p == c) {
            x = column_x[k.min(column_x.len() - 1)];
            // The border is drawn as a line; the cell stays for the cursor.
            *cell = crate::render::layout::Cell { x, width: border_w, ch: ' ', hidden: false, glyph: None, ligated: false, ..cell.clone() };
            x += border_w;
            continue;
        }
        match row.cells.iter().position(|&(a, b)| c >= a && c < b) {
            Some(k) if !is_separator => {
                if entered != Some(k) {
                    entered = Some(k);
                    let room = table.columns.get(k).copied().unwrap_or(0.0) - row.widths[k];
                    let offset = match table.aligns.get(k) {
                        Some(Align::Right) => room,
                        Some(Align::Center) => (room / 2.0).floor(),
                        _ => 0.0,
                    };
                    x = column_x[k] + border_w + pad + offset;
                }
                cell.x = x;
                if !cell.hidden {
                    x += cell.width;
                }
            }
            _ => {
                cell.hidden = true;
                cell.width = 0.0;
                cell.x = x;
            }
        }
    }
    layout.width = *column_x.last().expect("starts with one") + border_w;
    layout.rows = vec![(0, 0.0)];
    let (top, bottom) = (line == table.first, line == table.last);
    let pad = (layout.row_height / 2.0).floor();
    layout.text_y = if top { pad } else { 0.0 };
    layout.height = layout.row_height + layout.text_y + if bottom { pad } else { 0.0 };
    layout.table =
        Some(TableRow { borders: column_x.iter().map(|x| x + (border_w / 2.0).floor()).collect(), separator: is_separator, top, bottom });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Color;
    use crate::render::font::FontSet;
    use crate::render::layout::layout_line;
    use crate::theme::Style;

    fn plain(line: &str, font: &FontSet) -> LineLayout {
        let styles = StyleTable { styles: vec![], replacements: vec![], lines: vec![] };
        layout_line(line, &[], &styles, Style::plain(Color::WHITE), font, 4, false, None)
    }

    #[test]
    fn finds_image_paths() {
        assert_eq!(image_path("![logo](assets/a.png)"), Some("assets/a.png"));
        assert_eq!(image_path("![logo](<my a.png> \"title\")"), Some("my a.png"));
        assert_eq!(image_path("[![b](badge.png)](https://x)"), Some("badge.png"));
        assert_eq!(image_path("<img src=\"icon.png\" width=\"160\">"), Some("icon.png"));
        assert_eq!(image_path("<img src='icon.png'>"), Some("icon.png"));
        assert_eq!(resolve(Path::new("/doc"), "https://x/y.png"), None);
        assert_eq!(resolve(Path::new("/doc"), "a%20b.png"), Some(PathBuf::from("/doc/a b.png")));
    }

    #[test]
    fn image_size_follows_attributes_and_fits() {
        assert_eq!(display_size((1000, 500), "<img src=x width=\"160\">", 800.0, 600.0), (160, 80));
        assert_eq!(display_size((1000, 500), "![a](x)", 800.0, 600.0), (800, 400));
        assert_eq!(display_size((100, 1000), "![a](x)", 800.0, 500.0), (50, 500));
    }

    #[test]
    fn table_rows_line_up() {
        let font = FontSet::builtin(12.0);
        let w = font.cell_w;
        let lines = ["| a | long cell |", "|:-:|---:|", "| wide one | b |"];
        let rows: Vec<TableLine> = lines.iter().map(|l| measure_row(&plain(l, &font), l)).collect();
        assert_eq!(rows[1].separator, Some(vec![Align::Center, Align::Right]));
        let mut table = Table::new(0);
        for (i, row) in rows.iter().enumerate() {
            table.add(i, row);
        }
        assert_eq!(table.columns, vec![w * 8.0, w * 9.0]);
        let mut layouts: Vec<LineLayout> = lines.iter().map(|l| plain(l, &font)).collect();
        for (i, layout) in layouts.iter_mut().enumerate() {
            align_row(layout, i, &rows[i], &table, w);
        }
        // Every row has the same borders and width.
        assert_eq!(layouts[0].table.as_ref().unwrap().borders, layouts[2].table.as_ref().unwrap().borders);
        assert_eq!(layouts[0].width, layouts[2].width);
        assert!(layouts[0].table.as_ref().unwrap().top && layouts[2].table.as_ref().unwrap().bottom);
        assert!(layouts[1].table.as_ref().unwrap().separator);
        // The first and last rows get the padding the |---| line has.
        let pad = (font.cell_h / 2.0).floor();
        assert_eq!((layouts[0].text_y, layouts[0].height), (pad, font.cell_h + pad));
        assert_eq!((layouts[1].text_y, layouts[1].height), (0.0, font.cell_h));
        assert_eq!((layouts[2].text_y, layouts[2].height), (0.0, font.cell_h + pad));
        // "a" is centered in an 8-cell column: border, pad, then 3.5 cells of room.
        let a = layouts[0].cells.iter().find(|c| c.ch == 'a').unwrap();
        assert_eq!(a.x, w + w + (w * 7.0 / 2.0).floor());
        assert!(a.style.bold, "rows above |---| are the header");
        // "b" is right-aligned: its right edge meets the column's padding.
        let b = layouts[2].cells.iter().find(|c| c.ch == 'b').unwrap();
        let second = layouts[2].table.as_ref().unwrap().borders[2] - (w / 2.0).floor();
        assert_eq!(b.x + b.width, second - w);
    }

    #[test]
    fn centers_text_and_images() {
        let font = FontSet::builtin(12.0);
        let mut layout = plain("ab", &font);
        layout.images.push(PlacedImage { path: PathBuf::new(), x: 0.0, y: 20.0, width: 100, height: 10 });
        center(&mut layout, 300.0, false);
        assert_eq!(layout.cells[0].x, ((300.0 - font.cell_w * 2.0) / 2.0).floor());
        assert_eq!(layout.images[0].x, 100.0);
    }
}
