//! Terminal emulator: a pseudo-terminal running the user's shell, plus a grid of
//! cells that interprets its output (via the `vte` parser).
//!
//! The editor talks to this through a small API (`spawn`, `process_output`,
//! `send_key`, rows/cells for drawing). Output is read on a background thread and
//! parsed on the main thread when `process_output` is called.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use unicode_width::UnicodeWidthChar;

use crate::keymap::{Key, KeyCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermColor {
    Default,
    /// One of the 256 indexed colors (0-15 are the classic ANSI ones).
    Index(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: TermColor,
    pub bg: TermColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    /// First half of a double-width character.
    pub wide: bool,
    /// Second half of a double-width character (draws nothing).
    pub spacer: bool,
}

impl Cell {
    pub const BLANK: Cell = Cell {
        ch: ' ',
        fg: TermColor::Default,
        bg: TermColor::Default,
        bold: false,
        italic: false,
        underline: false,
        inverse: false,
        wide: false,
        spacer: false,
    };
}

pub type WakeFn = Arc<dyn Fn() + Send + Sync>;

/// Maximum number of rows kept above the screen.
const SCROLLBACK_LIMIT: usize = 10_000;

pub struct Terminal {
    screen: Screen,
    parser: vte::Parser,
    /// Output chunks from the reader thread. `None` once the thread has finished.
    output: Option<Receiver<Vec<u8>>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    exited: bool,
}

impl Terminal {
    /// Start the user's shell in a new pty of the given size. `wake` is called
    /// from a background thread whenever output arrives so the UI can redraw.
    pub fn spawn(cols: u16, rows: u16, cwd: Option<&Path>, wake: WakeFn) -> std::io::Result<Terminal> {
        let pty = native_pty_system();
        let pair = pty.openpty(pty_size(cols, rows)).map_err(to_io)?;

        let shell = default_shell();
        let mut command = CommandBuilder::new(&shell);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        if let Some(dir) = cwd {
            command.cwd(dir);
        }
        let child = pair.slave.spawn_command(command).map_err(to_io)?;
        // The slave side is only needed to spawn; dropping it lets us see EOF when
        // the child exits.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(to_io)?;
        let writer = pair.master.take_writer().map_err(to_io)?;
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sender.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                        wake();
                    }
                }
            }
            // Dropping the sender tells `process_output` that the shell is gone.
            drop(sender);
            wake();
        });

        let title = Path::new(&shell).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(shell);
        let mut screen = Screen::new(cols, rows);
        screen.title = title;
        Ok(Terminal { screen, parser: vte::Parser::new(), output: Some(receiver), writer, master: pair.master, child, exited: false })
    }

    /// Parse any output received since the last call. Returns true if the screen changed.
    pub fn process_output(&mut self) -> bool {
        let mut changed = false;
        let Some(receiver) = &self.output else { return false };
        loop {
            match receiver.try_recv() {
                Ok(bytes) => {
                    self.parser.advance(&mut self.screen, &bytes);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.output = None;
                    self.exited = true;
                    // The reader saw EOF, so the child is gone or going; reap it.
                    let _ = self.child.wait();
                    changed = true;
                    break;
                }
            }
        }
        self.flush_responses();
        changed
    }

    /// Send any replies the screen generated (cursor position reports...).
    fn flush_responses(&mut self) {
        if !self.screen.responses.is_empty() {
            let bytes = std::mem::take(&mut self.screen.responses);
            self.write(&bytes);
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.exited {
            return;
        }
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    pub fn send_key(&mut self, key: Key) {
        let encoding = KeyEncoding {
            app_cursor_keys: self.screen.app_cursor_keys,
            csi_u: self.screen.kitty_flags() & 1 != 0 || self.screen.modify_other_keys,
        };
        if let Some(bytes) = encode_key(key, encoding) {
            self.write(&bytes);
        }
    }

    /// Send text as if pasted.
    pub fn send_text(&mut self, text: &str) {
        if self.screen.bracketed_paste {
            let mut bytes = b"\x1b[200~".to_vec();
            bytes.extend_from_slice(text.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            self.write(&bytes);
        } else {
            self.write(text.as_bytes());
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) == self.size() || cols == 0 || rows == 0 {
            return;
        }
        self.screen.resize(cols as usize, rows as usize);
        let _ = self.master.resize(pty_size(cols, rows));
    }

    pub fn size(&self) -> (u16, u16) {
        (self.screen.cols as u16, self.screen.rows as u16)
    }

    pub fn exited(&self) -> bool {
        self.exited
    }

    /// The application asked for mouse clicks (`?1000h` and friends).
    pub fn wants_mouse(&self) -> bool {
        self.screen.mouse_mode != 0
    }

    /// The application wants to hear the mouse move: always (`?1003h`), or
    /// while a button is held (`?1002h`).
    pub fn wants_motion(&self, button_held: bool) -> bool {
        self.screen.mouse_mode == 1003 || (self.screen.mouse_mode == 1002 && button_held)
    }

    /// A full-screen application (vim, less...) is running.
    pub fn in_alt_screen(&self) -> bool {
        self.screen.in_alt_screen()
    }

    pub fn send_mouse(&mut self, event: MouseEvent) {
        if self.screen.mouse_mode != 0 {
            let bytes = encode_mouse(event, self.screen.mouse_sgr);
            self.write(&bytes);
        }
    }

    pub fn title(&self) -> &str {
        &self.screen.title
    }

    /// Number of rows including scrollback.
    pub fn total_rows(&self) -> usize {
        self.screen.total_rows()
    }

    /// Index (into `total_rows`) of the first row of the live screen.
    #[allow(dead_code)]
    pub fn screen_start(&self) -> usize {
        self.screen.screen_start()
    }

    pub fn row(&self, index: usize) -> &[Cell] {
        self.screen.row(index)
    }

    /// Cursor position as (absolute row, column).
    pub fn cursor(&self) -> (usize, usize) {
        (self.screen.screen_start() + self.screen.cursor.row, self.screen.cursor.col)
    }

    pub fn cursor_visible(&self) -> bool {
        self.screen.cursor_visible
    }

    /// Text of one row: spacer cells skipped, trailing blanks trimmed.
    pub fn row_text(&self, index: usize) -> String {
        let mut s: String = self.row(index).iter().filter(|c| !c.spacer).map(|c| c.ch).collect();
        let trimmed = s.trim_end().len();
        s.truncate(trimmed);
        s
    }

    /// The whole scrollback and screen as text, one line per row.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for i in 0..self.total_rows() {
            out.push_str(&self.row_text(i));
            out.push('\n');
        }
        out
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if !self.exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// portable-pty reports errors with its own type; the editor speaks `io::Error`.
fn to_io(err: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(err.to_string())
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
}

fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_string())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

// ---------------------------------------------------------------------------
// Key encoding
// ---------------------------------------------------------------------------

/// A mouse event to report to the application, in cells.
#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    /// 0 left, 1 middle, 2 right, 64 wheel up, 65 wheel down; 3 = no button (motion).
    pub button: u8,
    pub col: usize,
    pub row: usize,
    /// Release of the button (false for presses, wheel and motion).
    pub released: bool,
    pub motion: bool,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// xterm mouse reporting: SGR form (`CSI < code ; col ; row M`) when the
/// application enabled it (`?1006h`), else the old three-byte X10 form.
fn encode_mouse(event: MouseEvent, sgr: bool) -> Vec<u8> {
    let code = event.button
        + if event.motion { 32 } else { 0 }
        + if event.shift { 4 } else { 0 }
        + if event.alt { 8 } else { 0 }
        + if event.ctrl { 16 } else { 0 };
    if sgr {
        let kind = if event.released { 'm' } else { 'M' };
        format!("\x1b[<{code};{};{}{kind}", event.col + 1, event.row + 1).into_bytes()
    } else {
        let code = if event.released { 3 } else { code };
        let coord = |v: usize| (32 + v as u32 + 1).min(255) as u8;
        vec![0x1b, b'[', b'M', 32 + code, coord(event.col), coord(event.row)]
    }
}

/// How the application asked keys to be encoded.
#[derive(Debug, Clone, Copy, Default)]
struct KeyEncoding {
    app_cursor_keys: bool,
    /// The kitty keyboard protocol's "disambiguate" level (or xterm's
    /// modifyOtherKeys): modified keys that have no legacy encoding, such as
    /// Shift+Enter, go out as `CSI code;modifiers u`.
    csi_u: bool,
}

/// Bytes a terminal sends for `key`, following xterm conventions.
fn encode_key(key: Key, encoding: KeyEncoding) -> Option<Vec<u8>> {
    // Modifier parameter used by xterm: 1 + shift(1) + alt(2) + ctrl(4) + super(8).
    let modifier = 1 + (key.shift as u8) + (key.alt as u8) * 2 + (key.ctrl as u8) * 4 + (key.meta as u8) * 8;
    let app_cursor_keys = encoding.app_cursor_keys;
    if encoding.csi_u {
        // Keys the legacy encoding cannot tell apart from something else.
        let code = match key.code {
            KeyCode::Enter if modifier > 1 => Some(13),
            KeyCode::Tab if modifier > 2 => Some(9),
            KeyCode::Backspace if modifier > 1 => Some(127),
            KeyCode::Escape if modifier > 1 => Some(27),
            KeyCode::Char(c) if key.ctrl || key.alt || key.meta => Some(c.to_lowercase().next().unwrap_or(c) as u32),
            _ => None,
        };
        if let Some(code) = code {
            return Some(format!("\x1b[{code};{modifier}u").into_bytes());
        }
    }
    // Alt is sent as an ESC prefix for everything except the keys that encode
    // their modifiers as a parameter (arrows, function keys...).
    let alt_prefix = |mut bytes: Vec<u8>| -> Vec<u8> {
        if key.alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };
    let with_param = |letter: char, tilde: bool, number: u8| -> Vec<u8> {
        // Keys like "\x1b[3~" (number form) or "\x1b[A" (letter form).
        if tilde {
            if modifier > 1 { format!("\x1b[{number};{modifier}~") } else { format!("\x1b[{number}~") }
        } else if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}")
        } else if app_cursor_keys {
            format!("\x1bO{letter}")
        } else {
            format!("\x1b[{letter}")
        }
        .into_bytes()
    };

    let bytes = match key.code {
        KeyCode::Char(c) => {
            if key.ctrl {
                let byte = match c.to_ascii_lowercase() {
                    'a'..='z' => c.to_ascii_lowercase() as u8 - b'a' + 1,
                    ' ' | '@' | '2' => 0,
                    '[' | '3' => 0x1b,
                    '\\' | '4' => 0x1c,
                    ']' | '5' => 0x1d,
                    '^' | '6' => 0x1e,
                    '_' | '/' | '7' | '-' => 0x1f,
                    '?' | '8' => 0x7f,
                    _ => return None,
                };
                alt_prefix(vec![byte])
            } else {
                alt_prefix(c.to_string().into_bytes())
            }
        }
        KeyCode::Enter => alt_prefix(b"\r".to_vec()),
        KeyCode::Backspace => alt_prefix(if key.ctrl { b"\x08".to_vec() } else { b"\x7f".to_vec() }),
        KeyCode::Tab => {
            if key.shift {
                b"\x1b[Z".to_vec()
            } else {
                alt_prefix(b"\t".to_vec())
            }
        }
        KeyCode::Escape => alt_prefix(b"\x1b".to_vec()),
        KeyCode::Up => with_param('A', false, 0),
        KeyCode::Down => with_param('B', false, 0),
        KeyCode::Right => with_param('C', false, 0),
        KeyCode::Left => with_param('D', false, 0),
        KeyCode::Home => with_param('H', false, 0),
        KeyCode::End => with_param('F', false, 0),
        KeyCode::Insert => with_param(' ', true, 2),
        KeyCode::Delete => with_param(' ', true, 3),
        KeyCode::PageUp => with_param(' ', true, 5),
        KeyCode::PageDown => with_param(' ', true, 6),
        KeyCode::F(n @ 1..=4) => {
            let letter = (b'P' + n - 1) as char;
            if modifier > 1 { format!("\x1b[1;{modifier}{letter}") } else { format!("\x1bO{letter}") }.into_bytes()
        }
        KeyCode::F(n) => {
            let number = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                _ => return None,
            };
            with_param(' ', true, number)
        }
    };
    Some(bytes)
}

