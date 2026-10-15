//! Font loading, a glyph cache and ligature shaping on top of `fontdue` and
//! `rustybuzz`.
//!
//! The editor is monospace: every character occupies a whole number of cells
//! (`cell_w` wide, `cell_h` tall). A font set has a regular face, an optional
//! bold face and fallback faces for characters the main font lacks (the
//! embedded Nerd Font symbols, so prompts and file icons always render).
//! Italic is synthesized by shearing. Bigger text (markdown headings) is simply
//! rasterized at a larger pixel size.
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

pub const REGULAR_TTF: &[u8] = include_bytes!("../../assets/fonts/Hack-Regular.ttf");
pub const BOLD_TTF: &[u8] = include_bytes!("../../assets/fonts/Hack-Bold.ttf");
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

const REGULAR: u8 = 0;
const BOLD: u8 = 1;
const FALLBACK: u8 = 2;

pub struct FontSet {
    /// Regular, bold (when loaded), then fallbacks.
    faces: Vec<Face>,
    has_bold: bool,
    pub ligatures: bool,
    /// Base pixel size.
    pub size: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub ascent: f32,
    cache: HashMap<GlyphKey, Glyph>,
    /// Shaped punctuation runs, keyed by (text, bold).
    shaped: RefCell<HashMap<(String, bool), Vec<ShapedGlyph>>>,
}

