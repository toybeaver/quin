//! The editor state: buffers, views, tabs, mode and everything the commands
//! operate on. Key handling (`handle_key`) lives here too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::buffer::{Buffer, BufferId};
use crate::clipboard::Clipboard;
use crate::commands::{self, Command, Ctx};
use crate::config::Config;
use crate::frame::{Dir, Node, Tab, View, ViewId};
use crate::history::Transaction;
use crate::keymap::{Key, KeyCode, KeyNode, Keymap, Keymaps};
use crate::picker::{Picker, Prompt};
use crate::render::Rect;
use crate::selection::{Range, Selection};
use crate::syntax::Registry;
use crate::syntax::highlight::Highlighter;
use crate::terminal::{Terminal, WakeFn};
use crate::theme::{Style, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    /// Like insert mode, but typing overwrites (vim's `R`).
    Replace,
    Select,
    /// Keys go to the terminal of the focused frame.
    Terminal,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Normal => "NOR",
            Mode::Insert => "INS",
            Mode::Replace => "REP",
            Mode::Select => "SEL",
            Mode::Terminal => "TRM",
        }
    }

    /// Insert and replace mode: the modes where typing changes the text.
    pub fn is_editing(self) -> bool {
        matches!(self, Mode::Insert | Mode::Replace)
    }
}

/// What to do with the next literal key (after `f`, `r`, `mi`...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingChar {
    FindChar { forward: bool, till: bool },
    ReplaceChar,
    TextObject { around: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Error,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub text: String,
    pub kind: MessageKind,
}

pub enum Overlay {
    None,
    Prompt(Prompt),
    Picker(Picker),
}

/// A global search running on a background thread (see `files::grep_in_background`).
pub struct LiveSearch {
    pub results: std::sync::mpsc::Receiver<crate::files::GrepHit>,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub done: bool,
}

