//! Themes map scope names to colors.
//!
//! A theme file has one entry per line: `scope #rrggbb [bold] [italic]
//! [underline]`. A scope like `heading.1` falls back to `heading`, then to the
//! default text color. UI colors use the `ui.` prefix. Text after the color is
//! free-form, so trailing comments are fine.

use std::collections::HashMap;

use crate::render::Color;
use crate::syntax::{Registry, ScopeId, StyleAttrs};

/// Fully resolved look of a piece of text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub fg: Color,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub conceal: bool,
    /// Font scale factor (1.0 = normal).
    pub scale: f32,
}

impl Style {
    pub fn plain(fg: Color) -> Style {
        Style { fg, bg: None, bold: false, italic: false, underline: false, strike: false, conceal: false, scale: 1.0 }
    }
}

#[derive(Debug, Clone)]
pub struct ThemeEntry {
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, Clone)]
pub struct Theme {
    #[allow(dead_code)]
    pub name: String,
    entries: HashMap<String, ThemeEntry>,
}

const BUILTIN: &[(&str, &str)] =
    &[("dark", include_str!("../assets/themes/dark.theme")), ("light", include_str!("../assets/themes/light.theme"))];

/// The iTerm2-Color-Schemes collection (what Ghostty ships), one `[name]`
/// section per scheme in Ghostty's `key = value` format.
const TERMINAL_SCHEMES: &str = include_str!("../assets/themes/terminal-schemes.txt");

