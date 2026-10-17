//! Overlays that take the keyboard: the fuzzy picker (files, buffers, commands,
//! search results) and the one-line prompt (search, select-regex...).

use std::path::PathBuf;

use crate::buffer::BufferId;
use crate::commands::Command;
use crate::keymap::{Key, KeyCode};
use crate::selection::Selection;
use crate::typed::TypedCommand;

// ---------------------------------------------------------------------------
// Fuzzy matching
// ---------------------------------------------------------------------------

/// Score how well `needle` matches `haystack` as a subsequence. Higher is
/// better; `None` when it does not match. Also returns the matched positions
/// (char indices) for highlighting.
pub fn fuzzy_match(needle: &str, haystack: &str) -> Option<(i32, Vec<usize>)> {
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }
    let smart_case = needle.chars().any(|c| c.is_uppercase());
    let hay: Vec<char> = haystack.chars().collect();
    let mut positions = Vec::with_capacity(needle.len());
    let mut score = 0;
    let mut last: Option<usize> = None;
    let mut i = 0;
    for n in needle.chars() {
        let mut found = None;
        while i < hay.len() {
            let h = hay[i];
            let equal = if smart_case { h == n } else { h.to_lowercase().eq(n.to_lowercase()) };
            if equal {
                found = Some(i);
                break;
            }
            i += 1;
        }
        let at = found?;
        score += match last {
            Some(prev) if prev + 1 == at => 8,
            _ => 1,
        };
        // Word starts (after `/`, `_`, `-`, `.`, space, or a case change) are worth more.
        if at == 0 || matches!(hay[at - 1], '/' | '_' | '-' | '.' | ' ' | '\\') || (hay[at - 1].is_lowercase() && hay[at].is_uppercase()) {
            score += 6;
        }
        positions.push(at);
        last = Some(at);
        i = at + 1;
    }
    // Shorter haystacks and earlier matches rank higher.
    score -= (positions[0] as i32).min(20) / 4;
    score -= (hay.len() as i32).min(100) / 20;
    Some((score, positions))
}

