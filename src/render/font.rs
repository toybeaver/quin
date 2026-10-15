//! Font loading, a glyph cache and ligature shaping on top of `fontdue` and
//! `rustybuzz`.
//!
//! The editor is monospace: every character occupies a whole number of cells
//! (`cell_w` wide, `cell_h` tall). A font set has a regular face, optional
//! bold, italic and bold-italic faces, and a symbols face for characters the
//! main font lacks (the embedded Nerd Font symbols, so prompts and file icons
//! always render). The embedded default is Iosevka (Nerd Font Mono build,
//! all four faces, subset to Latin, Greek, Cyrillic, punctuation, box drawing
//! and Powerline); a `font-family` or `font` in the config swaps in a system
//! font. Missing bold is faked by smearing, missing italic by a sub-pixel
//! shear, both once at rasterization. Bigger text (markdown headings) is
//! simply rasterized at a larger pixel size.
//!
//! Ligatures: runs of punctuation (`->`, `!=`, `::`...) are shaped with
//! rustybuzz, which applies the font's `liga`/`calt` rules. Programming fonts
//! implement ligatures as one glyph per source character, so the grid stays
//! intact; a glyph that covers several characters is drawn once and the cells
//! it covers are left empty.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use unicode_width::UnicodeWidthChar;

pub const REGULAR_TTF: &[u8] = include_bytes!("../../assets/fonts/Iosevka-Regular.ttf");
pub const BOLD_TTF: &[u8] = include_bytes!("../../assets/fonts/Iosevka-Bold.ttf");
pub const ITALIC_TTF: &[u8] = include_bytes!("../../assets/fonts/Iosevka-Italic.ttf");
pub const BOLD_ITALIC_TTF: &[u8] = include_bytes!("../../assets/fonts/Iosevka-BoldItalic.ttf");
const SYMBOLS_TTF: &[u8] = include_bytes!("../../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

/// Characters that may take part in a ligature. Letters and digits never do
/// in programming fonts, which keeps shaping rare and cheap.
pub const LIGATURE_CHARS: &str = "!#$%&*+-./:<=>?@\\^|~";

pub struct Glyph {
    pub width: usize,
    pub height: usize,
    /// Horizontal offset from the pen position to the bitmap's left edge.
    pub xmin: i32,
    /// Vertical offset from the baseline to the bitmap's bottom edge (positive = up).
    pub ymin: i32,
    pub mask: Vec<u8>,
}

/// A glyph of one of the faces, as produced by `locate` or by shaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphId {
    face: u8,
    index: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    glyph: GlyphId,
    /// Pixel size times ten, so that scaled sizes get distinct cache entries.
    size_tenths: u32,
    /// Fake bold (no bold face available).
    embolden: bool,
    /// Fake italic (no italic face available).
    shear: bool,
}

/// One glyph produced by shaping a run: which source character it starts at
/// and how many characters it covers.
#[derive(Debug, Clone, Copy)]
pub struct ShapedGlyph {
    pub glyph: GlyphId,
    pub char_offset: usize,
    pub chars: usize,
}

struct Face {
    font: fontdue::Font,
    shaper: rustybuzz::Face<'static>,
}

impl Face {
    fn load(bytes: &'static [u8], size: f32) -> Result<Face, String> {
        let settings = fontdue::FontSettings { scale: size, ..fontdue::FontSettings::default() };
        let font = fontdue::Font::from_bytes(bytes, settings)?;
        let shaper = rustybuzz::Face::from_slice(bytes, 0).ok_or("not a usable font")?;
        Ok(Face { font, shaper })
    }
}

/// Face numbers used in `GlyphId`.
const REGULAR: u8 = 0;
const BOLD: u8 = 1;
const ITALIC: u8 = 2;
const BOLD_ITALIC: u8 = 3;
const SYMBOLS: u8 = 4;