impl Drop for LiveSearch {
    fn drop(&mut self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[derive(Debug, Default)]
pub struct SearchState {
    pub regex: Option<regex::Regex>,
    pub history: Vec<String>,
}

/// Resolved styles for every scope, rebuilt when the theme changes.
pub struct StyleTable {
    pub styles: Vec<Style>,
    /// Replacement text per scope (`style list.marker replace=•`).
    pub replacements: Vec<Option<String>>,
}

pub struct Editor {
    pub buffers: HashMap<BufferId, Buffer>,
    pub views: HashMap<ViewId, View>,
    pub tabs: Vec<Tab>,
    pub tab_index: usize,
    next_id: usize,

    pub mode: Mode,
    pub keymaps: Keymaps,
    /// Keys typed so far of a multi-key sequence (`space w ...`).
    pub pending_keys: Vec<Key>,
    pub count: Option<usize>,
    pub pending_char: Option<PendingChar>,
    pub search: SearchState,
    pub overlay: Overlay,
    pub message: Option<Message>,
    /// Keys of the last insert session, replayed by `.`.
    pub last_insert: Vec<Key>,
    recording: Option<Vec<Key>>,
    replaying: bool,

    pub registry: Registry,
    pub theme: Theme,
    pub styles: StyleTable,
    pub highlighter: Highlighter,
    pub config: Config,
    pub clipboard: Clipboard,
    /// Pieces of the last yank, one per selection (the clipboard holds them
    /// joined with newlines).
    pub yanked: Vec<String>,
    pub cwd: PathBuf,
    pub quit: bool,
    /// Set when the font size changed and the font must be reloaded.
    pub font_dirty: bool,
    /// Name of the font family in use (for the config template).
    pub font_family: String,
    /// The file tree sidebar, once opened.
    pub tree: Option<crate::tree::Tree>,

    /// Frame rectangles of the current tab from the last draw (for mouse and
    /// window navigation).
    pub frame_rects: Vec<(ViewId, Rect)>,
    /// Rectangles of the tab bar entries from the last draw.
    pub tab_rects: Vec<Rect>,
    /// Called by terminals when output arrives.
    pub wake: Option<WakeFn>,
    /// Buffers in order of last access (used when a buffer closes).
    pub buffer_access: Vec<BufferId>,
    /// Cached file list for the file picker: (includes ignored files, paths).
    pub file_list: Option<(bool, Vec<PathBuf>)>,
    /// The global search feeding the results picker, while one runs.
    pub live_search: Option<LiveSearch>,
    /// Preview of the item highlighted in a picker (rebuilt when it changes).
    pub preview: Option<Preview>,
    /// The theme to go back to when the theme picker is cancelled.
    pub theme_before_picker: Option<Theme>,
    /// Where the picker drew its list and its preview, for the mouse wheel.
    pub picker_rects: Option<(Rect, Rect)>,
}

/// What the picker preview pane shows for the highlighted item.
pub struct Preview {
    /// Identifies the item the preview was built for.
    pub key: String,
    /// A file read for the preview; `None` when previewing an open buffer.
    pub file: Option<Buffer>,
    pub buffer_id: Option<BufferId>,
    /// Line to highlight and keep in the middle (search results).
    pub center_line: Option<usize>,
    /// First visible line; starts centred on `center_line`, the mouse wheel moves it.
    pub scroll: usize,
    /// Shown instead of text when there is nothing to show.
    pub note: Option<String>,
}

impl Editor {
    pub fn new(config: Config, registry: Registry, theme: Theme) -> Editor {
        let styles = build_styles(&theme, &registry);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut editor = Editor {
            buffers: HashMap::new(),
            views: HashMap::new(),
            tabs: Vec::new(),
            tab_index: 0,
            next_id: 1,
            mode: Mode::Normal,
            keymaps: Keymaps::default_keymaps(),
            pending_keys: Vec::new(),
            count: None,
            pending_char: None,
            search: SearchState::default(),
            overlay: Overlay::None,
            message: None,
            last_insert: Vec::new(),
            recording: None,
            replaying: false,
            registry,
            theme,
            styles,
            highlighter: Highlighter::new(),
            config,
            clipboard: Clipboard::new(),
            yanked: Vec::new(),
            cwd,
            quit: false,
            font_dirty: false,
            font_family: "Hack".to_string(),
            tree: None,
            frame_rects: Vec::new(),
            tab_rects: Vec::new(),
            wake: None,
            buffer_access: Vec::new(),
            file_list: None,
            live_search: None,
            preview: None,
            theme_before_picker: None,
            picker_rects: None,
        };
        let buffer = editor.new_scratch_buffer();
        let view = editor.new_view(buffer);
        editor.tabs.push(Tab::new(view));
        editor
    }

    fn next_id(&mut self) -> usize {
        self.next_id += 1;
        self.next_id
    }

    // ----- Buffers -----

    pub fn new_scratch_buffer(&mut self) -> BufferId {
        let id = BufferId(self.next_id());
        self.buffers.insert(id, Buffer::scratch(id));
        id
    }

    /// Open a file (or reuse the buffer that already has it open).
    pub fn open_file(&mut self, path: &Path) -> Result<BufferId, String> {
        let absolute = crate::buffer::absolute(path);
        if let Some(existing) = self.buffers.values().find(|b| b.path.as_deref() == Some(&absolute)) {
            return Ok(existing.id);
        }
        if absolute.is_dir() {
            return Err(format!("{} is a directory", absolute.display()));
        }
        let id = BufferId(self.next_id());
        let buffer = Buffer::open(id, &absolute, &self.registry).map_err(|e| format!("{}: {e}", path.display()))?;
        self.buffers.insert(id, buffer);
        Ok(id)
    }

    /// Show a buffer in the focused view.
    pub fn show_buffer(&mut self, buffer: BufferId) {
        let view = self.focused_view();
        self.views.get_mut(&view).expect("focused view exists").switch_buffer(buffer);
        self.note_buffer_access(buffer);
        self.sync_mode_with_buffer();
    }

    fn note_buffer_access(&mut self, buffer: BufferId) {
        self.buffer_access.retain(|b| *b != buffer);
        self.buffer_access.push(buffer);
    }

    /// Close a buffer; views showing it move to another buffer (or a new scratch).
    pub fn close_buffer(&mut self, id: BufferId, force: bool) -> Result<(), String> {
        let buffer = self.buffers.get(&id).ok_or("no such buffer")?;
        if buffer.is_modified() && !force {
            return Err(format!("{} has unsaved changes", buffer.display_name()));
        }
        self.buffers.remove(&id);
        self.buffer_access.retain(|b| *b != id);
        let fallback = match self.buffer_access.last() {
            Some(b) => *b,
            None => self.new_scratch_buffer(),
        };
        for view in self.views.values_mut() {
            if view.buffer == id {
                view.switch_buffer(fallback);
            }
            view.saved_selections.remove(&id);
            view.jumps.retain(|j| j.buffer != id);
            view.jump_index = view.jump_index.min(view.jumps.len());
        }
        self.sync_mode_with_buffer();
        Ok(())
    }

    /// Buffers sorted by id, for pickers and `gn`/`gp`.
    pub fn buffer_ids(&self) -> Vec<BufferId> {
        let mut ids: Vec<BufferId> = self.buffers.keys().copied().collect();
        ids.sort();
        ids
    }

    // ----- Views and tabs -----

    fn new_view(&mut self, buffer: BufferId) -> ViewId {
        let id = ViewId(self.next_id());
        self.views.insert(id, View::new(id, buffer));
        self.note_buffer_access(buffer);
        id
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.tab_index]
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        let i = self.tab_index;
        &mut self.tabs[i]
    }

    pub fn focused_view(&self) -> ViewId {
        self.tab().focus
    }

    pub fn view(&self) -> &View {
        &self.views[&self.focused_view()]
    }

    pub fn view_mut(&mut self) -> &mut View {
        let id = self.focused_view();
        self.views.get_mut(&id).expect("focused view exists")
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffers[&self.view().buffer]
    }

    pub fn buffer_mut(&mut self) -> &mut Buffer {
        let id = self.view().buffer;
        self.buffers.get_mut(&id).expect("view's buffer exists")
    }

    /// The focused view and its buffer, both mutable.
    pub fn current(&mut self) -> (&mut View, &mut Buffer) {
        let view_id = self.focused_view();
        let view = self.views.get_mut(&view_id).expect("focused view exists");
        let buffer = self.buffers.get_mut(&view.buffer).expect("view's buffer exists");
        (view, buffer)
    }

    pub fn focus_view(&mut self, id: ViewId) {
        if !self.views.contains_key(&id) || !self.tab().root.contains(id) {
            return;
        }
        // Moving into the tree remembers where its files should open.
        let previous = self.focused_view();
        if let Some(tree) = &mut self.tree
            && self.views[&id].buffer == tree.buffer
            && previous != id
            && self.views.get(&previous).is_some_and(|v| v.buffer != tree.buffer)
        {
            tree.return_view = Some(previous);
        }
        self.tab_mut().focus = id;
        let buffer = self.views[&id].buffer;
        self.note_buffer_access(buffer);
        self.sync_mode_with_buffer();
    }

    /// Keep the mode consistent with the kind of buffer that has the focus:
    /// terminals start in terminal mode, text buffers never stay in it.
    pub fn sync_mode_with_buffer(&mut self) {
        let is_terminal = self.buffer().is_terminal();
        if is_terminal && self.mode.is_editing() {
            self.set_mode(Mode::Terminal);
        } else if !is_terminal && self.mode == Mode::Terminal {
            self.set_mode(Mode::Normal);
        }
    }

    /// Split the focused frame; the new frame shows `buffer` (default: the same
    /// buffer) and takes the focus.
    pub fn split(&mut self, dir: Dir, buffer: Option<BufferId>) -> ViewId {
        let focused = self.focused_view();
        let buffer = buffer.unwrap_or(self.views[&focused].buffer);
        let new = self.new_view(buffer);
        if buffer == self.views[&focused].buffer {
            let selection = self.views[&focused].selection.clone();
            let scroll = self.views[&focused].scroll_line;
            let view = self.views.get_mut(&new).expect("new view");
            view.selection = selection;
            view.scroll_line = scroll;
        }
        self.tab_mut().root.split(focused, dir, new);
        // Frames share the space equally after every split.
        self.tab_mut().root.reset_weights();
        self.focus_view(new);
        new
    }

    /// Add a frame along the right (or bottom) edge of the whole window.
    pub fn split_root(&mut self, dir: Dir, buffer: BufferId, fraction: f32) -> ViewId {
        let new = self.new_view(buffer);
        self.tab_mut().root.split_root(dir, new, fraction);
        self.focus_view(new);
        new
    }

    /// Close a frame. The last frame of the last tab stays open.
    pub fn close_view(&mut self, id: ViewId) {
        let tab_index = match self.tabs.iter().position(|t| t.root.contains(id)) {
            Some(i) => i,
            None => return,
        };
        let only_frame = self.tabs[tab_index].root.leaves().len() == 1;
        if only_frame {
            if self.tabs.len() == 1 {
                return;
            }
            self.tabs.remove(tab_index);
            self.tab_index = self.tab_index.min(self.tabs.len() - 1);
        } else {
            let tab = &mut self.tabs[tab_index];
            tab.root.remove(id);
            if tab.focus == id {
                tab.focus = tab.root.leaves()[0];
            }
        }
        self.views.remove(&id);
        // Frames go back to their default sizes when one of them closes.
        self.tab_mut().root.reset_weights();
        let focus = self.focused_view();
        self.focus_view(focus);
    }

    pub fn tab_new(&mut self, buffer: Option<BufferId>) {
        let buffer = buffer.unwrap_or(self.view().buffer);
        let view = self.new_view(buffer);
        self.tabs.insert(self.tab_index + 1, Tab::new(view));
        self.tab_index += 1;
        self.focus_view(view);
    }

    pub fn tab_close(&mut self) {
        if self.tabs.len() == 1 {
            self.set_error("cannot close the last tab");
            return;
        }
        let tab = self.tabs.remove(self.tab_index);
        for view in tab.root.leaves() {
            self.views.remove(&view);
        }
        self.tab_index = self.tab_index.min(self.tabs.len() - 1);
        let focus = self.focused_view();
        self.focus_view(focus);
    }

    pub fn tab_goto(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.tab_index = index;
            let focus = self.focused_view();
            self.focus_view(focus);
        }
    }

