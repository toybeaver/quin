//! quin: a small, fast, keyboard-driven code editor with a native GUI.
//!
//! This file holds the window glue (winit + softbuffer) and the command line.
//! The editor itself lives in `editor.rs`; drawing in `render/`.

mod buffer;
mod clipboard;
mod commands;
mod config;
mod editor;
mod files;
mod frame;
mod git;
mod history;
mod images;
mod install;
mod keymap;
mod links;
mod media;
mod movement;
mod overlays;
mod picker;
mod render;
mod search;
mod selection;
mod signs;
mod syntax;
mod terminal;
#[cfg(test)]
mod tests;
mod theme;
mod tree;
mod typed;
mod welcome;

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::data_transfer::TypeHint;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, AsyncRequestSerial, ControlFlow, DndAction, EventLoop, EventLoopProxy, OwnedDisplayHandle};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use winit::window::{ImeCapabilities, ImeEnableRequest, ImeRequest, ImeRequestData, Window, WindowAttributes, WindowId};

use crate::editor::{Editor, Mode};
use crate::keymap::{Key, KeyCode};
use crate::render::font::FontSet;
use crate::render::ui::Ui;
use crate::render::{Painter, Rect};
use crate::selection::{Range, Selection};

const USAGE: &str = "\
quin - a small, fast, keyboard-driven code editor

usage:
  quin [FILE|DIRECTORY ...]            open files (a directory becomes the working directory)
  quin convert textmate <grammar.json> [out.syn]
  quin convert treesitter <name> <highlights.scm> [out.syn]
  quin --screenshot <out.ppm> [--size WxH] [--keys \"<keys>\"] [FILE ...]
  quin install                         copy this binary to ~/.local/bin and add a desktop entry
  quin --help | --version

Press `:` inside the editor for the command palette, or `:help` for all keys.";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("--help") | Some("-h") => {
            println!("{USAGE}");
            return;
        }
        Some("--version") | Some("-V") => {
            println!("quin {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("install") => {
            if let Err(err) = install::run_cli() {
                eprintln!("quin: {err}");
                std::process::exit(1);
            }
            return;
        }
        Some("convert") => {
            if let Err(err) = syntax::convert::run_cli(&args[2..]) {
                eprintln!("quin: {err}");
                std::process::exit(1);
            }
            return;
        }
        _ => {}
    }

    let mut screenshot: Option<PathBuf> = None;
    let mut size = (1200u32, 800u32);
    let mut keys = String::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--screenshot" => {
                screenshot = args.get(i + 1).map(PathBuf::from);
                i += 1;
            }
            "--size" => {
                if let Some((w, h)) = args.get(i + 1).and_then(|s| s.split_once('x')) {
                    size = (w.parse().unwrap_or(1200), h.parse().unwrap_or(800));
                }
                i += 1;
            }
            "--keys" => {
                keys = args.get(i + 1).cloned().unwrap_or_default();
                i += 1;
            }
            other => paths.push(PathBuf::from(other)),
        }
        i += 1;
    }

    let mut editor = build_editor(&paths);
    if let Some(path) = screenshot {
        run_headless(&mut editor, &path, size, &keys);
        return;
    }
    run_window(editor);
}

/// Load configuration, syntax files, theme, and open the files given on the command line.
fn build_editor(paths: &[PathBuf]) -> Editor {
    let config = config::Config::load();
    let config_dir = config::config_dir();
    let registry = syntax::Registry::load(config_dir.as_ref().map(|d| d.join("syntax")).as_deref());
    let theme = load_theme(&config.theme, config_dir.as_deref()).unwrap_or_else(|| {
        eprintln!("quin: unknown theme `{}`, using dark", config.theme);
        theme::Theme::builtin("bright-lights").expect("dark theme exists")
    });
    let mut editor = Editor::new(config, registry, theme);

    // A directory argument sets the working directory; files are opened in order.
    let mut opened = Vec::new();
    for path in paths {
        if path.is_dir() {
            if std::env::set_current_dir(path).is_ok() {
                editor.cwd = buffer::absolute(path);
                welcome::remember(path);
            }
        } else {
            match editor.open_file(path) {
                Ok(id) => {
                    opened.push(id);
                    welcome::remember(path);
                }
                Err(err) => eprintln!("quin: {err}"),
            }
        }
    }
    let scratch = editor.view().buffer;
    match opened.first() {
        Some(first) => editor.show_buffer(*first),
        // Nothing to edit yet: the welcome buffer with the recent entries.
        None => welcome::open(&mut editor),
    }
    let _ = editor.close_buffer(scratch, true);
    editor
}