impl Theme {
    /// `dark`, `light`, then every bundled terminal scheme.
    pub fn builtin_names() -> Vec<&'static str> {
        let mut names: Vec<&'static str> = BUILTIN.iter().map(|(n, _)| *n).collect();
        names.extend(TERMINAL_SCHEMES.lines().filter_map(|l| l.strip_prefix('[')?.strip_suffix(']')));
        names
    }

    pub fn builtin(name: &str) -> Option<Theme> {
        if let Some((n, src)) = BUILTIN.iter().find(|(n, _)| *n == name) {
            return Some(Theme::parse(n, src));
        }
        let header = format!("[{name}]");
        let start = TERMINAL_SCHEMES.lines().position(|l| l == header)?;
        let body: Vec<&str> = TERMINAL_SCHEMES.lines().skip(start + 1).take_while(|l| !l.starts_with('[')).collect();
        Some(Theme::from_terminal_scheme(name, &body.join("\n")))
    }

    /// Build a full editor theme out of a terminal scheme: 16 palette colors,
    /// background, foreground, cursor and selection.
    pub fn from_terminal_scheme(name: &str, source: &str) -> Theme {
        let mut palette = [Color::WHITE; 16];
        let (mut bg, mut fg, mut cursor, mut selection) = (Color::BLACK, Color::WHITE, None, None);
        for line in source.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "palette" => {
                    if let Some((index, hex)) = value.split_once('=')
                        && let (Ok(i), Some(color)) = (index.trim().parse::<usize>(), Color::from_hex(hex.trim().trim_start_matches('#')))
                        && i < 16
                    {
                        palette[i] = color;
                    }
                }
                "background" => bg = Color::from_hex(value.trim_start_matches('#')).unwrap_or(bg),
                "foreground" => fg = Color::from_hex(value.trim_start_matches('#')).unwrap_or(fg),
                "cursor-color" => cursor = Color::from_hex(value.trim_start_matches('#')),
                "selection-background" => selection = Color::from_hex(value.trim_start_matches('#')),
                _ => {}
            }
        }
        let p = |i: usize| palette[i];
        let dark = bg.is_dark();
        // Shades between background and foreground, and a bar color a step
        // away from the background (darker on dark themes, lighter on light).
        let shade = |t: f32| bg.mix(fg, t);
        let bar = if dark { bg.mix(Color::BLACK, 0.25) } else { bg.mix(Color::BLACK, 0.06) };
        let border = if dark { bg.mix(Color::BLACK, 0.45) } else { bg.mix(Color::BLACK, 0.15) };
        let selection = selection.filter(|s| s.luma().abs_diff(bg.luma()) > 12).unwrap_or_else(|| shade(0.25));
        let cursor = cursor.unwrap_or_else(|| p(12));
        // Comments use "bright black" when it stands out from the background.
        let comment = if p(8).luma().abs_diff(bg.luma()) > 40 { p(8) } else { shade(0.5) };
        let dim = shade(0.5);
        let hex = |c: Color| format!("#{:06x}", c.to_u32());
        let entries: Vec<(&str, Color, &str)> = vec![
            ("ui.background", bg, ""),
            ("ui.text", fg, ""),
            ("ui.cursor", cursor, ""),
            ("ui.cursor.insert", p(2), ""),
            ("ui.cursor.secondary", shade(0.35), ""),
            ("ui.cursor.inactive", shade(0.25), ""),
            ("ui.selection", selection, ""),
            ("ui.selection.primary", selection.mix(fg, 0.1), ""),
            ("ui.cursorline", shade(0.07), ""),
            ("ui.linenr", shade(0.3), ""),
            ("ui.linenr.current", fg, ""),
            ("ui.gutter", bg, ""),
            ("ui.separator", border, ""),
            ("ui.statusline", bar, ""),
            ("ui.statusline.text", fg, ""),
            ("ui.statusline.mode", bg, ""),
            ("ui.mode.normal", p(4), ""),
            ("ui.mode.insert", p(2), ""),
            ("ui.mode.replace", p(1), ""),
            ("ui.mode.select", p(5), ""),
            ("ui.mode.terminal", p(3), ""),
            ("ui.tabbar", bar, ""),
            ("ui.tab", dim, ""),
            ("ui.tab.active", fg, ""),
            ("ui.tab.active.bg", bg, ""),
            ("ui.popup", bar, ""),
            ("ui.popup.border", border, ""),
            ("ui.popup.selected", shade(0.12), ""),
            ("ui.popup.match", p(3), ""),
            ("ui.prompt", p(4), ""),
            ("ui.help", dim, ""),
            ("ui.error", p(1), ""),
            ("ui.warning", p(3), ""),
            ("ui.info", p(6), ""),
            ("ui.match.bracket", selection, ""),
            ("ui.search.match", bg.mix(p(3), 0.3), ""),
            ("ui.whitespace", shade(0.15), ""),
            ("ui.dim", dim, ""),
            ("comment", comment, "italic"),
            ("comment.doc", comment, "italic"),
            ("comment.todo", p(3), "bold"),
            ("keyword", p(5), ""),
            ("operator", p(6), ""),
            ("type", p(3), ""),
            ("function", p(4), ""),
            ("function.builtin", p(6), ""),
            ("constant", p(1), ""),
            ("number", p(1), ""),
            ("string", p(2), ""),
            ("string.escape", p(6), ""),
            ("string.special", p(6), ""),
            ("char", p(2), ""),
            ("regex", p(6), ""),
            ("attribute", p(3), ""),
            ("annotation", p(3), ""),
            ("variable", fg, ""),
            ("variable.builtin", p(1), ""),
            ("property", p(6), ""),
            ("namespace", p(3), ""),
            ("label", p(1), ""),
            ("tag", p(1), ""),
            ("punctuation", fg, ""),
            ("punctuation.special", p(6), ""),
            ("preprocessor", p(5), ""),
            ("special", p(6), ""),
            ("conceal", shade(0.3), ""),
            ("heading", p(1), ""),
            ("heading.2", p(3), ""),
            ("heading.3", p(2), ""),
            ("heading.4", p(6), ""),
            ("heading.5", p(4), ""),
            ("heading.6", p(5), ""),
            ("strong", p(3), ""),
            ("emphasis", p(5), ""),
            ("strike", dim, ""),
            ("code", p(2), ""),
            ("code.background", shade(0.07), ""),
            ("code.block", fg, ""),
            ("link", p(4), "underline"),
            ("link.url", p(6), "underline"),
            ("link.text", p(4), ""),
            ("list.marker", p(4), ""),
            ("quote", dim, "italic"),
            ("rule", shade(0.3), ""),
            ("table", fg, ""),
            ("diff.add", p(2), ""),
            ("diff.remove", p(1), ""),
            ("diff.header", p(4), "bold"),
            ("diff.hunk", p(6), ""),
            ("diff.modified", p(3), ""),
            ("directory", p(4), "bold"),
            ("tree.hidden", dim, ""),
            ("tree.modified", p(3), ""),
            ("tree.new", p(2), ""),
            ("ui.git", p(6), ""),
            ("ui.git.sync", p(3), ""),
        ];
        let mut source = String::new();
        for (scope, color, attrs) in entries {
            source.push_str(&format!("{scope} {} {attrs}\n", hex(color)));
        }
        const TERM: [&str; 16] = [
            "black",
            "red",
            "green",
            "yellow",
            "blue",
            "magenta",
            "cyan",
            "white",
            "bright_black",
            "bright_red",
            "bright_green",
            "bright_yellow",
            "bright_blue",
            "bright_magenta",
            "bright_cyan",
            "bright_white",
        ];
        for (i, term) in TERM.iter().enumerate() {
            source.push_str(&format!("term.{term} {}\n", hex(p(i))));
        }
        Theme::parse(name, &source)
    }

    pub fn parse(name: &str, source: &str) -> Theme {
        let mut entries = HashMap::new();
        for raw in source.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut words = line.split_whitespace();
            let Some(scope) = words.next() else { continue };
            let mut entry: Option<ThemeEntry> = None;
            for word in words {
                if let Some(hex) = word.strip_prefix('#') {
                    match Color::from_hex(hex) {
                        Some(color) => entry = Some(ThemeEntry { color, bold: false, italic: false, underline: false }),
                        // Anything else after `#` is a trailing comment.
                        None => break,
                    }
                } else if let Some(e) = &mut entry {
                    match word {
                        "bold" => e.bold = true,
                        "italic" => e.italic = true,
                        "underline" => e.underline = true,
                        _ => {}
                    }
                }
            }
            if let Some(entry) = entry {
                entries.insert(scope.to_string(), entry);
            }
        }
        Theme { name: name.to_string(), entries }
    }

    /// Look up a scope, falling back to its dotted parents.
    pub fn entry(&self, scope: &str) -> Option<&ThemeEntry> {
        let mut key = scope;
        loop {
            if let Some(entry) = self.entries.get(key) {
                return Some(entry);
            }
            {
                let i = key.rfind('.')?;
                key = &key[..i]
            }
        }
    }

    pub fn color(&self, scope: &str) -> Color {
        match self.entry(scope) {
            Some(entry) => entry.color,
            None => self.ui("ui.text", Color::WHITE),
        }
    }

    /// Color of a UI element, or a fallback if the theme does not define it.
    pub fn ui(&self, scope: &str, fallback: Color) -> Color {
        self.entry(scope).map(|e| e.color).unwrap_or(fallback)
    }

    /// Resolve the style of every scope known to the registry, combining the
    /// theme's colors with the render attributes from the syntax files.
    pub fn style_table(&self, registry: &Registry) -> Vec<Style> {
        let text = self.color("ui.text");
        let mut table: Vec<Style> = (0..registry.scopes.len())
            .map(|i| {
                let name = registry.scopes.name(ScopeId(i as u16));
                let mut style = Style::plain(text);
                if let Some(entry) = self.entry(name) {
                    style.fg = entry.color;
                    style.bold = entry.bold;
                    style.italic = entry.italic;
                    style.underline = entry.underline;
                }
                style
            })
            .collect();
        for lang in &registry.languages {
            for (scope, attrs) in &lang.styles {
                apply_attrs(&mut table[scope.0 as usize], attrs, self);
            }
        }
        table
    }
}