    // ----- Mode and messages -----

    pub fn set_mode(&mut self, mode: Mode) {
        if self.mode.is_editing() && !mode.is_editing() {
            self.buffer_mut().history.commit();
            if let Some(keys) = self.recording.take() {
                self.last_insert = keys;
            }
        }
        let mode = if mode.is_editing() && self.buffer().is_terminal() { Mode::Terminal } else { mode };
        if mode.is_editing() && self.buffer().readonly {
            self.set_error("buffer is read-only");
            return;
        }
        self.mode = mode;
        self.pending_keys.clear();
        self.count = None;
        self.pending_char = None;
    }

    pub fn set_message(&mut self, text: impl Into<String>) {
        self.message = Some(Message { text: text.into(), kind: MessageKind::Info });
    }

    pub fn set_error(&mut self, text: impl Into<String>) {
        self.message = Some(Message { text: text.into(), kind: MessageKind::Error });
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.styles = build_styles(&theme, &self.registry);
        self.theme = theme;
    }

    // ----- Editing -----

    /// Apply a transaction to the focused buffer, set the new selection of the
    /// focused view and keep the selections of other views in sync.
    pub fn apply(&mut self, tx: Transaction, selection: Selection) {
        let view_id = self.focused_view();
        let buffer_id = self.views[&view_id].buffer;
        self.apply_to(buffer_id, view_id, tx, selection);
    }