// ---------------------------------------------------------------------------
// Screen: the grid of cells and the escape-sequence interpreter
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Cursor {
    row: usize,
    col: usize,
    /// Set after printing in the last column: the next printed character goes to
    /// the start of the next line instead (xterm's "pending wrap").
    pending_wrap: bool,
}

#[derive(Debug, Clone, Copy)]
struct Attrs {
    fg: TermColor,
    bg: TermColor,
    bold: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

impl Attrs {
    const DEFAULT: Attrs =
        Attrs { fg: TermColor::Default, bg: TermColor::Default, bold: false, italic: false, underline: false, inverse: false };

    fn cell(&self, ch: char) -> Cell {
        Cell {
            ch,
            fg: self.fg,
            bg: self.bg,
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
            inverse: self.inverse,
            wide: false,
            spacer: false,
        }
    }
}

/// Everything saved by ESC 7 / restored by ESC 8.
#[derive(Debug, Clone, Copy)]
struct SavedCursor {
    cursor: Cursor,
    attrs: Attrs,
    graphics_charset: bool,
}

/// The visible grid plus scrollback, and the interpreter state.
struct Screen {
    cols: usize,
    rows: usize,
    /// Rows of the active screen (main or alternate), each `cols` long.
    lines: Vec<Vec<Cell>>,
    /// Rows that scrolled off the top of the main screen, oldest first.
    scrollback: VecDeque<Vec<Cell>>,
    /// The main screen and its cursor, kept aside while the alternate screen is active.
    saved_main: Option<(Vec<Vec<Cell>>, Cursor)>,
    cursor: Cursor,
    saved_cursor: Option<SavedCursor>,
    attrs: Attrs,
    /// Scroll region as inclusive row indices.
    scroll_top: usize,
    scroll_bottom: usize,
    /// ESC ( 0 selected the DEC line-drawing charset.
    graphics_charset: bool,
    app_cursor_keys: bool,
    /// Kitty keyboard protocol flag stack (`CSI > flags u` pushes, `CSI < u` pops).
    kitty_stack: Vec<u16>,
    /// xterm's `CSI > 4 ; 2 m`.
    modify_other_keys: bool,
    /// Mouse reporting: 0 off, 1000 clicks, 1002 clicks and drags, 1003 all motion.
    mouse_mode: u16,
    /// `?1006h`: report in the SGR form.
    mouse_sgr: bool,
    autowrap: bool,
    cursor_visible: bool,
    bracketed_paste: bool,
    title: String,
    /// Bytes to send back to the application (answers to queries).
    responses: Vec<u8>,
}

impl Screen {
    /// Kitty keyboard protocol flags in effect.
    fn kitty_flags(&self) -> u16 {
        self.kitty_stack.last().copied().unwrap_or(0)
    }