/// The font files of one family. Only the regular one is required.
#[derive(Default)]
pub struct FontFiles {
    pub regular: Vec<u8>,
    pub bold: Option<Vec<u8>>,
    pub italic: Option<Vec<u8>>,
    pub bold_italic: Option<Vec<u8>>,
}

pub struct FontSet {
    /// Indexed by the face numbers above; the symbols face is always there.
    faces: [Option<Face>; 5],
    pub ligatures: bool,
    /// Base pixel size.
    pub size: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub ascent: f32,
    cache: HashMap<GlyphKey, Glyph>,
    /// Shaped punctuation runs, keyed by (text, bold, italic).
    shaped: RefCell<HashMap<(String, bool, bool), Vec<ShapedGlyph>>>,
}

impl FontSet {
    /// Load a font set from font files. The bytes are kept for the program's
    /// lifetime (fonts are loaded a handful of times at most).
    pub fn load(files: FontFiles, size: f32) -> Result<FontSet, String> {
        let optional = |bytes: Option<Vec<u8>>| -> Result<Option<Face>, String> {
            match bytes {
                Some(bytes) => Ok(Some(Face::load(bytes.leak(), size)?)),
                None => Ok(None),
            }
        };
        let faces = [
            Some(Face::load(files.regular.leak(), size)?),
            optional(files.bold)?,
            optional(files.italic)?,
            optional(files.bold_italic)?,
            Some(Face::load(SYMBOLS_TTF, size)?),
        ];
        let mut set = FontSet {
            faces,
            ligatures: true,
            size,
            cell_w: 0.0,
            cell_h: 0.0,
            ascent: 0.0,
            cache: HashMap::new(),
            shaped: RefCell::new(HashMap::new()),
        };
        set.set_size(size);
        Ok(set)
    }

    /// The embedded Iosevka font, all four faces.
    pub fn builtin(size: f32) -> FontSet {
        let files = FontFiles {
            regular: REGULAR_TTF.to_vec(),
            bold: Some(BOLD_TTF.to_vec()),
            italic: Some(ITALIC_TTF.to_vec()),
            bold_italic: Some(BOLD_ITALIC_TTF.to_vec()),
        };
        FontSet::load(files, size).expect("built-in font is valid")
    }

    fn face(&self, id: u8) -> &Face {
        self.faces[id as usize].as_ref().or(self.faces[REGULAR as usize].as_ref()).expect("the regular face is always loaded")
    }

    fn has_face(&self, id: u8) -> bool {
        self.faces[id as usize].is_some()
    }

    pub fn set_size(&mut self, size: f32) {
        self.size = size.clamp(5.0, 72.0);
        let (ascent, cell_h, cell_w) = {
            let regular = &self.face(REGULAR).font;
            let metrics = regular.horizontal_line_metrics(self.size).expect("horizontal font");
            let height = (metrics.ascent - metrics.descent + metrics.line_gap).ceil().max(1.0);
            (metrics.ascent.ceil(), height, regular.metrics('M', self.size).advance_width.ceil().max(1.0))
        };
        self.ascent = ascent;
        self.cell_h = cell_h;
        self.cell_w = cell_w;
        self.cache.clear();
    }

    /// Cell width and line height at a given scale.
    pub fn scaled_metrics(&self, scale: f32) -> (f32, f32, f32) {
        if scale == 1.0 {
            return (self.cell_w, self.cell_h, self.ascent);
        }
        let size = self.size * scale;
        let regular = &self.face(REGULAR).font;
        let metrics = regular.horizontal_line_metrics(size).expect("horizontal font");
        let ascent = metrics.ascent.ceil();
        let height = (metrics.ascent - metrics.descent + metrics.line_gap).ceil();
        let width = regular.metrics('M', size).advance_width.ceil();
        (width, height, ascent)
    }

    /// Number of cells a character occupies (0 for combining marks, 2 for CJK).
    pub fn char_cells(ch: char) -> usize {
        ch.width().unwrap_or(0)
    }

