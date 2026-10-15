//! A buffer is an open piece of text: a file, a scratch pad or a terminal.
//! Views (frames) look at buffers; several views may show the same buffer.

use std::io;
use std::path::{Path, PathBuf};

use ropey::Rope;

use crate::history::{History, Transaction};
use crate::selection::Selection;
use crate::syntax::highlight::HighlightCache;
use crate::syntax::{IndentStyle, LangId, Registry};
use crate::terminal::Terminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BufferId(pub usize);

pub struct Buffer {
    pub id: BufferId,
    pub text: Rope,
    pub path: Option<PathBuf>,
    pub language: Option<LangId>,
    pub history: History,
    pub highlight: HighlightCache,
    /// Present for terminal buffers. Their `text` mirrors the terminal screen so
    /// that normal-mode navigation and yanking work on it.
    pub terminal: Option<Terminal>,
    pub readonly: bool,
    /// The file tree sidebar (`tree.rs`): `:write` applies its edits to disk.
    pub tree: bool,
    /// The git status buffer (`space l`).
    pub git: bool,
    /// A commit message being written (`Some(true)` amends the last commit).
    pub commit: Option<bool>,
    /// The welcome buffer: the recent entry behind each line.
    pub welcome: Option<Vec<Option<PathBuf>>>,
    /// Bumped on every edit; views use it to know when to recompute layout.
    pub version: u64,
    pub indent: IndentStyle,
    /// The file used CRLF line endings; they are converted on load and restored on save.
    pub crlf: bool,
    /// Where the last edit happened (`g .`).
    pub last_change: Option<usize>,
}

const DEFAULT_INDENT: IndentStyle = IndentStyle { width: 4, use_tabs: false };

impl Buffer {
    pub fn scratch(id: BufferId) -> Buffer {
        Buffer {
            id,
            text: Rope::new(),
            path: None,
            language: None,
            history: History::new(),
            highlight: HighlightCache::new(),
            terminal: None,
            readonly: false,
            tree: false,
            git: false,
            commit: None,
            welcome: None,
            version: 0,
            indent: DEFAULT_INDENT,
            crlf: false,
            last_change: None,
        }
    }

    /// Open a file. A path that does not exist yet gives an empty buffer that
    /// will be created on save.
    pub fn open(id: BufferId, path: &Path, registry: &Registry) -> io::Result<Buffer> {
        let mut buffer = Buffer::scratch(id);
        let path = absolute(path);
        if path.exists() {
            let bytes = std::fs::read(&path)?;
            let content = String::from_utf8_lossy(&bytes);
            buffer.crlf = content.contains("\r\n");
            let normalized = if buffer.crlf { content.replace("\r\n", "\n") } else { content.into_owned() };
            buffer.text = Rope::from_str(&normalized);
            buffer.readonly = std::fs::metadata(&path).map(|m| m.permissions().readonly()).unwrap_or(false);
        }
        buffer.path = Some(path);
        buffer.detect_language(registry);
        buffer.indent = buffer.detect_indent(registry);
        Ok(buffer)
    }

    pub fn terminal(id: BufferId, terminal: Terminal) -> Buffer {
        let mut buffer = Buffer::scratch(id);
        buffer.terminal = Some(terminal);
        buffer.readonly = true;
        buffer
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal.is_some()
    }

    pub fn detect_language(&mut self, registry: &Registry) {
        let first_line: String = self.text.line(0).chars().take(200).collect();
        self.language = self.path.as_deref().and_then(|p| registry.detect(p, &first_line));
        self.highlight.reset();
    }

    pub fn set_language(&mut self, language: Option<LangId>) {
        self.language = language;
        self.highlight.reset();
    }

    /// Guess the indentation from the file content, falling back to the
    /// language's preference.
    fn detect_indent(&self, registry: &Registry) -> IndentStyle {
        let fallback = self.language.and_then(|l| registry.get(l).indent).unwrap_or(DEFAULT_INDENT);
        let mut tabs = 0;
        let mut counts = [0usize; 9];
        for line in self.text.lines().take(500) {
            let mut spaces = 0;
            for c in line.chars() {
                match c {
                    ' ' => spaces += 1,
                    '\t' if spaces == 0 => {
                        tabs += 1;
                        break;
                    }
                    _ => break,
                }
            }
            if (1..=8).contains(&spaces) {
                counts[spaces] += 1;
            }
        }
        let (width, count) = [2usize, 4, 8, 3].iter().map(|&w| (w, counts[w])).max_by_key(|&(_, c)| c).unwrap_or((4, 0));
        if tabs > count {
            IndentStyle { width: fallback.width, use_tabs: true }
        } else if count > 0 {
            IndentStyle { width, use_tabs: false }
        } else {
            fallback
        }
    }