impl FontSet {
    /// Load a font set from font files; `bold` is optional. The bytes are kept
    /// for the program's lifetime (fonts are loaded a handful of times at most).
    pub fn load(regular: Vec<u8>, bold: Option<Vec<u8>>, size: f32) -> Result<FontSet, String> {
        let mut faces = vec![Face::load(regular.leak(), size)?];
        let mut has_bold = false;
        if let Some(bold) = bold {
            faces.push(Face::load(bold.leak(), size)?);
            has_bold = true;
        }
        faces.push(Face::load(SYMBOLS_TTF, size)?);
        let mut set = FontSet {
            faces,
            has_bold,
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

    /// The embedded Hack font.
    pub fn builtin(size: f32) -> FontSet {
        FontSet::load(REGULAR_TTF.to_vec(), Some(BOLD_TTF.to_vec()), size).expect("built-in font is valid")
    }

    pub fn set_size(&mut self, size: f32) {
        self.size = size.clamp(5.0, 72.0);
        let regular = &self.faces[REGULAR as usize].font;
        let metrics = regular.horizontal_line_metrics(self.size).expect("horizontal font");
        self.ascent = metrics.ascent.ceil();
        self.cell_h = (metrics.ascent - metrics.descent + metrics.line_gap).ceil().max(1.0);
        self.cell_w = regular.metrics('M', self.size).advance_width.ceil().max(1.0);
        self.cache.clear();
    }

    /// Cell width and line height at a given scale.
    pub fn scaled_metrics(&self, scale: f32) -> (f32, f32, f32) {
        if scale == 1.0 {
            return (self.cell_w, self.cell_h, self.ascent);
        }
        let size = self.size * scale;
        let regular = &self.faces[REGULAR as usize].font;
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

    /// Which face draws `ch`: the bold face when asked and available, else the
    /// regular one, else the first fallback that has the character.
    fn locate(&self, ch: char, bold: bool) -> (GlyphId, bool) {
        let order: &[u8] = if bold && self.has_bold { &[BOLD, REGULAR, FALLBACK] } else { &[REGULAR, FALLBACK] };
        for &face in order {
            if let Some(face_data) = self.faces.get(face as usize) {
                let index = face_data.font.lookup_glyph_index(ch);
                if index != 0 {
                    let embolden = bold && face != BOLD;
                    return (GlyphId { face, index }, embolden);
                }
            }
        }
        let index = self.faces[REGULAR as usize].font.lookup_glyph_index('\u{FFFD}');
        (GlyphId { face: REGULAR, index }, bold)
    }

    /// Rasterized glyph for `ch`, from the cache when possible.
    pub fn glyph(&mut self, ch: char, bold: bool, scale: f32) -> &Glyph {
        let (glyph, embolden) = self.locate(ch, bold);
        self.glyph_by_id(glyph, embolden, scale)
    }

    /// Rasterized glyph by id (from shaping); `embolden` fakes bold.
    pub fn glyph_by_id(&mut self, glyph: GlyphId, embolden: bool, scale: f32) -> &Glyph {
        let size = self.size * scale;
        let key = GlyphKey { glyph, size_tenths: (size * 10.0) as u32, embolden };
        if !self.cache.contains_key(&key) {
            let rasterized = self.rasterize(glyph, embolden, size);
            self.cache.insert(key, rasterized);
        }
        &self.cache[&key]
    }

    fn rasterize(&self, glyph: GlyphId, embolden: bool, size: f32) -> Glyph {
        let font = &self.faces[glyph.face as usize].font;
        let (metrics, mut mask) = font.rasterize_indexed(glyph.index, size);
        if embolden {
            smear_right(&mut mask, metrics.width, metrics.height);
        }
        Glyph { width: metrics.width, height: metrics.height, xmin: metrics.xmin, ymin: metrics.ymin, mask }
    }

    /// True when bold must be faked because no bold face is loaded.
    pub fn fake_bold(&self) -> bool {
        !self.has_bold
    }

    /// Whether the regular font has `ch` at all (used to skip shaping).
    fn in_main_font(&self, ch: char) -> bool {
        self.faces[REGULAR as usize].font.lookup_glyph_index(ch) != 0
    }

    /// Shape a run of ligature characters. Returns `None` when the font makes
    /// no ligature out of it, so the caller draws plain characters.
    pub fn shape(&self, run: &str, bold: bool) -> Option<Vec<ShapedGlyph>> {
        if !self.ligatures || run.chars().count() < 2 || !run.chars().all(|c| self.in_main_font(c)) {
            return None;
        }
        let key = (run.to_string(), bold);
        if let Some(cached) = self.shaped.borrow().get(&key) {
            return if cached.is_empty() { None } else { Some(cached.clone()) };
        }
        let face_index = if bold && self.has_bold { BOLD } else { REGULAR };
        let face = &self.faces[face_index as usize];
        let shaped = shape_run(face, face_index, run);
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

/// Nerd Font families to look for, best first. The file names Nerd Fonts
/// ship are `<Family>NerdFont[Mono|Propo]-<Weight>.ttf`.
const PREFERRED_FAMILIES: &[&str] =
    &["IosevkaNerdFont", "IosevkaTermNerdFont", "JetBrainsMonoNerdFont", "FiraCodeNerdFont", "HackNerdFont"];

/// Find a font family by name on the system: (regular file, bold file if any).
/// Names are compared without spaces or case, so `"JetBrainsMono Nerd Font"`
/// finds `JetBrainsMonoNerdFont-Regular.ttf`.
pub fn find_font_family(family: &str) -> Option<(PathBuf, Option<PathBuf>)> {
    let wanted = normalize(family);
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in font_dirs() {
        collect_font_files(&dir, &mut files, 0);
    }
    let stem = |p: &Path| p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let matches = |p: &Path, weight: &str| {
        let s = normalize(&stem(p));
        s == format!("{wanted}-{weight}") || s == format!("{wanted}mono-{weight}") || s == format!("{wanted}{weight}")
    };
    let regular = files.iter().find(|p| matches(p, "regular"))?.clone();
    let bold = files.iter().find(|p| matches(p, "bold")).cloned();
    Some((regular, bold))
}

fn normalize(name: &str) -> String {
    name.chars().filter(|c| !c.is_whitespace() && *c != '_').flat_map(char::to_lowercase).collect()
}

/// Find a Nerd Font on the system: (regular file, bold file if any).
pub fn find_system_nerd_font() -> Option<(PathBuf, Option<PathBuf>)> {
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in font_dirs() {
        collect_font_files(&dir, &mut files, 0);
    }
    let stem = |p: &Path| p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let mut families: Vec<&str> = PREFERRED_FAMILIES.to_vec();
    // Any other Nerd Font is better than none.
    let others: Vec<String> = files
        .iter()
        .map(|p| stem(p))
        .filter(|s| s.contains("NerdFont") && s.ends_with("-Regular"))
        .map(|s| s.trim_end_matches("-Regular").trim_end_matches("Mono").trim_end_matches("Propo").to_string())
        .collect();
    families.extend(others.iter().map(|s| s.as_str()));
    for family in families {
        let regular_name = |s: &str| s == format!("{family}-Regular") || s == format!("{family}Mono-Regular");
        let bold_name = |s: &str| s == format!("{family}-Bold") || s == format!("{family}Mono-Bold");
        if let Some(regular) = files.iter().find(|p| regular_name(&stem(p))) {
            let bold = files.iter().find(|p| bold_name(&stem(p))).cloned();
            return Some((regular.clone(), bold));
        }
    }
    None
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
        let glyph = font.glyph('A', false, 1.0);
        assert!(glyph.width > 0 && glyph.height > 0);
        assert!(glyph.mask.iter().any(|&c| c > 0));
        assert_eq!(FontSet::char_cells('a'), 1);
        assert_eq!(FontSet::char_cells('漢'), 2);
    }

    #[test]
    fn symbols_come_from_the_fallback_face() {
        let mut font = FontSet::builtin(14.0);
        let (id, _) = font.locate('\u{f07b}', false); // folder icon
        assert_eq!(id.face, FALLBACK);
        let glyph = font.glyph('\u{f07b}', false, 1.0);
        assert!(glyph.mask.iter().any(|&c| c > 0));
        let (missing, _) = font.locate('\u{10ffff}', false);
        assert_eq!(missing.face, REGULAR, "unknown characters draw the replacement glyph");
    }

    #[test]
    fn hack_has_no_ligatures_so_shaping_is_skipped() {
        let font = FontSet::builtin(14.0);
        assert!(font.shape("->", false).is_none());
        assert!(font.shape("x", false).is_none());
    }
}