fn apply_attrs(style: &mut Style, attrs: &StyleAttrs, theme: &Theme) {
    style.bold |= attrs.bold;
    style.italic |= attrs.italic;
    style.underline |= attrs.underline;
    style.strike |= attrs.strike;
    style.conceal |= attrs.conceal;
    if let Some(size) = attrs.size {
        style.scale = size;
    }
    if let Some(bg) = &attrs.bg {
        style.bg = Some(theme.color(bg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_terminal_schemes_become_themes() {
        let names = Theme::builtin_names();
        assert!(names.len() > 400);
        assert!(names.contains(&"catppuccin-mocha") && names.contains(&"gruvbox-dark"));
        let theme = Theme::builtin("catppuccin-mocha").unwrap();
        assert_eq!(theme.color("ui.background"), Color::from_hex("1e1e2e").unwrap());
        assert_eq!(theme.color("string"), Color::from_hex("a6e3a1").unwrap(), "strings are palette green");
        assert_eq!(theme.color("term.bright_white"), Color::from_hex("a6adc8").unwrap());
        assert!(theme.entry("link").unwrap().underline);
        for name in names {
            assert!(Theme::builtin(name).is_some(), "{name}");
        }
    }

    #[test]
    fn parses_and_falls_back() {
        let theme = Theme::parse("t", "ui.text #aabbcc\nheading #ff0000 bold # red\n");
        assert_eq!(theme.color("heading.2"), Color::from_hex("ff0000").unwrap());
        assert!(theme.entry("heading.2").unwrap().bold);
        assert_eq!(theme.color("unknown"), Color::from_hex("aabbcc").unwrap());
    }
}