    pub fn apply_to(&mut self, buffer_id: BufferId, view_id: ViewId, tx: Transaction, selection: Selection) {
        if tx.is_empty() {
            self.views.get_mut(&view_id).expect("view").selection = selection;
            return;
        }
        let buffer = self.buffers.get_mut(&buffer_id).expect("buffer");
        if buffer.readonly {
            self.set_error("buffer is read-only");
            return;
        }
        let before = self.views[&view_id].selection.clone();
        let len = {
            buffer.apply(tx.clone(), &before, &selection);
            buffer.len_chars()
        };
        for view in self.views.values_mut() {
            if view.id == view_id {
                view.selection = selection.clamp(len);
            } else if view.buffer == buffer_id {
                view.selection = tx.map_selection(&view.selection).clamp(len);
            }
            if let Some(saved) = view.saved_selections.get_mut(&buffer_id) {
                *saved = tx.map_selection(saved).clamp(len);
            }
        }
    }

    /// Insert text at every cursor of the focused view (insert mode typing).
    pub fn insert_text(&mut self, text: &str) {
        let (view, buffer) = self.current();
        let mut tx = Transaction::new();
        let mut ranges = Vec::new();
        let mut delta = 0usize;
        let len = text.chars().count();
        for range in view.selection.iter() {
            let at = range.head.min(buffer.len_chars());
            tx.insert(at, text);
            ranges.push(Range::point(at + delta + len));
            delta += len;
        }
        let primary = view.selection.primary_index();
        self.apply(tx, Selection::from_ranges(ranges, primary));
    }