    fn new(cols: u16, rows: u16) -> Screen {
        let cols = cols.max(1) as usize;
        let rows = rows.max(1) as usize;
        Screen {
            cols,
            rows,
            lines: vec![vec![Cell::BLANK; cols]; rows],
            scrollback: VecDeque::new(),
            saved_main: None,
            cursor: Cursor { row: 0, col: 0, pending_wrap: false },
            saved_cursor: None,
            attrs: Attrs::DEFAULT,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            graphics_charset: false,
            app_cursor_keys: false,
            kitty_stack: Vec::new(),
            modify_other_keys: false,
            mouse_mode: 0,
            mouse_sgr: false,
            autowrap: true,
            cursor_visible: true,
            bracketed_paste: false,
            title: String::new(),
            responses: Vec::new(),
        }
    }

    fn in_alt_screen(&self) -> bool {
        self.saved_main.is_some()
    }

    fn total_rows(&self) -> usize {
        self.screen_start() + self.rows
    }

    fn screen_start(&self) -> usize {
        if self.in_alt_screen() { 0 } else { self.scrollback.len() }
    }

    fn row(&self, index: usize) -> &[Cell] {
        let start = self.screen_start();
        if index < start { &self.scrollback[index] } else { &self.lines[(index - start).min(self.rows - 1)] }
    }

    fn blank_row(&self) -> Vec<Cell> {
        vec![Cell::BLANK; self.cols]
    }

    // ----- Cursor movement -----