    /// The faces to try for a style, best first; the symbols face closes the list.
    fn preference(bold: bool, italic: bool) -> &'static [u8] {
        match (bold, italic) {
            (true, true) => &[BOLD_ITALIC, ITALIC, BOLD, REGULAR, SYMBOLS],
            (true, false) => &[BOLD, REGULAR, SYMBOLS],
            (false, true) => &[ITALIC, REGULAR, SYMBOLS],
            (false, false) => &[REGULAR, SYMBOLS],
        }
    }

    /// Which face draws `ch` in a style: the first face of the preference
    /// that is loaded and has the character.
    fn locate(&self, ch: char, bold: bool, italic: bool) -> GlyphId {
        for &face in FontSet::preference(bold, italic) {
            if self.has_face(face) {
                let index = self.face(face).font.lookup_glyph_index(ch);
                if index != 0 {
                    return GlyphId { face, index };
                }
            }
        }
        GlyphId { face: REGULAR, index: self.face(REGULAR).font.lookup_glyph_index('\u{FFFD}') }
    }

    /// Rasterized glyph for `ch`, from the cache when possible. Bold and
    /// italic come from the matching faces, or are faked when those are missing.
    pub fn glyph(&mut self, ch: char, bold: bool, italic: bool, scale: f32) -> &Glyph {
        let glyph = self.locate(ch, bold, italic);
        self.glyph_by_id(glyph, bold, italic, scale)
    }

    /// Rasterized glyph by id (from shaping), in a style.
    pub fn glyph_by_id(&mut self, glyph: GlyphId, bold: bool, italic: bool, scale: f32) -> &Glyph {
        let size = self.size * scale;
        let embolden = bold && !matches!(glyph.face, BOLD | BOLD_ITALIC);
        let shear = italic && !matches!(glyph.face, ITALIC | BOLD_ITALIC);
        let key = GlyphKey { glyph, size_tenths: (size * 10.0) as u32, embolden, shear };
        if !self.cache.contains_key(&key) {
            let rasterized = self.rasterize(glyph, embolden, shear, size);
            self.cache.insert(key, rasterized);
        }
        &self.cache[&key]
    }

    fn rasterize(&self, glyph: GlyphId, embolden: bool, shear: bool, size: f32) -> Glyph {
        let font = &self.face(glyph.face).font;
        let (metrics, mut mask) = font.rasterize_indexed(glyph.index, size);
        if embolden {
            smear_right(&mut mask, metrics.width, metrics.height);
        }
        let glyph = Glyph { width: metrics.width, height: metrics.height, xmin: metrics.xmin, ymin: metrics.ymin, mask };
        if shear { shear_right(glyph) } else { glyph }
    }

    /// Whether the regular font has `ch` at all (used to skip shaping).
    fn in_main_font(&self, ch: char) -> bool {
        self.face(REGULAR).font.lookup_glyph_index(ch) != 0
    }

    /// Shape a run of ligature characters. Returns `None` when the font makes
    /// no ligature out of it, so the caller draws plain characters.
    pub fn shape(&self, run: &str, bold: bool, italic: bool) -> Option<Vec<ShapedGlyph>> {
        if !self.ligatures || run.chars().count() < 2 || !run.chars().all(|c| self.in_main_font(c)) {
            return None;
        }
        let key = (run.to_string(), bold, italic);
        if let Some(cached) = self.shaped.borrow().get(&key) {
            return if cached.is_empty() { None } else { Some(cached.clone()) };
        }
        let face_index = FontSet::preference(bold, italic).iter().copied().find(|f| *f != SYMBOLS && self.has_face(*f)).unwrap_or(REGULAR);
        let shaped = shape_run(self.face(face_index), face_index, run);
        let result = if shaped.is_empty() { None } else { Some(shaped.clone()) };
        self.shaped.borrow_mut().insert(key, shaped);
        result
    }
}