    /// Keep the cursor of the focused view inside the visible area.
    pub fn ensure_cursor_visible(&mut self) {
        let scrolloff = self.config.scrolloff;
        let tab_width = self.config.tab_width;
        let animation = self.config.scroll_animation;
        let (view, buffer) = self.current();
        let cursor = view.cursor().min(buffer.len_chars());
        let line = buffer.line_of(cursor);
        let rows = view.rows.max(1);
        // A terminal keeps its screen where it was: no context lines from the scrollback.
        let scrolloff = if buffer.is_terminal() { 0 } else { scrolloff.min(rows.saturating_sub(1) / 2) };
        let animation = if buffer.is_terminal() { 0 } else { animation };
        if line < view.scroll_line + scrolloff {
            view.scroll_to(line.saturating_sub(scrolloff), animation);
        } else if line + scrolloff >= view.scroll_line + rows {
            view.scroll_to(line + scrolloff + 1 - rows, animation);
        }
        let col = crate::movement::column_of(&buffer.text, cursor, tab_width);
        let cols = view.cols.max(1);
        if col < view.scroll_col {
            view.scroll_col = col;
        } else if col >= view.scroll_col + cols {
            view.scroll_col = col + 1 - cols;
        }
    }

    // ----- Terminals -----

    /// Create a terminal buffer. Fails if the shell cannot be started.
    pub fn new_terminal_buffer(&mut self) -> Result<BufferId, String> {
        let wake = self.wake.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
        let (cols, rows) = {
            let view = self.view();
            (view.cols.max(2) as u16, view.rows.max(2) as u16)
        };
        let terminal = Terminal::spawn(cols, rows, Some(&self.cwd), wake).map_err(|e| format!("cannot start terminal: {e}"))?;
        let id = BufferId(self.next_id());
        self.buffers.insert(id, Buffer::terminal(id, terminal));
        Ok(id)
    }

