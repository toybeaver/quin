//! Syntax definitions ("languages") and the registry that holds them.
//!
//! A language is described by a small text file (`*.syn`, see `format.rs` for the
//! grammar and `assets/syntax/` for examples). Highlighting is regex-based and runs
//! line by line, carrying a small state (the stack of open regions) from one line
//! to the next. The same files also say how tokens should be *rendered* (bold,
//! bigger, hidden...), which is what makes e.g. Markdown files look like documents.

pub mod convert;
pub mod format;
pub mod highlight;

use std::collections::HashMap;
use std::path::Path;

use regex::Regex;

/// Index of a language in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LangId(pub usize);

/// Interned scope name (`keyword`, `string`, `heading.1`...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(pub u16);

/// How a token should be drawn, independently of the theme's colors.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyleAttrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Hide the token unless the cursor is on its line (markdown `**` markers...).
    pub conceal: bool,
    /// Font scale relative to the normal text (headings).
    pub size: Option<f32>,
    /// Theme scope whose foreground color is used as this token's background.
    pub bg: Option<String>,
    /// Draw this text instead of the token (e.g. `•` for list markers).
    pub replace: Option<String>,
    /// Lines inside a region with this scope get a bar on their left edge in
    /// the theme color of this scope (quotes, callouts).
    pub bar: Option<String>,
    /// Lines inside a region with this scope are centered in the frame.
    pub center: bool,
    /// Lines inside a region with this scope are rows of a pipe table and are
    /// aligned with the rows around them.
    pub grid: bool,
    /// The token is the path of an image to draw below the line.
    pub image: bool,
}

#[derive(Debug)]
pub enum Embed {
    /// Embed a language by name.
    Language(String),
    /// Embed the language named by a capture group of the start regex.
    Capture(usize),
}

