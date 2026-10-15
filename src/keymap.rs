//! Keys and keymaps.
//!
//! A keymap is a trie: a key either runs a command or opens a sub-map (`g`, `m`,
//! `space`, ...). The tables at the bottom of this file are the single source of
//! truth for the default bindings; the README's keybinding list mirrors them.

use std::collections::HashMap;

use crate::commands;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Escape,
    Enter,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    /// Only meaningful for non-character keys (`S-tab`); for characters the shift
    /// is already folded into the character itself (`A` vs `a`).
    pub shift: bool,
    /// Command/Super/Windows key.
    pub meta: bool,
}

impl Key {
    pub fn char(c: char) -> Key {
        Key { code: KeyCode::Char(c), ctrl: false, alt: false, shift: false, meta: false }
    }

    pub fn code(code: KeyCode) -> Key {
        Key { code, ctrl: false, alt: false, shift: false, meta: false }
    }

    #[cfg(test)]
    pub fn ctrl(mut self) -> Key {
        self.ctrl = true;
        self
    }

    #[cfg(test)]
    pub fn alt(mut self) -> Key {
        self.alt = true;
        self
    }

    pub fn plain_char(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(c) if !self.ctrl && !self.alt && !self.meta => Some(c),
            _ => None,
        }
    }

    /// Parse the textual form used in the keymap tables: `C-w`, `A-;`, `space`,
    /// `S-tab`, `ret`, `esc`, `pageup`, `F5`...
    pub fn parse(s: &str) -> Option<Key> {
        let mut key = Key::code(KeyCode::Escape);
        let mut rest = s;
        loop {
            if let Some(r) = rest.strip_prefix("C-") {
                key.ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("A-") {
                key.alt = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("S-") {
                key.shift = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("M-") {
                key.meta = true;
                rest = r;
            } else {
                break;
            }
        }
        key.code = match rest {
            "esc" => KeyCode::Escape,
            "ret" | "enter" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "del" | "delete" => KeyCode::Delete,
            "ins" | "insert" => KeyCode::Insert,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "space" => KeyCode::Char(' '),
            "minus" => KeyCode::Char('-'),
            _ => {
                let mut chars = rest.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => KeyCode::Char(c),
                    (Some('F'), Some(_)) => KeyCode::F(rest[1..].parse().ok()?),
                    _ => return None,
                }
            }
        };
        Some(key)
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ctrl {
            write!(f, "C-")?;
        }
        if self.alt {
            write!(f, "A-")?;
        }
        if self.shift {
            write!(f, "S-")?;
        }
        if self.meta {
            write!(f, "M-")?;
        }
        match self.code {
            KeyCode::Char(' ') => write!(f, "space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Escape => write!(f, "esc"),
            KeyCode::Enter => write!(f, "ret"),
            KeyCode::Tab => write!(f, "tab"),
            KeyCode::Backspace => write!(f, "backspace"),
            KeyCode::Delete => write!(f, "del"),
            KeyCode::Insert => write!(f, "ins"),
            KeyCode::Home => write!(f, "home"),
            KeyCode::End => write!(f, "end"),
            KeyCode::PageUp => write!(f, "pageup"),
            KeyCode::PageDown => write!(f, "pagedown"),
            KeyCode::Up => write!(f, "up"),
            KeyCode::Down => write!(f, "down"),
            KeyCode::Left => write!(f, "left"),
            KeyCode::Right => write!(f, "right"),
            KeyCode::F(n) => write!(f, "F{n}"),
        }
    }
}

/// A node in the keymap trie.
#[derive(Debug, Clone)]
pub enum KeyNode {
    Command(&'static commands::Command),
    Menu(Keymap),
}

#[derive(Debug, Clone, Default)]
pub struct Keymap {
    pub name: &'static str,
    pub nodes: HashMap<Key, KeyNode>,
}

impl Keymap {
    pub fn get(&self, key: Key) -> Option<&KeyNode> {
        self.nodes.get(&key)
    }

    /// Insert a binding for a key sequence such as `"space w v"`.
    fn bind(&mut self, sequence: &str, command: &'static commands::Command, menu_names: &[(&str, &'static str)]) {
        let keys: Vec<Key> = sequence.split(' ').map(|k| Key::parse(k).unwrap_or_else(|| panic!("bad key in keymap: {k:?}"))).collect();
        let mut map = self;
        let mut prefix = String::new();
        for (i, key) in keys.iter().enumerate() {
            if i + 1 == keys.len() {
                map.nodes.insert(*key, KeyNode::Command(command));
                return;
            }
            if !prefix.is_empty() {
                prefix.push(' ');
            }
            prefix.push_str(&key.to_string());
            let menu_name = menu_names.iter().find(|(p, _)| *p == prefix).map(|(_, n)| *n).unwrap_or("");
            let node = map.nodes.entry(*key).or_insert_with(|| KeyNode::Menu(Keymap { name: menu_name, nodes: HashMap::new() }));
            map = match node {
                KeyNode::Menu(m) => m,
                KeyNode::Command(_) => panic!("key {prefix:?} is both a command and a menu"),
            };
        }
    }

    /// Build a keymap from a `(sequence, command name)` table.
    pub fn from_table(name: &'static str, table: &[(&str, &str)], menu_names: &[(&str, &'static str)]) -> Keymap {
        let mut map = Keymap { name, nodes: HashMap::new() };
        for (sequence, command) in table {
            let cmd = commands::find(command).unwrap_or_else(|| panic!("unknown command in keymap: {command}"));
            map.bind(sequence, cmd, menu_names);
        }
        map
    }

    /// All (sequence, command) pairs, for the command palette and `:help`.
    pub fn bindings(&self) -> Vec<(String, &'static commands::Command)> {
        let mut out = Vec::new();
        collect_bindings(self, String::new(), &mut out);
        out
    }
}