    /// Feed pending output of every terminal. Returns true if anything changed.
    pub fn process_terminals(&mut self) -> bool {
        let mut changed = false;
        let focused_buffer = self.view().buffer;
        let in_terminal_mode = self.mode == Mode::Terminal;
        for buffer in self.buffers.values_mut() {
            let Some(terminal) = &mut buffer.terminal else { continue };
            if terminal.process_output() {
                changed = true;
                // In normal mode the text mirror is what the user navigates.
                if !(in_terminal_mode && buffer.id == focused_buffer) {
                    let text = terminal.to_text();
                    buffer.set_text(&text);
                }
            }
        }
        if changed {
            let len = self.buffer().len_chars();
            let (view, _) = self.current();
            view.selection = view.selection.clamp(len);
        }
        changed
    }

    /// Any view still sliding its scroll or cursor (the window keeps redrawing).
    pub fn is_animating(&self) -> bool {
        self.views.values().any(|v| v.anim.is_some() || v.cursor_anim.is_some())
    }

    /// Move new global search hits into the results picker. Returns true when
    /// something changed (the caller redraws).
    pub fn poll_search(&mut self) -> bool {
        let Some(search) = &mut self.live_search else { return false };
        if search.done {
            return false;
        }
        let mut hits = Vec::new();
        loop {
            match search.results.try_recv() {
                Ok(hit) => hits.push(hit),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    search.done = true;
                    break;
                }
            }
        }
        let done = search.done;
        if hits.is_empty() && !done {
            return false;
        }
        crate::overlays::add_search_results(self, hits);
        true
    }

    /// Refresh the text mirror of a terminal buffer (when leaving terminal mode).
    pub fn refresh_terminal_text(&mut self, buffer_id: BufferId) {
        if let Some(buffer) = self.buffers.get_mut(&buffer_id) {
            let Some((text, (row, col))) = buffer.terminal.as_ref().map(|t| (t.to_text(), t.cursor())) else { return };
            {
                buffer.set_text(&text);
                let pos = buffer.line_start(row.min(buffer.line_count() - 1)) + col;
                let pos = pos.min(buffer.line_end(row.min(buffer.line_count() - 1)));
                for view in self.views.values_mut() {
                    if view.buffer == buffer_id {
                        view.selection = Selection::point(pos);
                    }
                }
            }
        }
    }

    // ----- Key handling -----

    pub fn handle_key(&mut self, key: Key) {
        self.message = None;
        if !self.replaying
            && let Some(recording) = &mut self.recording
        {
            recording.push(key);
        }
        match self.overlay {
            Overlay::Prompt(_) => crate::overlays::prompt_key(self, key),
            Overlay::Picker(_) => crate::overlays::picker_key(self, key),
            Overlay::None => match self.mode {
                Mode::Terminal => self.terminal_key(key),
                Mode::Insert | Mode::Replace => self.insert_key(key),
                Mode::Normal | Mode::Select => self.normal_key(key),
            },
        }
        if matches!(self.overlay, Overlay::None) && self.mode != Mode::Terminal {
            self.ensure_cursor_visible();
        }
    }

    fn terminal_key(&mut self, key: Key) {
        if let Some(KeyNode::Command(cmd)) = self.keymaps.terminal.get(key) {
            self.run_command(cmd, Ctx::default());
            return;
        }
        if let Some(terminal) = &mut self.buffer_mut().terminal {
            if terminal.exited() {
                // Any key closes a finished terminal.
                let id = self.view().buffer;
                let _ = self.close_buffer(id, true);
                self.set_message("terminal closed");
                return;
            }
            terminal.send_key(key);
        }
    }

    /// Insert and replace mode share the keymap; they differ in what typing does.
    fn insert_key(&mut self, key: Key) {
        if let Some(KeyNode::Command(cmd)) = self.keymaps.insert.get(key) {
            self.run_command(cmd, Ctx::default());
            return;
        }
        if let Some(c) = key.plain_char() {
            if self.mode == Mode::Replace {
                commands::overwrite_char(self, c);
            } else {
                commands::insert_char(self, c);
            }
        }
    }

    fn normal_key(&mut self, key: Key) {
        if let Some(pending) = self.pending_char.take() {
            if key.code == KeyCode::Escape {
                self.count = None;
                return;
            }
            if let Some(c) = key.plain_char() {
                let ctx = self.take_ctx();
                commands::pending_char(self, pending, c, ctx);
            }
            return;
        }
        // Digits build a count, except a leading `0`.
        if self.pending_keys.is_empty()
            && let Some(d) = key.plain_char().and_then(|c| c.to_digit(10))
            && (d != 0 || self.count.is_some())
        {
            self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(d as usize));
            return;
        }
        let keymap: &Keymap = match self.mode {
            Mode::Select => &self.keymaps.select,
            _ => &self.keymaps.normal,
        };
        let mut node = None;
        let mut map = keymap;
        for pending in &self.pending_keys {
            if let Some(KeyNode::Menu(m)) = map.get(*pending) {
                map = m;
            }
        }
        if let Some(found) = map.get(key) {
            node = Some(match found {
                KeyNode::Command(cmd) => Ok(*cmd),
                KeyNode::Menu(_) => Err(()),
            });
        }
        match node {
            Some(Ok(cmd)) => {
                let sequence = std::mem::take(&mut self.pending_keys);
                let ctx = self.take_ctx();
                self.run_command(cmd, ctx);
                if crate::keymap::REPEATABLE.contains(&cmd.name) {
                    self.pending_keys = sequence.clone();
                }
                if self.mode.is_editing() && self.recording.is_none() && !self.replaying {
                    let mut keys = sequence;
                    keys.push(key);
                    self.recording = Some(keys);
                }
            }
            Some(Err(())) => self.pending_keys.push(key),
            None => {
                if key.code != KeyCode::Escape && self.pending_keys.is_empty() && self.count.is_none() {
                    self.set_error(format!("key {key} is not bound"));
                }
                self.pending_keys.clear();
                self.count = None;
            }
        }
    }

    fn take_ctx(&mut self) -> Ctx {
        Ctx { count: self.count.take(), extend: self.mode == Mode::Select }
    }

    pub fn run_command(&mut self, cmd: &'static Command, ctx: Ctx) {
        (cmd.run)(self, ctx);
    }

    /// Replay the keys of the last insert or replace session (`.`).
    pub fn replay_last_insert(&mut self) {
        if self.last_insert.is_empty() || self.replaying {
            return;
        }
        let keys = self.last_insert.clone();
        self.replaying = true;
        for key in keys {
            self.handle_key(key);
        }
        self.replaying = false;
    }

    /// Text under the cursor used by `gf` and pickers: the primary selection if
    /// it is more than a cursor, else the word/path around it.
    pub fn primary_text_or_word(&self) -> String {
        let (view, buffer) = (self.view(), self.buffer());
        let primary = view.selection.primary();
        if primary.from() + 1 < primary.to() {
            return primary.slice(&buffer.text);
        }
        let text = &buffer.text;
        let len = text.len_chars();
        if len == 0 {
            return String::new();
        }
        let is_path_char = |c: char| c.is_alphanumeric() || "_-./~\\:".contains(c);
        let pos = primary.head.min(len - 1);
        let mut start = pos;
        while start > 0 && is_path_char(text.char(start - 1)) {
            start -= 1;
        }
        let mut end = pos;
        while end < len && is_path_char(text.char(end)) {
            end += 1;
        }
        text.slice(start..end).to_string()
    }

    /// Node of the current tab, for layout.
    pub fn root(&self) -> &Node {
        &self.tab().root
    }
}

pub fn build_styles(theme: &Theme, registry: &Registry) -> StyleTable {
    let styles = theme.style_table(registry);
    let mut replacements = vec![None; registry.scopes.len()];
    for lang in &registry.languages {
        for (scope, attrs) in &lang.styles {
            if let Some(text) = &attrs.replace {
                replacements[scope.0 as usize] = Some(text.clone());
            }
        }
    }
    StyleTable { styles, replacements }
}