/// Run the shaper on `run` and turn its output into per-character glyphs.
/// An empty result means "nothing changed, draw the characters as they are".
fn shape_run(face: &Face, face_index: u8, run: &str) -> Vec<ShapedGlyph> {
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(run);
    let output = rustybuzz::shape(&face.shaper, &[], buffer);
    let infos = output.glyph_infos();
    let byte_to_char: HashMap<usize, usize> = run.char_indices().enumerate().map(|(i, (b, _))| (b, i)).collect();
    let char_count = run.chars().count();
    let mut glyphs = Vec::with_capacity(infos.len());
    for (i, info) in infos.iter().enumerate() {
        let Some(&char_offset) = byte_to_char.get(&(info.cluster as usize)) else { return Vec::new() };
        let next_offset = infos.get(i + 1).and_then(|n| byte_to_char.get(&(n.cluster as usize)).copied()).unwrap_or(char_count);
        if next_offset <= char_offset || info.glyph_id > u16::MAX as u32 {
            return Vec::new();
        }
        glyphs.push(ShapedGlyph {
            glyph: GlyphId { face: face_index, index: info.glyph_id as u16 },
            char_offset,
            chars: next_offset - char_offset,
        });
    }
    // Same glyphs as the plain characters: no ligature here.
    let plain: Vec<u16> = run.chars().map(|c| face.font.lookup_glyph_index(c)).collect();
    let unchanged = glyphs.len() == plain.len() && glyphs.iter().zip(&plain).all(|(g, p)| g.glyph.index == *p);
    if unchanged { Vec::new() } else { glyphs }
}

/// Fake bold: smear every pixel one step to the right.
fn smear_right(mask: &mut [u8], width: usize, height: usize) {
    for row in 0..height {
        let line = &mut mask[row * width..(row + 1) * width];
        for x in (1..width).rev() {
            line[x] = line[x].max(line[x - 1]);
        }
    }
}

/// Fake italic: slant the bitmap to the right by a fifth of its height. Each
/// row moves by a fractional amount, spread over two pixels, so the slant is
/// smooth instead of stepping a whole pixel every few rows.
fn shear_right(glyph: Glyph) -> Glyph {
    const SLANT: f32 = 0.2;
    let extra = (glyph.height as f32 * SLANT).ceil() as usize + 1;
    let width = glyph.width + extra;
    let mut mask = vec![0u8; width * glyph.height];
    for row in 0..glyph.height {
        let shift = (glyph.height - row) as f32 * SLANT;
        let whole = shift.floor() as usize;
        let frac = shift - whole as f32;
        for x in 0..glyph.width {
            let value = glyph.mask[row * glyph.width + x] as f32;
            if value == 0.0 {
                continue;
            }
            let i = row * width + x + whole;
            mask[i] = (mask[i] as f32 + value * (1.0 - frac)).min(255.0) as u8;
            mask[i + 1] = (mask[i + 1] as f32 + value * frac).min(255.0) as u8;
        }
    }
    Glyph { width, height: glyph.height, xmin: glyph.xmin, ymin: glyph.ymin, mask }
}

// ---------------------------------------------------------------------------
// Finding fonts on the system
// ---------------------------------------------------------------------------