fn load_theme(name: &str, config_dir: Option<&Path>) -> Option<theme::Theme> {
    if let Some(dir) = config_dir {
        let path = dir.join("themes").join(format!("{name}.theme"));
        if let Ok(source) = std::fs::read_to_string(&path) {
            return Some(theme::Theme::parse(name, &source));
        }
    }
    theme::Theme::builtin(name)
}

/// The configured font file or family, else the embedded Iosevka. Records
/// the family used on the editor (for the config template).
fn load_font(editor: &mut Editor, scale: f64) -> FontSet {
    let config = &editor.config;
    let size = (config.font_size as f64 * scale) as f32;
    let read = |path: &Option<PathBuf>| path.as_ref().and_then(|p| std::fs::read(p).ok());
    let family_of = |path: &Path| path.file_stem().and_then(|s| s.to_str()).unwrap_or("").trim_end_matches("-Regular").to_string();
    let found = match (&config.font, &config.font_family) {
        (Some(path), _) => {
            Some(render::font::FamilyFiles { regular: path.clone(), bold: config.bold_font.clone(), italic: None, bold_italic: None })
        }
        (None, Some(family)) => {
            let found = render::font::find_font_family(family);
            if found.is_none() {
                eprintln!("quin: font family `{family}` not found, using the built-in Iosevka");
            }
            found
        }
        (None, None) => None,
    };
    let builtin = || (FontSet::builtin(size), "Iosevka".to_string());
    let (mut font, family) = match found {
        Some(files) => match std::fs::read(&files.regular) {
            Ok(regular) => {
                let bytes = render::font::FontFiles {
                    regular,
                    bold: read(&files.bold),
                    italic: read(&files.italic),
                    bold_italic: read(&files.bold_italic),
                };
                match FontSet::load(bytes, size) {
                    Ok(font) => (font, family_of(&files.regular)),
                    Err(err) => {
                        eprintln!("quin: cannot load font {}: {err}", files.regular.display());
                        builtin()
                    }
                }
            }
            Err(_) => builtin(),
        },
        None => builtin(),
    };
    font.ligatures = config.ligatures;
    editor.font_family = family;
    font
}

/// Render once to a PPM file without opening a window (used for testing).
fn run_headless(editor: &mut Editor, path: &Path, size: (u32, u32), keys: &str) {
    let mut font = load_font(editor, 1.0);
    editor.poll_git(true);
    let mut pixels = vec![0u32; (size.0 * size.1) as usize];
    // Draw once so that frame sizes are known before keys are replayed.
    {
        let painter = Painter::new(&mut pixels, size.0 as usize, size.1 as usize);
        let mut ui = Ui { painter, font: &mut font };
        render::ui::draw(editor, &mut ui);
    }
    for token in keys.split_whitespace() {
        if let Some(text) = token.strip_prefix("text:") {
            for c in text.chars() {
                editor.handle_key(Key::char(c));
            }
        } else if token == "wait" {
            // Let shells print their prompt and searches finish.
            wait_for_background(editor);
        } else if let Some(key) = Key::parse(token) {
            editor.handle_key(key);
        } else {
            eprintln!("quin: unknown key `{token}`");
        }
        editor.process_terminals();
        editor.poll_search();
    }
    // Draw so that images and git signs on screen start loading, then wait
    // for them; twice, since git signs first read the file at HEAD, then diff.
    for _ in 0..2 {
        let painter = Painter::new(&mut pixels, size.0 as usize, size.1 as usize);
        let mut ui = Ui { painter, font: &mut font };
        render::ui::draw(editor, &mut ui);
        wait_for_background(editor);
    }
    editor.views.values_mut().for_each(|v| {
        v.anim = None;
        v.cursor_anim = None;
        // Loaded images moved lines: the cursor must not slide from where it was.
        v.cursor_px = None;
    });
    let painter = Painter::new(&mut pixels, size.0 as usize, size.1 as usize);
    let mut ui = Ui { painter, font: &mut font };
    render::ui::draw(editor, &mut ui);
    if let Err(err) = std::fs::write(path, ui.painter.to_ppm()) {
        eprintln!("quin: cannot write {}: {err}", path.display());
    }
}