// ---------------------------------------------------------------------------
// Picker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ItemData {
    Path(PathBuf),
    /// A directory in the browsing file picker: choosing it descends into it.
    Dir,
    /// A path completion for a palette command's argument (`cd src/`).
    Argument(String),
    Buffer(BufferId),
    Typed(&'static TypedCommand),
    Command(&'static Command),
    Location {
        path: PathBuf,
        line: usize,
    },
    Theme(String),
    Language(String),
}

#[derive(Debug, Clone)]
pub struct PickerItem {
    pub label: String,
    /// Shown dimmed at the right (keybinding, path, description...).
    pub detail: String,
    pub data: ItemData,
    /// Characters of the label to highlight when the query is not a fuzzy
    /// needle (search results carry the regex match).
    pub highlight: Vec<usize>,
}

impl PickerItem {
    pub fn new(label: impl Into<String>, detail: impl Into<String>, data: ItemData) -> PickerItem {
        PickerItem { label: label.into(), detail: detail.into(), data, highlight: Vec::new() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Files,
    Buffers,
    Commands,
    Locations,
    Themes,
    Languages,
}

/// How the picker's choice should be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    Here,
    VSplit,
    HSplit,
}

#[derive(Debug, Clone)]
pub struct Picker {
    pub kind: PickerKind,
    pub title: String,
    pub items: Vec<PickerItem>,
    pub query: String,
    /// Indices into `items` that match the query, best first, with match positions.
    pub matches: Vec<(usize, Vec<usize>)>,
    pub selected: usize,
    /// First visible match (kept in step with `selected` by the drawing code).
    pub scroll: usize,
    /// Byte offset in `query` where the fuzzy needle starts (the file picker
    /// keeps a directory path in front of it).
    pub needle_start: usize,
    /// Directory text the file picker is browsing (`~/src/`), if any.
    pub browse: Option<String>,
}

pub enum PickerEvent {
    Pending,
    Close,
    /// An item was chosen (index into `items`) with the query text at that time.
    Choose {
        item: usize,
        query: String,
        open: OpenMode,
    },
    /// Enter with no matching item: the query itself is the answer.
    Submit(String),
}

impl Picker {
    pub fn new(kind: PickerKind, title: &str, items: Vec<PickerItem>) -> Picker {
        let mut picker = Picker {
            kind,
            title: title.to_string(),
            items,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
            scroll: 0,
            needle_start: 0,
            browse: None,
        };
        picker.filter();
        picker
    }

    pub fn with_query(mut self, query: &str) -> Picker {
        self.query = query.to_string();
        self.filter();
        self
    }

    /// Replace the items (results arriving, directory changed) and re-filter.
    pub fn set_items(&mut self, items: Vec<PickerItem>) {
        self.items = items;
        self.filter();
    }

    pub fn filter(&mut self) {
        // Search results are not filtered: the query is the regex they match.
        if self.kind == PickerKind::Locations {
            self.matches = self.items.iter().enumerate().map(|(i, item)| (i, item.highlight.clone())).collect();
            self.selected = self.selected.min(self.matches.len().saturating_sub(1));
            return;
        }
        let query = self.query.get(self.needle_start..).unwrap_or("").trim();
        // For commands, only the first word is matched against names so that
        // arguments can be typed after it (unless the argument is being completed).
        let needle =
            if self.kind == PickerKind::Commands && self.needle_start == 0 { query.split_whitespace().next().unwrap_or("") } else { query };
        let mut scored: Vec<(i32, usize, Vec<usize>)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| fuzzy_match(needle, &item.label).map(|(score, pos)| (score, i, pos)))
            .collect();
        if !needle.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        self.matches = scored.into_iter().map(|(_, i, pos)| (i, pos)).collect();
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    pub fn selected_item(&self) -> Option<&PickerItem> {
        self.matches.get(self.selected).map(|(i, _)| &self.items[*i])
    }

    pub fn handle_key(&mut self, key: Key) -> PickerEvent {
        let count = self.matches.len();
        match (key.code, key.ctrl, key.alt) {
            (KeyCode::Escape, _, _) | (KeyCode::Char('c'), true, _) => return PickerEvent::Close,
            (KeyCode::Enter, _, _) => return self.choose(OpenMode::Here),
            (KeyCode::Char('v'), true, _) => return self.choose(OpenMode::VSplit),
            (KeyCode::Char('s'), true, _) => return self.choose(OpenMode::HSplit),
            (KeyCode::Up, _, _) | (KeyCode::Char('p'), true, _) | (KeyCode::Char('k'), true, _) => {
                self.selected = if self.selected == 0 { count.saturating_sub(1) } else { self.selected - 1 };
            }
            (KeyCode::Down, _, _) | (KeyCode::Char('n'), true, _) | (KeyCode::Char('j'), true, _) => {
                self.selected = if count == 0 || self.selected + 1 >= count { 0 } else { self.selected + 1 };
            }
            (KeyCode::PageUp, _, _) => self.selected = self.selected.saturating_sub(10),
            (KeyCode::PageDown, _, _) => self.selected = (self.selected + 10).min(count.saturating_sub(1)),
            (KeyCode::Home, _, _) => self.selected = 0,
            (KeyCode::End, _, _) => self.selected = count.saturating_sub(1),
            (KeyCode::Tab, _, _) => {
                // Complete the query with the selected item's name (a path
                // keeps the directory part in front of it).
                if let Some(item) = self.selected_item() {
                    let prefix = self.query.get(..self.needle_start).unwrap_or("").to_string();
                    self.query = match self.kind {
                        PickerKind::Files => format!("{prefix}{}", item.label),
                        PickerKind::Commands if self.needle_start > 0 => format!("{prefix}{}", item.label),
                        _ => format!("{} ", item.label.split_whitespace().next().unwrap_or("")),
                    };
                    self.filter();
                }
            }
            (KeyCode::Char('w'), true, _) | (KeyCode::Backspace, false, true) => {
                let trimmed = self.query.trim_end().len();
                self.query.truncate(trimmed);
                while self.query.pop().is_some_and(|c| !c.is_whitespace()) {}
                self.filter();
            }
            (KeyCode::Char('u'), true, _) => {
                self.query.clear();
                self.filter();
            }
            (KeyCode::Backspace, _, _) => {
                self.query.pop();
                self.filter();
            }
            (KeyCode::Char(c), false, false) => {
                self.query.push(c);
                self.filter();
            }
            _ => {}
        }
        PickerEvent::Pending
    }

    fn choose(&self, open: OpenMode) -> PickerEvent {
        match self.matches.get(self.selected) {
            Some((item, _)) => PickerEvent::Choose { item: *item, query: self.query.clone(), open },
            None => PickerEvent::Submit(self.query.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Prompt
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    Search { reverse: bool, extend: bool },
    SelectRegex,
}

#[derive(Debug, Clone)]
pub struct Prompt {
    pub kind: PromptKind,
    pub label: String,
    pub text: String,
    /// Cursor as a char index into `text`.
    pub cursor: usize,
    /// Selection and scroll when the prompt was opened (restored on cancel,
    /// and used as the origin for incremental search).
    pub origin: Selection,
    pub origin_scroll: usize,
    pub history_index: Option<usize>,
}

pub enum PromptEvent {
    Pending,
    Changed,
    Submit(String),
    Cancel,
    /// Up/Down: the caller fills the text from its history.
    History(isize),
}

impl Prompt {
    pub fn new(kind: PromptKind, label: &str, origin: Selection, origin_scroll: usize) -> Prompt {
        Prompt { kind, label: label.to_string(), text: String::new(), cursor: 0, origin, origin_scroll, history_index: None }
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.chars().count();
    }

    /// Insert text at the cursor (pasting).
    pub fn insert(&mut self, text: &str) {
        let at = self.byte_cursor();
        self.text.insert_str(at, text);
        self.cursor += text.chars().count();
    }

    fn byte_cursor(&self) -> usize {
        self.text.char_indices().nth(self.cursor).map(|(i, _)| i).unwrap_or(self.text.len())
    }

    pub fn handle_key(&mut self, key: Key) -> PromptEvent {
        let len = self.text.chars().count();
        match (key.code, key.ctrl, key.alt) {
            (KeyCode::Escape, _, _) | (KeyCode::Char('c'), true, _) => return PromptEvent::Cancel,
            (KeyCode::Enter, _, _) => return PromptEvent::Submit(self.text.clone()),
            (KeyCode::Up, _, _) | (KeyCode::Char('p'), true, _) => return PromptEvent::History(-1),
            (KeyCode::Down, _, _) | (KeyCode::Char('n'), true, _) => return PromptEvent::History(1),
            (KeyCode::Left, _, _) | (KeyCode::Char('b'), true, _) => self.cursor = self.cursor.saturating_sub(1),
            (KeyCode::Right, _, _) | (KeyCode::Char('f'), true, _) => self.cursor = (self.cursor + 1).min(len),
            (KeyCode::Home, _, _) | (KeyCode::Char('a'), true, _) => self.cursor = 0,
            (KeyCode::End, _, _) | (KeyCode::Char('e'), true, _) => self.cursor = len,
            (KeyCode::Backspace, false, false) | (KeyCode::Char('h'), true, _) => {
                if self.cursor > 0 {
                    let at = self.byte_cursor();
                    let prev = self.text[..at].chars().next_back().map(|c| c.len_utf8()).unwrap_or(0);
                    self.text.replace_range(at - prev..at, "");
                    self.cursor -= 1;
                    return PromptEvent::Changed;
                }
            }
            (KeyCode::Delete, _, _) | (KeyCode::Char('d'), true, _) => {
                if self.cursor < len {
                    let at = self.byte_cursor();
                    let next = self.text[at..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
                    self.text.replace_range(at..at + next, "");
                    return PromptEvent::Changed;
                }
            }
            (KeyCode::Char('w'), true, _) | (KeyCode::Backspace, false, true) => {
                let at = self.byte_cursor();
                let head: String = self.text[..at].to_string();
                let trimmed = head.trim_end();
                let word_start = trimmed.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
                self.text.replace_range(word_start..at, "");
                self.cursor = self.text[..word_start].chars().count();
                return PromptEvent::Changed;
            }
            (KeyCode::Char('u'), true, _) => {
                let at = self.byte_cursor();
                self.text.replace_range(..at, "");
                self.cursor = 0;
                return PromptEvent::Changed;
            }
            (KeyCode::Char('k'), true, _) => {
                let at = self.byte_cursor();
                self.text.truncate(at);
                return PromptEvent::Changed;
            }
            (KeyCode::Char(c), false, false) => {
                let at = self.byte_cursor();
                self.text.insert(at, c);
                self.cursor += 1;
                return PromptEvent::Changed;
            }
            _ => {}
        }
        PromptEvent::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_word_starts_and_runs() {
        let (a, _) = fuzzy_match("mn", "src/main.rs").unwrap();
        let (b, _) = fuzzy_match("mn", "common.rs").unwrap();
        assert!(a > b);
        assert!(fuzzy_match("xyz", "abc").is_none());
        assert_eq!(fuzzy_match("ab", "xaxb").unwrap().1, vec![1, 3]);
    }

    #[test]
    fn prompt_editing() {
        let mut p = Prompt::new(PromptKind::SelectRegex, "select:", Selection::point(0), 0);
        for c in "héllo".chars() {
            p.handle_key(Key::char(c));
        }
        p.handle_key(Key::code(KeyCode::Backspace));
        assert_eq!(p.text, "héll");
        p.handle_key(Key::char('w').ctrl());
        assert_eq!(p.text, "");
    }
}