fn collect_bindings(map: &Keymap, prefix: String, out: &mut Vec<(String, &'static commands::Command)>) {
    for (key, node) in &map.nodes {
        let seq = if prefix.is_empty() { key.to_string() } else { format!("{prefix} {key}") };
        match node {
            KeyNode::Command(cmd) => out.push((seq, cmd)),
            KeyNode::Menu(m) => collect_bindings(m, seq, out),
        }
    }
}

pub struct Keymaps {
    pub normal: Keymap,
    pub select: Keymap,
    pub insert: Keymap,
    pub terminal: Keymap,
}

impl Keymaps {
    pub fn default_keymaps() -> Keymaps {
        let mut normal = Keymap::from_table("normal", NORMAL_KEYS, MENU_NAMES);
        let mut select = normal.clone();
        select.name = "select";
        for (sequence, command) in SELECT_KEYS {
            let cmd = commands::find(command).expect("select keymap command");
            select.bind(sequence, cmd, MENU_NAMES);
        }
        normal.name = "normal";
        Keymaps {
            normal,
            select,
            insert: Keymap::from_table("insert", INSERT_KEYS, &[]),
            terminal: Keymap::from_table("terminal", TERMINAL_KEYS, &[]),
        }
    }
}

const MENU_NAMES: &[(&str, &str)] = &[("g", "goto"), ("m", "match"), ("space", "space"), ("space w", "window")];

/// Normal mode bindings (Helix-style, trimmed to the basics).
pub const NORMAL_KEYS: &[(&str, &str)] = &[
    // Movement
    ("h", "move_char_left"),
    ("l", "move_char_right"),
    ("j", "move_line_down"),
    ("k", "move_line_up"),
    ("w", "move_next_word_start"),
    ("b", "move_prev_word_start"),
    ("e", "move_next_word_end"),
    ("W", "move_next_long_word_start"),
    ("B", "move_prev_long_word_start"),
    ("E", "move_next_long_word_end"),
    ("f", "find_next_char"),
    ("t", "find_till_char"),
    ("F", "find_prev_char"),
    ("T", "find_till_prev_char"),
    ("C-d", "half_page_down"),
    ("C-u", "half_page_up"),
    ("C-o", "jump_backward"),
    ("C-i", "jump_forward"),
    // Goto menu
    ("g g", "goto_file_start"),
    ("g e", "goto_file_end"),
    ("g h", "goto_line_start"),
    ("g l", "goto_line_end"),
    ("g s", "goto_first_nonwhitespace"),
    ("g n", "goto_next_buffer"),
    ("g p", "goto_previous_buffer"),
    ("g .", "goto_last_modification"),
    ("g f", "goto_file"),
    ("g t", "tab_next"),
    ("g T", "tab_prev"),
    // Selections
    ("%", "select_all"),
    ("x", "extend_line_below"),
    ("X", "extend_line_above"),
    (";", "collapse_selection"),
    ("A-;", "flip_selections"),
    (",", "keep_primary_selection"),
    ("C", "copy_selection_on_next_line"),
    ("A-C", "copy_selection_on_prev_line"),
    ("s", "select_regex"),
    ("v", "select_mode"),
    ("esc", "normal_mode"),
    // Match menu
    ("m m", "match_brackets"),
    ("m i", "select_textobject_inner"),
    ("m a", "select_textobject_around"),
    // Changes
    ("i", "insert_mode"),
    ("a", "append_mode"),
    ("I", "insert_at_line_start"),
    ("A", "insert_at_line_end"),
    ("o", "open_below"),
    ("O", "open_above"),
    ("d", "delete_selection"),
    ("A-d", "delete_selection_noyank"),
    ("c", "change_selection"),
    ("A-c", "change_selection_noyank"),
    ("y", "yank"),
    ("p", "paste"),
    ("P", "paste_noyank"),
    ("r", "replace_char"),
    ("R", "replace_mode"),
    (">", "indent"),
    ("<", "unindent"),
    ("u", "undo"),
    ("U", "redo"),
    ("C-c", "toggle_comments"),
    (".", "repeat_last_insert"),
    // Search
    ("/", "search"),
    ("?", "rsearch"),
    ("n", "search_next"),
    ("N", "search_prev"),
    ("*", "search_selection"),
    // Pickers
    (":", "command_palette"),
    ("space e", "file_picker"),
    ("space E", "file_picker_all"),
    ("space b", "buffer_picker"),
    ("space f", "global_search"),
    ("space c", "window_close"),
    ("A-e", "tree_toggle"),
    ("ret", "activate_line"),
    ("C-=", "zoom_in"),
    ("C-+", "zoom_in"),
    ("C--", "zoom_out"),
    // Window menu
    ("space w w", "window_next"),
    ("space w W", "window_prev"),
    ("space w v", "vsplit"),
    ("space w s", "hsplit"),
    ("space w n", "vsplit_new"),
    ("space w N", "hsplit_new"),
    ("space w h", "window_left"),
    ("space w j", "window_down"),
    ("space w k", "window_up"),
    ("space w l", "window_right"),
    ("space w H", "window_swap_left"),
    ("space w J", "window_swap_down"),
    ("space w K", "window_swap_up"),
    ("space w L", "window_swap_right"),
    ("space w q", "window_close"),
    ("space w o", "window_only"),
    ("space w t", "tab_new"),
    ("space w x", "tab_close"),
    ("space w +", "resize_taller"),
    ("space w -", "resize_shorter"),
    ("space w >", "resize_wider"),
    ("space w <", "resize_narrower"),
    ("space w 0", "resize_reset"),
];

/// Commands that keep their menu open after running, so the key can be
/// pressed (or held) again: `space w + + +` keeps resizing.
pub const REPEATABLE: &[&str] = &["resize_taller", "resize_shorter", "resize_wider", "resize_narrower"];

/// Extra bindings for select (extend) mode on top of the normal ones.
pub const SELECT_KEYS: &[(&str, &str)] =
    &[("v", "normal_mode"), ("esc", "normal_mode"), ("x", "extend_line_below"), ("X", "extend_line_above")];

/// Insert and replace mode: everything else types a character.
pub const INSERT_KEYS: &[(&str, &str)] = &[
    ("esc", "normal_mode"),
    ("ret", "insert_newline"),
    ("tab", "insert_tab"),
    ("backspace", "delete_char_backward"),
    ("del", "delete_char_forward"),
    ("C-=", "zoom_in"),
    ("C-+", "zoom_in"),
    ("C--", "zoom_out"),
];

/// Keys intercepted while a terminal buffer is in terminal mode. Everything
/// else is sent to the shell.
pub const TERMINAL_KEYS: &[(&str, &str)] =
    &[("C-esc", "normal_mode"), ("C-S-c", "yank"), ("C-S-v", "paste"), ("C-=", "zoom_in"), ("C-+", "zoom_in"), ("C--", "zoom_out")];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keys() {
        assert_eq!(Key::parse("C-w"), Some(Key::char('w').ctrl()));
        assert_eq!(Key::parse("A-;"), Some(Key::char(';').alt()));
        assert_eq!(Key::parse("space"), Some(Key::char(' ')));
        assert_eq!(Key::parse("C-esc"), Some(Key::code(KeyCode::Escape).ctrl()));
        assert_eq!(Key::parse("F5"), Some(Key::code(KeyCode::F(5))));
    }

    #[test]
    fn default_keymaps_build() {
        let maps = Keymaps::default_keymaps();
        assert!(maps.normal.get(Key::char('w')).is_some());
        assert!(matches!(maps.normal.get(Key::char('g')), Some(KeyNode::Menu(_))));
    }
}