    fn move_to(&mut self, row: usize, col: usize) {
        self.cursor.row = row.min(self.rows - 1);
        self.cursor.col = col.min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    fn move_by(&mut self, rows: isize, cols: isize) {
        let row = (self.cursor.row as isize + rows).clamp(0, self.rows as isize - 1) as usize;
        let col = (self.cursor.col as isize + cols).clamp(0, self.cols as isize - 1) as usize;
        self.move_to(row, col);
    }

    /// Move down one row, scrolling if the cursor is at the bottom of the region.
    fn index(&mut self) {
        if self.cursor.row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor.row < self.rows - 1 {
            self.cursor.row += 1;
        }
        self.cursor.pending_wrap = false;
    }

    /// Move up one row, scrolling the region down if at its top.
    fn reverse_index(&mut self) {
        if self.cursor.row == self.scroll_top {
            self.scroll_down(1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
        self.cursor.pending_wrap = false;
    }

    fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    fn tab(&mut self) {
        let next = (self.cursor.col / 8 + 1) * 8;
        self.cursor.col = next.min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    // ----- Scrolling -----

    /// Scroll the region up `n` rows: the top rows disappear (into the scrollback
    /// when the region is the whole main screen) and blank rows appear at the bottom.
    fn scroll_up(&mut self, n: usize) {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        let keep_history = !self.in_alt_screen() && self.scroll_top == 0 && self.scroll_bottom == self.rows - 1;
        for _ in 0..n {
            let row = self.lines.remove(self.scroll_top);
            if keep_history {
                self.scrollback.push_back(row);
                if self.scrollback.len() > SCROLLBACK_LIMIT {
                    self.scrollback.pop_front();
                }
            }
            self.lines.insert(self.scroll_bottom, self.blank_row());
        }
    }

    /// Scroll the region down `n` rows: blank rows appear at the top.
    fn scroll_down(&mut self, n: usize) {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        for _ in 0..n {
            self.lines.remove(self.scroll_bottom);
            self.lines.insert(self.scroll_top, self.blank_row());
        }
    }

    // ----- Printing -----

    fn print_char(&mut self, c: char) {
        let c = if self.graphics_charset { line_drawing(c) } else { c };
        let width = c.width().unwrap_or(0);
        if width == 0 {
            // Combining marks and other zero-width characters are dropped.
            return;
        }
        if self.cursor.pending_wrap {
            if self.autowrap {
                self.carriage_return();
                self.index();
            } else {
                self.cursor.pending_wrap = false;
            }
        }
        // A wide character that does not fit at the end of the line wraps early.
        if width == 2 && self.cursor.col + 1 >= self.cols {
            if self.autowrap {
                self.carriage_return();
                self.index();
            } else {
                self.cursor.col = self.cols.saturating_sub(2);
            }
        }
        let (row, col) = (self.cursor.row, self.cursor.col);
        self.clear_wide_pair(row, col);
        let mut cell = self.attrs.cell(c);
        if width == 2 {
            cell.wide = true;
            self.clear_wide_pair(row, col + 1);
            let mut spacer = self.attrs.cell(' ');
            spacer.spacer = true;
            self.lines[row][col + 1] = spacer;
        }
        self.lines[row][col] = cell;
        if col + width >= self.cols {
            self.cursor.col = self.cols - 1;
            self.cursor.pending_wrap = true;
        } else {
            self.cursor.col = col + width;
        }
    }

    /// Overwriting half of a wide character blanks the other half too.
    fn clear_wide_pair(&mut self, row: usize, col: usize) {
        let cell = self.lines[row][col];
        if cell.wide && col + 1 < self.cols {
            self.lines[row][col + 1] = Cell::BLANK;
        } else if cell.spacer && col > 0 {
            self.lines[row][col - 1] = Cell::BLANK;
        }
    }

    // ----- Erasing and editing -----

    /// A blank cell carrying the current background color (as xterm does).
    fn erased_cell(&self) -> Cell {
        let mut cell = Cell::BLANK;
        cell.bg = self.attrs.bg;
        cell
    }

    fn erase_cells(&mut self, row: usize, from: usize, to: usize) {
        let cell = self.erased_cell();
        for col in from.min(self.cols)..to.min(self.cols) {
            self.lines[row][col] = cell;
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        match mode {
            0 => self.erase_cells(row, col, self.cols),
            1 => self.erase_cells(row, 0, col + 1),
            2 => self.erase_cells(row, 0, self.cols),
            _ => {}
        }
    }

    fn erase_in_display(&mut self, mode: u16) {
        let row = self.cursor.row;
        match mode {
            0 => {
                self.erase_in_line(0);
                for r in row + 1..self.rows {
                    self.erase_cells(r, 0, self.cols);
                }
            }
            1 => {
                for r in 0..row {
                    self.erase_cells(r, 0, self.cols);
                }
                self.erase_in_line(1);
            }
            2 => {
                for r in 0..self.rows {
                    self.erase_cells(r, 0, self.cols);
                }
            }
            3 => self.scrollback.clear(),
            _ => {}
        }
    }

    fn insert_lines(&mut self, n: usize) {
        if self.cursor.row < self.scroll_top || self.cursor.row > self.scroll_bottom {
            return;
        }
        let n = n.min(self.scroll_bottom - self.cursor.row + 1);
        for _ in 0..n {
            self.lines.remove(self.scroll_bottom);
            self.lines.insert(self.cursor.row, self.blank_row());
        }
        self.cursor.pending_wrap = false;
    }

    fn delete_lines(&mut self, n: usize) {
        if self.cursor.row < self.scroll_top || self.cursor.row > self.scroll_bottom {
            return;
        }
        let n = n.min(self.scroll_bottom - self.cursor.row + 1);
        for _ in 0..n {
            self.lines.remove(self.cursor.row);
            self.lines.insert(self.scroll_bottom, self.blank_row());
        }
        self.cursor.pending_wrap = false;
    }

    fn insert_chars(&mut self, n: usize) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        let n = n.min(self.cols - col);
        let cell = self.erased_cell();
        let line = &mut self.lines[row];
        line.truncate(self.cols - n);
        for _ in 0..n {
            line.insert(col, cell);
        }
        self.cursor.pending_wrap = false;
    }

    fn delete_chars(&mut self, n: usize) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        let n = n.min(self.cols - col);
        let cell = self.erased_cell();
        let line = &mut self.lines[row];
        line.drain(col..col + n);
        line.resize(self.cols, cell);
        self.cursor.pending_wrap = false;
    }

    // ----- Modes and screens -----

    fn enter_alt_screen(&mut self) {
        if self.in_alt_screen() {
            return;
        }
        let main = std::mem::replace(&mut self.lines, vec![vec![Cell::BLANK; self.cols]; self.rows]);
        self.saved_main = Some((main, self.cursor));
        self.move_to(0, 0);
    }

    fn leave_alt_screen(&mut self) {
        if let Some((main, cursor)) = self.saved_main.take() {
            self.lines = main;
            self.cursor = cursor;
        }
    }

    fn set_private_mode(&mut self, mode: u16, on: bool) {
        match mode {
            1 => self.app_cursor_keys = on,
            7 => self.autowrap = on,
            1000 | 1002 | 1003 => self.mouse_mode = if on { mode } else { 0 },
            1006 => self.mouse_sgr = on,
            25 => self.cursor_visible = on,
            47 | 1047 => {
                if on {
                    self.enter_alt_screen()
                } else {
                    self.leave_alt_screen()
                }
            }
            1049 => {
                if on {
                    self.save_cursor();
                    self.enter_alt_screen();
                } else {
                    self.leave_alt_screen();
                    self.restore_cursor();
                }
            }
            2004 => self.bracketed_paste = on,
            _ => {}
        }
    }

    fn save_cursor(&mut self) {
        self.saved_cursor = Some(SavedCursor { cursor: self.cursor, attrs: self.attrs, graphics_charset: self.graphics_charset });
    }

    fn restore_cursor(&mut self) {
        if let Some(saved) = self.saved_cursor {
            self.cursor = saved.cursor;
            self.attrs = saved.attrs;
            self.graphics_charset = saved.graphics_charset;
            self.move_to(saved.cursor.row, saved.cursor.col);
        } else {
            self.move_to(0, 0);
        }
    }

    fn reset(&mut self) {
        let (cols, rows) = (self.cols, self.rows);
        let title = std::mem::take(&mut self.title);
        *self = Screen::new(cols as u16, rows as u16);
        self.title = title;
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let resize_lines = |lines: &mut Vec<Vec<Cell>>| {
            for line in lines.iter_mut() {
                line.resize(cols, Cell::BLANK);
            }
            lines.resize(rows, vec![Cell::BLANK; cols]);
        };
        resize_lines(&mut self.lines);
        if let Some((main, cursor)) = &mut self.saved_main {
            resize_lines(main);
            cursor.row = cursor.row.min(rows - 1);
            cursor.col = cursor.col.min(cols - 1);
        }
        for line in self.scrollback.iter_mut() {
            line.resize(cols, Cell::BLANK);
        }
        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.move_to(self.cursor.row, self.cursor.col);
    }

    // ----- SGR -----

    fn set_graphics(&mut self, params: &vte::Params) {
        if params.is_empty() {
            self.attrs = Attrs::DEFAULT;
            return;
        }
        // Flatten "38;2;r;g;b" and "38:2:r:g:b" into one list of numbers.
        let values: Vec<u16> = params.iter().flat_map(|p| p.iter().copied()).collect();
        let mut i = 0;
        while i < values.len() {
            let v = values[i];
            match v {
                0 => self.attrs = Attrs::DEFAULT,
                1 => self.attrs.bold = true,
                2 => {} // dim: not rendered
                3 => self.attrs.italic = true,
                4 => self.attrs.underline = true,
                7 => self.attrs.inverse = true,
                9 => {} // strikethrough: not rendered
                22 => self.attrs.bold = false,
                23 => self.attrs.italic = false,
                24 => self.attrs.underline = false,
                27 => self.attrs.inverse = false,
                29 => {}
                30..=37 => self.attrs.fg = TermColor::Index((v - 30) as u8),
                39 => self.attrs.fg = TermColor::Default,
                40..=47 => self.attrs.bg = TermColor::Index((v - 40) as u8),
                49 => self.attrs.bg = TermColor::Default,
                90..=97 => self.attrs.fg = TermColor::Index((v - 90 + 8) as u8),
                100..=107 => self.attrs.bg = TermColor::Index((v - 100 + 8) as u8),
                38 | 48 => {
                    let (color, used) = extended_color(&values[i + 1..]);
                    if let Some(color) = color {
                        if v == 38 { self.attrs.fg = color } else { self.attrs.bg = color }
                    }
                    i += used;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// Kitty keyboard protocol levels we implement: 1 = disambiguate escape codes.
const KITTY_SUPPORTED: u16 = 1;

/// Parse the tail of a `38;...` / `48;...` SGR: `5;n` or `2;r;g;b`. Returns the
/// color and how many values were consumed.
fn extended_color(values: &[u16]) -> (Option<TermColor>, usize) {
    match values {
        [5, n, ..] => (Some(TermColor::Index(*n as u8)), 2),
        [2, r, g, b, ..] => (Some(TermColor::Rgb(*r as u8, *g as u8, *b as u8)), 4),
        _ => (None, 0),
    }
}

/// DEC special graphics (ESC ( 0): the letters TUIs use to draw boxes.
fn line_drawing(c: char) -> char {
    match c {
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'q' => '─',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'a' => '▒',
        '`' => '◆',
        _ => c,
    }
}

/// First parameter of a CSI sequence, defaulting to `default` when absent or zero.
fn param(params: &vte::Params, index: usize, default: u16) -> u16 {
    match params.iter().nth(index).and_then(|p| p.first()) {
        Some(&v) if v != 0 => v,
        _ => default,
    }
}

impl vte::Perform for Screen {
    fn print(&mut self, c: char) {
        self.print_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => self.move_by(0, -1),
            0x09 => self.tab(),
            0x0a..=0x0c => self.index(),
            0x0d => self.carriage_return(),
            _ => {} // BEL and the rest are ignored
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if let [kind, text, ..] = params
            && (*kind == b"0" || *kind == b"2")
        {
            self.title = String::from_utf8_lossy(text).into_owned();
        }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, action: char) {
        let n = param(params, 0, 1) as usize;
        // Sequences with a private marker (`?`, `>`, `<`, `=`) mean something
        // else entirely; they must never reach the plain handlers below.
        match (action, intermediates.first()) {
            ('m', Some(b'>')) => {
                // modifyOtherKeys: `CSI > 4 ; 2 m` enables it, `CSI > 4 m` resets it.
                if param(params, 0, 0) == 4 {
                    self.modify_other_keys = param(params, 1, 0) >= 1;
                }
                return;
            }
            ('u', Some(b'?')) => {
                // Kitty keyboard protocol query: report what is active.
                self.responses.extend(format!("\x1b[?{}u", self.kitty_flags()).into_bytes());
                return;
            }
            ('u', Some(b'>')) => {
                self.kitty_stack.push(param(params, 0, 0) & KITTY_SUPPORTED);
                return;
            }
            ('u', Some(b'<')) => {
                let count = param(params, 0, 1).max(1) as usize;
                let keep = self.kitty_stack.len().saturating_sub(count);
                self.kitty_stack.truncate(keep);
                return;
            }
            ('u', Some(b'=')) => {
                let flags = param(params, 0, 0) & KITTY_SUPPORTED;
                let current = self.kitty_stack.pop().unwrap_or(0);
                let new = match param(params, 1, 1) {
                    2 => current | flags,
                    3 => current & !flags,
                    _ => flags,
                };
                self.kitty_stack.push(new);
                return;
            }
            (_, Some(marker)) if *marker != b'?' => return, // other `>` `<` `=` sequences: queries we do not answer
            _ => {}
        }
        let private = intermediates.first() == Some(&b'?');
        match (action, private) {
            ('A', _) => self.move_by(-(n as isize), 0),
            ('B', _) | ('e', _) => self.move_by(n as isize, 0),
            ('C', _) | ('a', _) => self.move_by(0, n as isize),
            ('D', _) => self.move_by(0, -(n as isize)),
            ('E', _) => {
                self.move_by(n as isize, 0);
                self.carriage_return();
            }
            ('F', _) => {
                self.move_by(-(n as isize), 0);
                self.carriage_return();
            }
            ('G', _) | ('`', _) => self.move_to(self.cursor.row, n - 1),
            ('H', _) | ('f', _) => self.move_to(n - 1, param(params, 1, 1) as usize - 1),
            ('I', _) => {
                for _ in 0..n {
                    self.tab();
                }
            }
            ('J', _) => self.erase_in_display(param(params, 0, 0)),
            ('K', _) => self.erase_in_line(param(params, 0, 0)),
            ('L', _) => self.insert_lines(n),
            ('M', _) => self.delete_lines(n),
            ('P', _) => self.delete_chars(n),
            ('S', _) => self.scroll_up(n),
            ('T', _) => self.scroll_down(n),
            ('X', _) => self.erase_cells(self.cursor.row, self.cursor.col, self.cursor.col + n),
            ('Z', _) => {
                let col = self.cursor.col.saturating_sub(1) / 8 * 8;
                self.move_to(self.cursor.row, col);
            }
            ('@', _) => self.insert_chars(n),
            ('d', _) => self.move_to(n - 1, self.cursor.col),
            ('m', false) => self.set_graphics(params),
            ('r', false) => {
                let top = n - 1;
                let bottom = param(params, 1, self.rows as u16) as usize - 1;
                if top < bottom && bottom < self.rows {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                    self.move_to(0, 0);
                }
            }
            ('s', false) => self.save_cursor(),
            ('u', false) => self.restore_cursor(),
            ('n', false) => match param(params, 0, 0) {
                5 => self.responses.extend_from_slice(b"\x1b[0n"),
                6 => self.responses.extend(format!("\x1b[{};{}R", self.cursor.row + 1, self.cursor.col + 1).into_bytes()),
                _ => {}
            },
            ('c', false) => self.responses.extend_from_slice(b"\x1b[?6c"),
            ('h', true) | ('l', true) => {
                for p in params.iter() {
                    if let Some(&mode) = p.first() {
                        self.set_private_mode(mode, action == 'h');
                    }
                }
            }
            _ => {} // cursor style, window ops, unknown: ignored
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (intermediates.first(), byte) {
            (None, b'7') => self.save_cursor(),
            (None, b'8') => self.restore_cursor(),
            (None, b'D') => self.index(),
            (None, b'M') => self.reverse_index(),
            (None, b'E') => {
                self.index();
                self.carriage_return();
            }
            (None, b'c') => self.reset(),
            (Some(b'('), b'0') => self.graphics_charset = true,
            (Some(b'('), _) => self.graphics_charset = false,
            _ => {} // ESC ) ..., ESC =, ESC > and others: ignored
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed bytes through the parser into a fresh screen.
    fn screen(cols: u16, rows: u16, input: &str) -> Screen {
        let mut screen = Screen::new(cols, rows);
        feed(&mut screen, input);
        screen
    }

    fn feed(screen: &mut Screen, input: &str) {
        let mut parser = vte::Parser::new();
        parser.advance(screen, input.as_bytes());
    }

    fn row_text(screen: &Screen, index: usize) -> String {
        let s: String = screen.row(index).iter().filter(|c| !c.spacer).map(|c| c.ch).collect();
        s.trim_end().to_string()
    }

    #[test]
    fn prints_and_wraps() {
        let s = screen(5, 3, "abcdefg");
        assert_eq!(row_text(&s, 0), "abcde");
        assert_eq!(row_text(&s, 1), "fg");
        assert_eq!((s.cursor.row, s.cursor.col), (1, 2));
    }

    #[test]
    fn pending_wrap_keeps_cursor_on_line_until_next_char() {
        let mut s = screen(5, 3, "abcde");
        assert_eq!((s.cursor.row, s.cursor.col), (0, 4));
        assert!(s.cursor.pending_wrap);
        feed(&mut s, "\r\nx");
        assert_eq!(row_text(&s, 1), "x");
    }

    #[test]
    fn cr_lf_and_scrollback() {
        let s = screen(10, 2, "one\r\ntwo\r\nthree\r\nfour");
        assert_eq!(s.scrollback.len(), 2);
        assert_eq!(s.total_rows(), 4);
        assert_eq!(s.screen_start(), 2);
        assert_eq!(row_text(&s, 0), "one");
        assert_eq!(row_text(&s, 1), "two");
        assert_eq!(row_text(&s, 2), "three");
        assert_eq!(row_text(&s, 3), "four");
    }

    #[test]
    fn mouse_modes_and_encoding() {
        let mut s = screen(10, 2, "\x1b[?1002h\x1b[?1006h");
        assert_eq!((s.mouse_mode, s.mouse_sgr), (1002, true));
        let click = MouseEvent { button: 0, col: 4, row: 1, released: false, motion: false, shift: false, alt: false, ctrl: false };
        assert_eq!(encode_mouse(click, true), b"\x1b[<0;5;2M");
        assert_eq!(encode_mouse(MouseEvent { released: true, ..click }, true), b"\x1b[<0;5;2m");
        assert_eq!(encode_mouse(MouseEvent { button: 65, ctrl: true, ..click }, true), b"\x1b[<81;5;2M");
        assert_eq!(encode_mouse(MouseEvent { motion: true, ..click }, true), b"\x1b[<32;5;2M");
        assert_eq!(encode_mouse(click, false), b"\x1b[M\x20\x25\x22");
        feed(&mut s, "\x1b[?1002l");
        assert_eq!(s.mouse_mode, 0);
    }

    #[test]
    fn private_csi_sequences_do_not_leak_into_sgr() {
        let mut s = screen(20, 2, "\x1b[>4m\x1b[>0q\x1b[?u\x1b[<u\x1b[>4;2mplain");
        assert!(!s.row(0)[0].underline && !s.row(0)[0].bold, "modifyOtherKeys is not an SGR");
        assert_eq!(s.responses, b"\x1b[?0u", "the kitty query is answered with the flags in effect");
        assert!(s.modify_other_keys);
        s.responses.clear();
        feed(&mut s, "\x1b[>1u\x1b[?u");
        assert_eq!(s.kitty_flags(), 1);
        assert_eq!(s.responses, b"\x1b[?1u");
        feed(&mut s, "\x1b[<u");
        assert_eq!(s.kitty_flags(), 0);
    }

    #[test]
    fn shift_enter_is_only_special_when_asked_for() {
        let shift_enter = Key { shift: true, ..Key::code(KeyCode::Enter) };
        let plain = KeyEncoding::default();
        let csi_u = KeyEncoding { csi_u: true, ..plain };
        assert_eq!(encode_key(shift_enter, plain).unwrap(), b"\r");
        assert_eq!(encode_key(shift_enter, csi_u).unwrap(), b"\x1b[13;2u");
        assert_eq!(encode_key(Key::code(KeyCode::Enter), csi_u).unwrap(), b"\r", "a plain enter stays a plain enter");
        assert_eq!(encode_key(Key::char('c').ctrl(), csi_u).unwrap(), b"\x1b[99;5u");
        assert_eq!(encode_key(Key::char('c').ctrl(), plain).unwrap(), b"\x03");
        assert_eq!(encode_key(Key::char('A'), csi_u).unwrap(), b"A", "shifted text is still text");
    }

    #[test]
    fn sgr_colors_and_attributes() {
        let s = screen(20, 2, "\x1b[1;31mred\x1b[0m \x1b[38;5;200mx\x1b[48;2;1;2;3my\x1b[38:2:9:8:7mz");
        let row = s.row(0);
        assert!(row[0].bold);
        assert_eq!(row[0].fg, TermColor::Index(1));
        assert!(!row[3].bold);
        assert_eq!(row[4].fg, TermColor::Index(200));
        assert_eq!(row[5].bg, TermColor::Rgb(1, 2, 3));
        assert_eq!(row[6].fg, TermColor::Rgb(9, 8, 7));
    }

    #[test]
    fn cursor_movement() {
        let mut s = screen(10, 5, "\x1b[3;4H");
        assert_eq!((s.cursor.row, s.cursor.col), (2, 3));
        feed(&mut s, "\x1b[2A\x1b[3C");
        assert_eq!((s.cursor.row, s.cursor.col), (0, 6));
        feed(&mut s, "\x1b[10B\x1b[G");
        assert_eq!((s.cursor.row, s.cursor.col), (4, 0));
        feed(&mut s, "\x1b[2d");
        assert_eq!(s.cursor.row, 1);
    }

    #[test]
    fn erase_line_and_display() {
        let mut s = screen(6, 3, "aaaaaa\r\nbbbbbb\r\ncccccc");
        feed(&mut s, "\x1b[2;3H\x1b[K");
        assert_eq!(row_text(&s, 1), "bb");
        feed(&mut s, "\x1b[J");
        assert_eq!(row_text(&s, 2), "");
        assert_eq!(row_text(&s, 0), "aaaaaa");
        feed(&mut s, "\x1b[2J");
        assert_eq!(row_text(&s, 0), "");
    }

    #[test]
    fn scroll_region_does_not_touch_scrollback() {
        let mut s = screen(5, 4, "a\r\nb\r\nc\r\nd");
        feed(&mut s, "\x1b[2;3r\x1b[3;1H\n");
        assert_eq!(s.scrollback.len(), 0);
        assert_eq!(row_text(&s, 0), "a");
        assert_eq!(row_text(&s, 1), "c");
        assert_eq!(row_text(&s, 2), "");
        assert_eq!(row_text(&s, 3), "d");
    }

    #[test]
    fn insert_and_delete_lines_and_chars() {
        let mut s = screen(6, 3, "one\r\ntwo\r\nthree\x1b[1;1H\x1b[L");
        assert_eq!(row_text(&s, 0), "");
        assert_eq!(row_text(&s, 1), "one");
        assert_eq!(row_text(&s, 2), "two");
        feed(&mut s, "\x1b[M");
        assert_eq!(row_text(&s, 0), "one");
        feed(&mut s, "\x1b[1;2H\x1b[P");
        assert_eq!(row_text(&s, 0), "oe");
        feed(&mut s, "\x1b[2@");
        assert_eq!(row_text(&s, 0), "o  e");
    }

    #[test]
    fn alt_screen_is_separate_and_restored() {
        let mut s = screen(5, 2, "x\r\ny\r\nz");
        feed(&mut s, "\x1b[?1049h");
        assert_eq!(s.total_rows(), 2);
        assert_eq!(s.screen_start(), 0);
        assert_eq!(row_text(&s, 0), "");
        feed(&mut s, "alt\x1b[?1049l");
        assert_eq!(s.total_rows(), 3);
        assert_eq!(row_text(&s, 1), "y");
        assert_eq!(row_text(&s, 2), "z");
        assert_eq!((s.cursor.row, s.cursor.col), (1, 1));
    }

    #[test]
    fn wide_chars_take_two_cells() {
        let s = screen(4, 2, "a漢b漢");
        let row = s.row(0);
        assert!(row[1].wide);
        assert!(row[2].spacer);
        assert_eq!(row[3].ch, 'b');
        // The second wide char did not fit in the last column and wrapped.
        assert_eq!(row_text(&s, 1), "漢");
        assert_eq!(row_text(&s, 0), "a漢b");
    }

    #[test]
    fn resize_keeps_content_and_cursor_in_range() {
        let mut s = screen(5, 3, "hello\r\nworld\x1b[2;5H");
        s.resize(3, 2);
        assert_eq!(s.rows, 2);
        assert_eq!(s.cols, 3);
        assert_eq!(row_text(&s, 0), "hel");
        assert_eq!((s.cursor.row, s.cursor.col), (1, 2));
        s.resize(8, 4);
        assert_eq!(s.lines.len(), 4);
        assert_eq!(s.lines[0].len(), 8);
        assert_eq!(s.scroll_bottom, 3);
    }

    #[test]
    fn line_drawing_charset_and_title() {
        let s = screen(5, 1, "\x1b(0lqk\x1b(B\x1b]2;hi\x07");
        assert_eq!(row_text(&s, 0), "┌─┐");
        assert_eq!(s.title, "hi");
    }

    #[test]
    fn cursor_position_report() {
        let s = screen(5, 2, "ab\x1b[6n");
        assert_eq!(s.responses, b"\x1b[1;3R");
    }

    #[test]
    fn modes() {
        let mut s = screen(5, 2, "\x1b[?25l\x1b[?2004h\x1b[?1h");
        assert!(!s.cursor_visible);
        assert!(s.bracketed_paste);
        assert!(s.app_cursor_keys);
        feed(&mut s, "\x1b[?7l");
        feed(&mut s, "abcdefg");
        assert_eq!(row_text(&s, 0), "abcdg");
    }

    #[test]
    fn key_encoding() {
        let enc = |k: Key| encode_key(k, KeyEncoding::default()).unwrap();
        assert_eq!(enc(Key::char('a')), b"a");
        assert_eq!(enc(Key::char('a').ctrl()), vec![1]);
        assert_eq!(enc(Key::char('x').alt()), b"\x1bx");
        assert_eq!(enc(Key::code(KeyCode::Enter)), b"\r");
        assert_eq!(enc(Key::code(KeyCode::Backspace)), b"\x7f");
        assert_eq!(enc(Key::code(KeyCode::Up)), b"\x1b[A");
        let app = KeyEncoding { app_cursor_keys: true, ..KeyEncoding::default() };
        assert_eq!(encode_key(Key::code(KeyCode::Up), app).unwrap(), b"\x1bOA");
        assert_eq!(enc(Key::code(KeyCode::Right).ctrl()), b"\x1b[1;5C");
        assert_eq!(enc(Key::code(KeyCode::Delete)), b"\x1b[3~");
        assert_eq!(enc(Key::code(KeyCode::Delete).alt()), b"\x1b[3;3~");
        assert_eq!(enc(Key { shift: true, ..Key::code(KeyCode::Tab) }), b"\x1b[Z");
        assert_eq!(enc(Key::code(KeyCode::F(1))), b"\x1bOP");
        assert_eq!(enc(Key::code(KeyCode::F(5))), b"\x1b[15~");
        assert_eq!(enc(Key::char(' ').ctrl()), vec![0]);
    }
}