#[derive(Debug)]
pub enum Rule {
    /// A single-line pattern. Capture groups may be given their own scope.
    Match { scope: ScopeId, regex: Regex, captures: Vec<(usize, ScopeId)> },
    /// A possibly multi-line section delimited by two patterns.
    Region {
        scope: ScopeId,
        start: Regex,
        end: Regex,
        start_captures: Vec<(usize, ScopeId)>,
        /// Rules that apply inside the region (indices into `Language::rules`).
        inner: Vec<usize>,
        embed: Option<Embed>,
        /// The end match closes the region but is not part of it: its text is
        /// highlighted again by the rules outside (`keep-end`).
        keep_end: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndentStyle {
    pub width: usize,
    pub use_tabs: bool,
}

#[derive(Debug, Default)]
pub struct Language {
    pub name: String,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub shebangs: Vec<String>,
    pub line_comment: Option<String>,
    pub block_comment: Option<(String, String)>,
    pub indent: Option<IndentStyle>,
    /// Characters that auto-close when typed: `( [ { " '`.
    pub auto_pairs: Vec<(char, char)>,
    /// All rules, flat. Nested rules are referenced by index from their region.
    pub rules: Vec<Rule>,
    /// Indices of the top-level rules, in order.
    pub top: Vec<usize>,
    /// Render attributes per scope.
    pub styles: Vec<(ScopeId, StyleAttrs)>,
}

/// Maps scope names to small integers so highlight spans stay cheap.
#[derive(Debug, Default)]
pub struct Scopes {
    names: Vec<String>,
    ids: HashMap<String, ScopeId>,
}

impl Scopes {
    pub fn intern(&mut self, name: &str) -> ScopeId {
        if let Some(id) = self.ids.get(name) {
            return *id;
        }
        let id = ScopeId(self.names.len() as u16);
        self.names.push(name.to_string());
        self.ids.insert(name.to_string(), id);
        id
    }

    pub fn name(&self, id: ScopeId) -> &str {
        &self.names[id.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }
}

#[derive(Debug, Default)]
pub struct Registry {
    pub languages: Vec<Language>,
    by_name: HashMap<String, LangId>,
    pub scopes: Scopes,
}

/// Syntax files shipped inside the binary.
const BUILTIN: &[(&str, &str)] = &[
    ("rust", include_str!("../../assets/syntax/rust.syn")),
    ("markdown", include_str!("../../assets/syntax/markdown.syn")),
    ("tree", include_str!("../../assets/syntax/tree.syn")),
    ("git-status", include_str!("../../assets/syntax/git-status.syn")),
    ("welcome", include_str!("../../assets/syntax/welcome.syn")),
    ("c", include_str!("../../assets/syntax/c.syn")),
    ("cpp", include_str!("../../assets/syntax/cpp.syn")),
    ("python", include_str!("../../assets/syntax/python.syn")),
    ("javascript", include_str!("../../assets/syntax/javascript.syn")),
    ("typescript", include_str!("../../assets/syntax/typescript.syn")),
    ("go", include_str!("../../assets/syntax/go.syn")),
    ("shell", include_str!("../../assets/syntax/shell.syn")),
    ("toml", include_str!("../../assets/syntax/toml.syn")),
    ("json", include_str!("../../assets/syntax/json.syn")),
    ("yaml", include_str!("../../assets/syntax/yaml.syn")),
    ("html", include_str!("../../assets/syntax/html.syn")),
    ("css", include_str!("../../assets/syntax/css.syn")),
    ("lua", include_str!("../../assets/syntax/lua.syn")),
    ("zig", include_str!("../../assets/syntax/zig.syn")),
    ("java", include_str!("../../assets/syntax/java.syn")),
    ("csharp", include_str!("../../assets/syntax/csharp.syn")),
    ("ruby", include_str!("../../assets/syntax/ruby.syn")),
    ("sql", include_str!("../../assets/syntax/sql.syn")),
    ("makefile", include_str!("../../assets/syntax/makefile.syn")),
    ("diff", include_str!("../../assets/syntax/diff.syn")),
    ("quin", include_str!("../../assets/syntax/quin.syn")),
    ("git-commit", include_str!("../../assets/syntax/git-commit.syn")),
    ("ini", include_str!("../../assets/syntax/ini.syn")),
    ("xml", include_str!("../../assets/syntax/xml.syn")),
    ("haskell", include_str!("../../assets/syntax/haskell.syn")),
    ("ocaml", include_str!("../../assets/syntax/ocaml.syn")),
    ("elixir", include_str!("../../assets/syntax/elixir.syn")),
    ("kotlin", include_str!("../../assets/syntax/kotlin.syn")),
    ("swift", include_str!("../../assets/syntax/swift.syn")),
    ("php", include_str!("../../assets/syntax/php.syn")),
    ("nix", include_str!("../../assets/syntax/nix.syn")),
    ("dockerfile", include_str!("../../assets/syntax/dockerfile.syn")),
    ("cmake", include_str!("../../assets/syntax/cmake.syn")),
    ("latex", include_str!("../../assets/syntax/latex.syn")),
];

impl Registry {
    /// Registry with the built-in languages, plus any `*.syn` found in `user_dir`
    /// (user files replace built-ins with the same name).
    pub fn load(user_dir: Option<&Path>) -> Registry {
        let mut registry = Registry::default();
        for (name, source) in BUILTIN {
            match format::parse(source, &mut registry.scopes) {
                Ok(lang) => registry.add(lang),
                Err(err) => eprintln!("quin: built-in syntax {name}: {err}"),
            }
        }
        if let Some(dir) = user_dir {
            registry.load_dir(dir);
        }
        registry
    }

    pub fn load_dir(&mut self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.extension().and_then(|e| e.to_str()) != Some("syn") {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(source) => match format::parse(&source, &mut self.scopes) {
                    Ok(lang) => self.add(lang),
                    Err(err) => eprintln!("quin: syntax file {}: {err}", path.display()),
                },
                Err(err) => eprintln!("quin: cannot read {}: {err}", path.display()),
            }
        }
    }

    pub fn add(&mut self, lang: Language) {
        let name = lang.name.clone();
        if let Some(id) = self.by_name.get(&name) {
            self.languages[id.0] = lang;
            return;
        }
        let id = LangId(self.languages.len());
        self.languages.push(lang);
        self.by_name.insert(name, id);
    }

    pub fn get(&self, id: LangId) -> &Language {
        &self.languages[id.0]
    }

    /// Find a language by name, or by one of its file extensions (so that a
    /// markdown fence like ```py works too).
    pub fn find(&self, name: &str) -> Option<LangId> {
        let lower = name.to_ascii_lowercase();
        if let Some(id) = self.by_name.get(&lower) {
            return Some(*id);
        }
        self.languages.iter().position(|l| l.extensions.contains(&lower)).map(LangId)
    }

    /// Pick a language for a file from its name and first line.
    pub fn detect(&self, path: &Path, first_line: &str) -> Option<LangId> {
        let file_name = path.file_name()?.to_str()?;
        let lower_name = file_name.to_ascii_lowercase();
        if let Some(i) = self.languages.iter().position(|l| l.filenames.contains(&lower_name)) {
            return Some(LangId(i));
        }
        // Longest matching extension wins (`.d.ts` before `.ts`).
        let mut best: Option<(usize, usize)> = None;
        for (i, lang) in self.languages.iter().enumerate() {
            for ext in &lang.extensions {
                let suffix = format!(".{ext}");
                if lower_name.ends_with(&suffix) && best.is_none_or(|(_, len)| ext.len() > len) {
                    best = Some((i, ext.len()));
                }
            }
        }
        if let Some((i, _)) = best {
            return Some(LangId(i));
        }
        if first_line.starts_with("#!") {
            for (i, lang) in self.languages.iter().enumerate() {
                if lang.shebangs.iter().any(|s| first_line.contains(s.as_str())) {
                    return Some(LangId(i));
                }
            }
        }
        None
    }

    pub fn language_names(&self) -> Vec<&str> {
        self.languages.iter().map(|l| l.name.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_languages_parse() {
        let registry = Registry::load(None);
        assert_eq!(registry.languages.len(), BUILTIN.len());
        assert!(registry.find("rust").is_some());
        assert_eq!(registry.find("rs"), registry.find("rust"));
    }

    #[test]
    fn detects_by_extension_and_name() {
        let registry = Registry::load(None);
        let rust = registry.find("rust").unwrap();
        assert_eq!(registry.detect(Path::new("src/main.rs"), ""), Some(rust));
        let make = registry.find("makefile").unwrap();
        assert_eq!(registry.detect(Path::new("Makefile"), ""), Some(make));
        let sh = registry.find("shell").unwrap();
        assert_eq!(registry.detect(Path::new("run"), "#!/bin/bash"), Some(sh));
    }
}