/// Directories where fonts usually live.
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(home) = &home {
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
        dirs.push(home.join("Library/Fonts"));
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(data).join("fonts"));
    }
    dirs.push(PathBuf::from("/usr/share/fonts"));
    dirs.push(PathBuf::from("/usr/local/share/fonts"));
    dirs.push(PathBuf::from("/Library/Fonts"));
    dirs.push(PathBuf::from("/System/Library/Fonts"));
    if let Some(windir) = std::env::var_os("WINDIR") {
        dirs.push(PathBuf::from(windir).join("Fonts"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Microsoft/Windows/Fonts"));
    }
    dirs
}

/// The files of a font family found on the system.
pub struct FamilyFiles {
    pub regular: PathBuf,
    pub bold: Option<PathBuf>,
    pub italic: Option<PathBuf>,
    pub bold_italic: Option<PathBuf>,
}

/// Find a font family by name on the system. Names are compared without
/// spaces or case, so `"JetBrainsMono Nerd Font"` finds
/// `JetBrainsMonoNerdFont-Regular.ttf` and its bold and italic siblings.
pub fn find_font_family(family: &str) -> Option<FamilyFiles> {
    let wanted = normalize(family);
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in font_dirs() {
        collect_font_files(&dir, &mut files, 0);
    }
    let stem = |p: &Path| p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let find = |weight: &str| {
        files
            .iter()
            .find(|p| {
                let s = normalize(&stem(p));
                s == format!("{wanted}-{weight}") || s == format!("{wanted}mono-{weight}") || s == format!("{wanted}{weight}")
            })
            .cloned()
    };
    Some(FamilyFiles { regular: find("regular")?, bold: find("bold"), italic: find("italic"), bold_italic: find("bolditalic") })
}

fn normalize(name: &str) -> String {
    name.chars().filter(|c| !c.is_whitespace() && *c != '_').flat_map(char::to_lowercase).collect()
}

fn collect_font_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < 3 {
                collect_font_files(&path, out, depth + 1);
            }
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))
        {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_builtin_font_and_rasterizes() {
        let mut font = FontSet::builtin(14.0);
        assert!(font.cell_w > 5.0 && font.cell_h > 10.0);
        let glyph = font.glyph('A', false, false, 1.0);
        assert!(glyph.width > 0 && glyph.height > 0);
        assert!(glyph.mask.iter().any(|&c| c > 0));
        assert_eq!(FontSet::char_cells('a'), 1);
        assert_eq!(FontSet::char_cells('漢'), 2);
    }

    #[test]
    fn symbols_come_from_the_fallback_face() {
        let mut font = FontSet::builtin(14.0);
        let id = font.locate('\u{f07b}', false, false); // folder icon
        assert_eq!(id.face, SYMBOLS);
        let glyph = font.glyph('\u{f07b}', false, false, 1.0);
        assert!(glyph.mask.iter().any(|&c| c > 0));
        let missing = font.locate('\u{10ffff}', false, false);
        assert_eq!(missing.face, REGULAR, "unknown characters draw the replacement glyph");
    }

    #[test]
    fn iosevka_shapes_ligatures() {
        let mut font = FontSet::builtin(14.0);
        let shaped = font.shape("->", false, false).expect("Iosevka ligates ->");
        assert_eq!(shaped.iter().map(|g| g.chars).sum::<usize>(), 2, "the ligature covers both characters");
        assert!(font.shape("x", false, false).is_none(), "single characters are not shaped");
        assert!(font.glyph('é', false, false, 1.0).mask.iter().any(|&c| c > 0));
        assert!(font.glyph('│', false, false, 1.0).mask.iter().any(|&c| c > 0), "box drawing survived the subset");
        assert!(font.glyph('\u{e0a0}', false, false, 1.0).mask.iter().any(|&c| c > 0), "powerline too");
    }

    #[test]
    fn italic_comes_from_a_real_face_or_a_smooth_shear() {
        let mut font = FontSet::builtin(14.0);
        assert_eq!(font.locate('a', false, true).face, ITALIC);
        assert_eq!(font.locate('a', true, true).face, BOLD_ITALIC);
        assert_eq!(font.locate('\u{f07b}', false, true).face, SYMBOLS, "icons have no italic and are not sheared");
        let upright = font.glyph('l', false, false, 1.0).width;
        // A font without italic faces gets a sheared glyph, wider than the upright one.
        let mut plain = FontSet::load(FontFiles { regular: REGULAR_TTF.to_vec(), ..FontFiles::default() }, 14.0).unwrap();
        assert_eq!(plain.locate('a', false, true).face, REGULAR);
        let sheared = plain.glyph('l', false, true, 1.0);
        assert!(sheared.width > upright);
        let top_row = &sheared.mask[..sheared.width];
        let partial = top_row.iter().any(|&v| v > 0 && v < 255);
        assert!(partial, "the shear is sub-pixel: edge pixels are partially covered");
    }
}