/// Headless mode has no event loop: give terminals and searches a moment.
fn wait_for_background(editor: &mut Editor) {
    if editor.buffers.values().any(|b| b.is_terminal()) {
        std::thread::sleep(std::time::Duration::from_millis(400));
        editor.process_terminals();
    }
    for _ in 0..200 {
        editor.poll_search();
        editor.poll_git(false);
        editor.images.poll();
        editor.signs.poll();
        if editor.live_search.as_ref().is_none_or(|s| s.done)
            && !editor.git.in_flight()
            && editor.git.remote.is_none()
            && !editor.images.loading()
            && !editor.signs.busy()
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

/// Animations (scroll and cursor slides) draw at most one frame this often.
/// Without a limit they would redraw as fast as the CPU allows, flooding the
/// compositor and starving input.
const FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

fn run_window(editor: Editor) {
    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let app = App::new(editor, proxy);
    if let Err(err) = event_loop.run_app(app) {
        eprintln!("quin: {err}");
    }
}

struct App {
    editor: Editor,
    font: FontSet,
    window: Option<Rc<dyn Window>>,
    surface: Option<softbuffer::Surface<OwnedDisplayHandle, Rc<dyn Window>>>,
    modifiers: ModifiersState,
    mouse: (i32, i32),
    /// Active mouse drag: the frame and the anchor position.
    drag: Option<(frame::ViewId, usize)>,
    /// Button held over a terminal application that reports the mouse, and
    /// the last cell reported to it.
    term_button: Option<u8>,
    term_cell: (usize, usize),
    /// Left button pressed on a terminal that does not report the mouse (and
    /// where): dragging from there selects text.
    term_press: Option<(frame::ViewId, (i32, i32))>,
    scale: f64,
    close_requested: bool,
    /// Part of a line scrolled but not applied yet (touchpads scroll by pixels).
    scroll_rest: f32,
    /// When the next paced frame is due: an animation runs, or background
    /// work (terminal output, search results) changed something.
    next_frame: Option<std::time::Instant>,
    /// When the last frame started.
    last_frame: std::time::Instant,
    /// What was last given to the window, to skip calls that change nothing.
    title: String,
    surface_size: (u32, u32),
    /// Files or an image dragged over the window: the kind of data to fetch
    /// on drop, and the frame and line under the pointer.
    dnd: Option<TypeHint>,
    dnd_at: Option<(frame::ViewId, usize)>,
    /// A drop waiting for its data: the request, the kind, where it landed.
    dropped: Option<(AsyncRequestSerial, TypeHint, frame::ViewId, usize)>,
}

/// What can be dropped on the window, best first: files, then image data.
const DROP_KINDS: [TypeHint; 3] =
    [TypeHint::UriList, TypeHint::Image { extension_hint: Some("png") }, TypeHint::Image { extension_hint: Some("jpg") }];

impl App {
    fn new(mut editor: Editor, proxy: EventLoopProxy) -> App {
        editor.wake = Some(Arc::new(move || proxy.wake_up()));
        let font = load_font(&mut editor, 1.0);
        App {
            editor,
            font,
            window: None,
            surface: None,
            modifiers: ModifiersState::empty(),
            mouse: (0, 0),
            drag: None,
            term_button: None,
            term_cell: (0, 0),
            term_press: None,
            scale: 1.0,
            close_requested: false,
            scroll_rest: 0.0,
            next_frame: None,
            last_frame: std::time::Instant::now(),
            title: String::new(),
            surface_size: (0, 0),
            dnd: None,
            dnd_at: None,
            dropped: None,
        }
    }

    fn redraw(&mut self) {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else { return };
        let size = window.surface_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        if self.surface_size != (size.width, size.height) {
            if surface.resize(w, h).is_err() {
                return;
            }
            self.surface_size = (size.width, size.height);
        }
        let started = std::time::Instant::now();
        self.last_frame = started;
        let Ok(mut buffer) = surface.buffer_mut() else { return };
        {
            let painter = Painter::new(&mut buffer, size.width as usize, size.height as usize);
            let mut ui = Ui { painter, font: &mut self.font };
            render::ui::draw(&mut self.editor, &mut ui);
        }
        // Lets Wayland pace the next frame to the display.
        window.pre_present_notify();
        let _ = buffer.present();
        let title = format!("{} - quin", self.editor.buffer().display_name());
        if title != self.title {
            window.set_title(&title);
            self.title = title;
        }
        self.next_frame = self.editor.is_animating().then(|| started + FRAME_INTERVAL);
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Redraw for background work, at most once per `FRAME_INTERVAL`: a shell
    /// printing without pause must not take every cycle away from input.
    fn request_frame(&mut self) {
        let due = self.last_frame + FRAME_INTERVAL;
        if std::time::Instant::now() >= due {
            self.request_redraw();
        } else {
            self.next_frame = Some(self.next_frame.map_or(due, |at| at.min(due)));
        }
    }

    /// Things to check after any input: quitting and redrawing.
    fn after_input(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.editor.poll_search();
        self.editor.poll_git(false);
        if self.editor.font_dirty {
            self.editor.font_dirty = false;
            self.font = load_font(&mut self.editor, self.scale);
        }
        if self.editor.quit {
            event_loop.exit();
        }
        self.request_redraw();
    }

    /// A drag moved to a pixel: find the frame and line under it and show
    /// what a drop there would do.
    fn drag_to(&mut self, x: f64, y: f64) {
        let (x, y) = (x as i32, y as i32);
        let at = self.frame_at(x, y).and_then(|(view, rect)| {
            let pos = render::ui::position_at(&mut self.editor, &self.font, view, rect, x, y)?;
            Some((view, self.editor.buffers[&self.editor.views[&view].buffer].line_of(pos)))
        });
        self.dnd_at = at;
        let hover = at.and_then(|(view, line)| media::hover(&self.editor, view, line));
        if hover != self.editor.drop_hover {
            if let Some(hover) = &hover {
                self.editor.set_message(hover.message.clone());
            }
            self.editor.drop_hover = hover;
            self.request_redraw();
        }
    }

    /// The drag left the window or ended: no more highlight.
    fn drag_ended(&mut self) {
        self.dnd = None;
        if self.editor.drop_hover.take().is_some() {
            self.editor.message = None;
            self.request_redraw();
        }
    }

    fn frame_at(&self, x: i32, y: i32) -> Option<(frame::ViewId, Rect)> {
        self.editor.frame_rects.iter().copied().find(|(_, r)| r.contains(x, y))
    }

    fn terminal_wants_mouse(&self) -> bool {
        self.editor.buffer().terminal.as_ref().is_some_and(|t| t.wants_mouse())
    }

    /// Report a mouse event to the terminal application in the focused frame.
    fn send_mouse(&mut self, button: u8, released: bool, motion: bool) {
        let (x, y) = self.mouse;
        let focused = self.editor.focused_view();
        let Some((_, rect)) = self.editor.frame_rects.iter().copied().find(|(id, _)| *id == focused) else { return };
        let (col, row) = render::ui::terminal_cell_at(&self.font, rect, x, y);
        let view = self.editor.view_mut();
        let (col, row) = (col.min(view.cols.saturating_sub(1)), row.min(view.rows.saturating_sub(1)));
        view.scrolled_back = false;
        self.term_cell = (col, row);
        let modifiers = self.modifiers;
        let event = terminal::MouseEvent {
            button,
            col,
            row,
            released,
            motion,
            shift: modifiers.shift_key(),
            alt: modifiers.alt_key(),
            ctrl: modifiers.control_key(),
        };
        if let Some(terminal) = &mut self.editor.buffer_mut().terminal {
            terminal.send_mouse(event);
        }
    }

    fn mouse_button(&mut self, button: MouseButton, pressed: bool) {
        let (x, y) = self.mouse;
        let code = match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            _ => return,
        };
        if !pressed {
            self.drag = None;
            self.term_press = None;
            if let Some(held) = self.term_button.take() {
                self.send_mouse(held, true, false);
            }
            return;
        }
        if code == 0
            && let Some(i) = self.editor.tab_rects.iter().position(|r| r.contains(x, y))
        {
            self.editor.tab_goto(i);
            return;
        }
        let Some((view_id, rect)) = self.frame_at(x, y) else { return };
        if !matches!(self.editor.overlay, editor::Overlay::None) {
            return;
        }
        self.editor.focus_view(view_id);
        if self.editor.mode == Mode::Terminal {
            // An application that asked for the mouse gets the click; in a
            // plain shell, dragging from here selects text.
            if self.terminal_wants_mouse() {
                self.term_button = Some(code);
                self.send_mouse(code, false, false);
            } else if code == 0 {
                self.term_press = Some((view_id, (x, y)));
            }
            return;
        }
        if code != 0 {
            return;
        }
        let Some(pos) = render::ui::position_at(&mut self.editor, &self.font, view_id, rect, x, y) else { return };
        // Ctrl+click (Cmd+click on macOS) follows the link there, like `g x`.
        let link_click = if cfg!(target_os = "macos") { self.modifiers.meta_key() } else { self.modifiers.control_key() };
        if link_click {
            self.editor.views.get_mut(&view_id).expect("view").selection = Selection::point(pos);
            if let Some(command) = commands::find("open_link") {
                self.editor.run_command(command, commands::Ctx::default());
            }
            return;
        }
        let extend = self.modifiers.shift_key();
        let view = self.editor.views.get_mut(&view_id).expect("view");
        let anchor = if extend { view.selection.primary().anchor } else { pos };
        view.selection = Selection::single(Range::new(anchor, pos));
        self.drag = Some((view_id, anchor));
    }

    /// The pointer moved: extend a drag selection, report motion to a terminal
    /// application, or turn a drag on a plain terminal into a selection.
    /// Returns true when something to draw changed.
    fn mouse_moved(&mut self) -> bool {
        if self.drag.is_some() {
            self.mouse_drag();
            return true;
        }
        if self.editor.mode != Mode::Terminal || !matches!(self.editor.overlay, editor::Overlay::None) {
            return false;
        }
        let held = self.term_button;
        if self.editor.buffer().terminal.as_ref().is_some_and(|t| t.wants_motion(held.is_some())) {
            let (x, y) = self.mouse;
            let focused = self.editor.focused_view();
            if let Some((_, rect)) = self.editor.frame_rects.iter().copied().find(|(id, _)| *id == focused)
                && render::ui::terminal_cell_at(&self.font, rect, x, y) != self.term_cell
            {
                self.send_mouse(held.unwrap_or(3), false, true);
            }
            return false;
        }
        if let Some((view_id, (px, py))) = self.term_press.take() {
            let Some((_, rect)) = self.editor.frame_rects.iter().copied().find(|(id, _)| *id == view_id) else { return false };
            let buffer = self.editor.view().buffer;
            self.editor.refresh_terminal_text(buffer);
            self.editor.set_mode(Mode::Normal);
            let (x, y) = self.mouse;
            let anchor = render::ui::position_at(&mut self.editor, &self.font, view_id, rect, px, py);
            let pos = render::ui::position_at(&mut self.editor, &self.font, view_id, rect, x, y);
            if let (Some(anchor), Some(pos)) = (anchor, pos) {
                self.editor.views.get_mut(&view_id).expect("view").selection = Selection::single(Range::new(anchor, pos));
                self.drag = Some((view_id, anchor));
            }
            return true;
        }
        false
    }

    fn mouse_drag(&mut self) {
        let Some((view_id, anchor)) = self.drag else { return };
        let Some((_, rect)) = self.editor.frame_rects.iter().copied().find(|(id, _)| *id == view_id) else { return };
        let (x, y) = self.mouse;
        let x = x.clamp(rect.x, rect.right() - 1);
        let y = y.clamp(rect.y, rect.bottom() - 1);
        if let Some(pos) = render::ui::position_at(&mut self.editor, &self.font, view_id, rect, x, y) {
            let view = self.editor.views.get_mut(&view_id).expect("view");
            view.selection = Selection::single(Range::new(anchor, pos));
        }
    }

    fn scroll(&mut self, lines: i32) {
        let (x, y) = self.mouse;
        // Over a picker: the wheel moves the preview, or the list.
        if let (editor::Overlay::Picker(picker), Some((list_rect, preview_rect))) = (&mut self.editor.overlay, self.editor.picker_rects) {
            if preview_rect.contains(x, y) {
                if let Some(preview) = &mut self.editor.preview {
                    preview.scroll = (preview.scroll as i64 - lines as i64).max(0) as usize;
                }
            } else if list_rect.contains(x, y) {
                let count = picker.matches.len() as i64;
                picker.selected = (picker.selected as i64 - lines as i64).clamp(0, (count - 1).max(0)) as usize;
            }
            return;
        }
        let Some((view_id, _)) = self.frame_at(x, y) else { return };
        // The welcome page does not scroll.
        if self.editor.buffers[&self.editor.views[&view_id].buffer].welcome.is_some() {
            return;
        }
        // In terminal mode: an application that asked for the mouse gets wheel
        // events, a full-screen one without it gets arrow keys, a shell scrolls
        // its scrollback.
        if view_id == self.editor.focused_view() && self.editor.mode == Mode::Terminal {
            let (wants_mouse, alt_screen) = match &self.editor.buffer().terminal {
                Some(t) => (t.wants_mouse(), t.in_alt_screen()),
                None => (false, false),
            };
            if wants_mouse {
                let button = if lines > 0 { 64 } else { 65 };
                for _ in 0..lines.unsigned_abs() {
                    self.send_mouse(button, false, false);
                }
                return;
            }
            if alt_screen {
                let key = Key::code(if lines > 0 { KeyCode::Up } else { KeyCode::Down });
                for _ in 0..lines.unsigned_abs() {
                    self.editor.handle_key(key);
                }
                return;
            }
        }
        let view = self.editor.views.get_mut(&view_id).expect("view");
        let buffer = &self.editor.buffers[&view.buffer];
        let max = buffer.line_count().saturating_sub(1);
        let terminal_rows = buffer.terminal.as_ref().map(|t| t.total_rows().saturating_sub(view.rows)).unwrap_or(max);
        let max = if buffer.is_terminal() { terminal_rows } else { max };
        view.scroll_line = (view.scroll_line as i64 - lines as i64).clamp(0, max as i64) as usize;
        view.anim = None;
        view.scrolled_back = buffer.is_terminal() && view.scroll_line < max;
        view.cursor_follows = !buffer.is_terminal();
    }
}

impl ApplicationHandler for App {
    /// The animation timer ran out: draw the next frame.
    fn new_events(&mut self, _event_loop: &dyn ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            self.next_frame = None;
            self.request_redraw();
        }
    }

    /// Sleep until input arrives, or until the next animation frame is due.
    fn about_to_wait(&mut self, event_loop: &dyn ActiveEventLoop) {
        match self.next_frame {
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = WindowAttributes::default().with_title("quin").with_surface_size(LogicalSize::new(1100.0, 750.0));
        let window: Rc<dyn Window> = Rc::from(event_loop.create_window(attributes).expect("create window"));
        // Input methods and compositor-side dead keys deliver text through IME commits.
        if let Some(enable) = ImeEnableRequest::new(ImeCapabilities::new(), ImeRequestData::default()) {
            let _ = window.request_ime_update(ImeRequest::Enable(enable));
        }
        let context = softbuffer::Context::new(event_loop.owned_display_handle()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.scale = window.scale_factor();
        self.font = load_font(&mut self.editor, self.scale);
        self.window = Some(window);
        self.surface = Some(surface);
    }

    /// Background work woke us up: terminal output or search results.
    fn proxy_wake_up(&mut self, _event_loop: &dyn ActiveEventLoop) {
        let terminals = self.editor.process_terminals();
        let git = self.editor.poll_git(false);
        let signs = self.editor.signs.poll();
        // A running pull or push ticks the window so its spinner keeps turning.
        let spinning = self.editor.git.remote.is_some();
        if self.editor.poll_search() || terminals || git || signs || spinning {
            self.request_frame();
        }
    }

    fn window_event(&mut self, event_loop: &dyn ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                let modified = self.editor.buffers.values().any(|b| b.is_modified());
                if modified && !self.close_requested {
                    self.close_requested = true;
                    self.editor.set_error("unsaved changes: close again to discard them, or :write first");
                    self.request_redraw();
                } else {
                    event_loop.exit();
                }
            }
            WindowEvent::SurfaceResized(_) => self.request_redraw(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = scale_factor;
                self.font = load_font(&mut self.editor, scale_factor);
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(key) = translate_key(&event, self.modifiers) {
                    self.close_requested = false;
                    self.editor.handle_key(key);
                    self.after_input(event_loop);
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                for c in text.chars() {
                    self.editor.handle_key(Key::char(c));
                }
                self.close_requested = false;
                self.after_input(event_loop);
            }
            WindowEvent::PointerMoved { position, .. } => {
                self.mouse = (position.x as i32, position.y as i32);
                if self.mouse_moved() {
                    self.request_redraw();
                }
            }
            WindowEvent::PointerButton { state, button, .. } => {
                if let Some(button) = button.mouse_button() {
                    self.mouse_button(button, state == ElementState::Pressed);
                    self.after_input(event_loop);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 3.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / self.font.cell_h,
                    _ => 0.0,
                };
                let lines = wheel_lines(&mut self.scroll_rest, amount);
                if lines != 0 {
                    self.scroll(lines);
                    self.request_redraw();
                }
            }
            WindowEvent::DragEntered { id, position } => {
                let kind = event_loop.data_transfer(id).ok().and_then(|transfer| DROP_KINDS.into_iter().find(|k| transfer.has_type(k)));
                let actions: &[DndAction] = if kind.is_some() { &[DndAction::Copy] } else { &[] };
                let _ = event_loop.set_valid_dnd_actions(id, actions);
                self.dnd = kind;
                if let (Some(_), Some(position)) = (kind, position) {
                    self.drag_to(position.x, position.y);
                }
            }
            WindowEvent::DragPosition { position, .. } if self.dnd.is_some() => self.drag_to(position.x, position.y),
            WindowEvent::DragDropped { id, .. } => {
                if let (Some(kind), Some((view, line))) = (self.dnd, self.dnd_at)
                    && let Ok(serial) = event_loop.fetch_data_transfer(id, &kind)
                {
                    self.dropped = Some((serial, kind, view, line));
                }
                self.drag_ended();
            }
            WindowEvent::DragLeft { .. } => self.drag_ended(),
            WindowEvent::DataTransferReceived { serial, value, .. } => {
                let Some((_, kind, view, line)) = self.dropped.take_if(|(waiting, ..)| *waiting == serial) else { return };
                match kind {
                    TypeHint::UriList => match value.try_as_file_paths() {
                        Ok(files) => media::drop_files(&mut self.editor, view, line, files),
                        Err(_) => self.editor.set_error("only local files can be dropped"),
                    },
                    _ => match value.try_as_bytes() {
                        Ok(bytes) => media::drop_image(&mut self.editor, view, line, &bytes),
                        Err(err) => self.editor.set_error(format!("drop: {err}")),
                    },
                }
                self.after_input(event_loop);
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }
}

/// Whole lines to scroll for a wheel or touchpad event of `amount` lines.
/// Touchpads send many events of a few pixels each: the fractions add up in
/// `rest` until they make a line, instead of each rounding down to nothing.
/// Turning the other way drops what was left over.
pub fn wheel_lines(rest: &mut f32, amount: f32) -> i32 {
    if *rest != 0.0 && rest.signum() != amount.signum() {
        *rest = 0.0;
    }
    *rest += amount;
    let lines = rest.trunc();
    *rest -= lines;
    lines as i32
}

/// Convert a winit key event to the editor's `Key`.
fn translate_key(event: &KeyEvent, modifiers: ModifiersState) -> Option<Key> {
    let ctrl = modifiers.control_key();
    let alt = modifiers.alt_key();
    let meta = modifiers.meta_key();
    let mut shift = modifiers.shift_key();
    // What the key actually types, with dead keys composed: on an international
    // layout `'` then `a` is `á` and `'` then space is `'`. The logical key
    // would say "a" or "Space" there, so the typed text wins for plain keys.
    if !ctrl
        && !alt
        && !meta
        && let Some(text) = &event.text
        && let mut chars = text.chars()
        && let (Some(c), None) = (chars.next(), chars.next())
        && !c.is_control()
    {
        return Some(Key::char(c));
    }
    let code = match &event.logical_key {
        WinitKey::Named(named) => match named {
            NamedKey::Escape => KeyCode::Escape,
            NamedKey::Enter => KeyCode::Enter,
            NamedKey::Tab => KeyCode::Tab,
            NamedKey::Backspace => KeyCode::Backspace,
            NamedKey::Delete => KeyCode::Delete,
            NamedKey::Insert => KeyCode::Insert,
            NamedKey::Home => KeyCode::Home,
            NamedKey::End => KeyCode::End,
            NamedKey::PageUp => KeyCode::PageUp,
            NamedKey::PageDown => KeyCode::PageDown,
            NamedKey::ArrowUp => KeyCode::Up,
            NamedKey::ArrowDown => KeyCode::Down,
            NamedKey::ArrowLeft => KeyCode::Left,
            NamedKey::ArrowRight => KeyCode::Right,
            NamedKey::F1 => KeyCode::F(1),
            NamedKey::F2 => KeyCode::F(2),
            NamedKey::F3 => KeyCode::F(3),
            NamedKey::F4 => KeyCode::F(4),
            NamedKey::F5 => KeyCode::F(5),
            NamedKey::F6 => KeyCode::F(6),
            NamedKey::F7 => KeyCode::F(7),
            NamedKey::F8 => KeyCode::F(8),
            NamedKey::F9 => KeyCode::F(9),
            NamedKey::F10 => KeyCode::F(10),
            NamedKey::F11 => KeyCode::F(11),
            NamedKey::F12 => KeyCode::F(12),
            _ => return None,
        },
        WinitKey::Character(text) => {
            let mut chars = text.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            // Characters already carry the shift (`A` vs `a`); keep the flag only
            // for chords like C-S-v where the keymap wants the lower-case letter.
            if (ctrl || alt || meta) && shift && c.is_alphabetic() {
                KeyCode::Char(c.to_lowercase().next().unwrap_or(c))
            } else {
                shift = false;
                KeyCode::Char(c)
            }
        }
        _ => return None,
    };
    if let KeyCode::Char(' ') = code {
        shift = false;
    }
    Some(Key { code, ctrl, alt, shift, meta })
}