    pub fn display_name(&self) -> String {
        if let Some(term) = &self.terminal {
            return format!("[terminal] {}", term.title());
        }
        if self.tree {
            return "[tree]".to_string();
        }
        if self.git {
            return "[git]".to_string();
        }
        if self.commit.is_some() {
            return "[commit]".to_string();
        }
        if self.welcome.is_some() {
            return "[welcome]".to_string();
        }
        match &self.path {
            Some(path) => relative_to_cwd(path),
            None => "[scratch]".to_string(),
        }
    }

    pub fn is_modified(&self) -> bool {
        !self.is_terminal() && self.history.is_modified()
    }

    /// Write the buffer to `path` (or its own path). Returns the number of bytes written.
    pub fn save(&mut self, path: Option<&Path>) -> io::Result<usize> {
        let path = match path.map(absolute).or_else(|| self.path.clone()) {
            Some(p) => p,
            None => return Err(io::Error::new(io::ErrorKind::InvalidInput, "no file name (use :write <path>)")),
        };
        let mut content = self.text.to_string();
        if self.crlf {
            content = content.replace('\n', "\r\n");
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &content)?;
        self.path = Some(path);
        self.history.mark_saved();
        Ok(content.len())
    }

    /// Apply a transaction, record it for undo and keep the highlighter in sync.
    pub fn apply(&mut self, tx: Transaction, before: &Selection, after: &Selection) {
        if tx.is_empty() {
            return;
        }
        let first_line = tx.changes().iter().map(|c| self.text.char_to_line(c.from.min(self.text.len_chars()))).min().unwrap_or(0);
        let old_lines = self.text.len_lines();
        self.last_change = tx.changes().iter().map(|c| c.from).min();
        let inverse = tx.clone().apply(&mut self.text);
        let new_lines = self.text.len_lines();
        let last_line =
            inverse.changes().iter().map(|c| self.text.char_to_line(c.to.min(self.text.len_chars()))).max().unwrap_or(first_line);
        self.highlight.edited(first_line, last_line, new_lines as isize - old_lines as isize);
        self.history.record(tx, inverse, before, after);
        self.version += 1;
    }

    /// Replace the whole text without recording history (terminal refresh, reload).
    pub fn set_text(&mut self, text: &str) {
        self.text = Rope::from_str(text);
        self.highlight.reset();
        self.version += 1;
    }

    pub fn undo(&mut self) -> Option<Selection> {
        let selection = self.history.undo(&mut self.text)?;
        self.highlight.reset();
        self.version += 1;
        Some(selection)
    }

    pub fn redo(&mut self) -> Option<Selection> {
        let selection = self.history.redo(&mut self.text)?;
        self.highlight.reset();
        self.version += 1;
        Some(selection)
    }

    // ----- Convenience accessors on the text -----

    pub fn len_chars(&self) -> usize {
        self.text.len_chars()
    }

    pub fn line_count(&self) -> usize {
        self.text.len_lines()
    }

    pub fn line_of(&self, pos: usize) -> usize {
        self.text.char_to_line(pos.min(self.text.len_chars()))
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.text.line_to_char(line.min(self.line_count() - 1))
    }

    /// Position of the newline ending `line` (or the end of the text for the last line).
    pub fn line_end(&self, line: usize) -> usize {
        let line = line.min(self.line_count() - 1);
        let start = self.text.line_to_char(line);
        let len = self.text.line(line).len_chars();
        let end = start + len;
        if end > start && self.text.char(end - 1) == '\n' { end - 1 } else { end }
    }

    pub fn line_text(&self, line: usize) -> String {
        crate::syntax::highlight::line_without_newline(&self.text, line)
    }

    /// Leading whitespace of a line.
    pub fn line_indent(&self, line: usize) -> String {
        self.text.line(line).chars().take_while(|c| *c == ' ' || *c == '\t').collect()
    }

    /// Position of the first non-blank character of a line.
    pub fn first_nonblank(&self, line: usize) -> usize {
        let start = self.line_start(line);
        let end = self.line_end(line);
        let mut pos = start;
        while pos < end && matches!(self.text.char(pos), ' ' | '\t') {
            pos += 1;
        }
        pos
    }

    pub fn indent_unit(&self) -> String {
        if self.indent.use_tabs { "\t".to_string() } else { " ".repeat(self.indent.width) }
    }
}

/// Make a path absolute without touching the filesystem.
pub fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return normalize(path);
    }
    match std::env::current_dir() {
        Ok(cwd) => normalize(&cwd.join(path)),
        Err(_) => path.to_path_buf(),
    }
}

/// Remove `.` and `..` components.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

pub fn relative_to_cwd(path: &Path) -> String {
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(rel) = path.strip_prefix(&cwd)
    {
        return rel.display().to_string();
    }
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rel) = path.strip_prefix(&home)
    {
        return format!("~/{}", rel.display());
    }
    path.display().to_string()
}
