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
mod install;
mod keymap;
mod movement;
mod overlays;
mod picker;
mod render;
mod search;
mod selection;
mod syntax;
mod terminal;
#[cfg(test)]
mod tests;
mod theme;
mod tree;
mod typed;

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

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
            }
        } else {
            match editor.open_file(path) {
                Ok(id) => opened.push(id),
                Err(err) => eprintln!("quin: {err}"),
            }
        }
    }
    if let Some(first) = opened.first() {
        let scratch = editor.view().buffer;
        editor.show_buffer(*first);
        let _ = editor.close_buffer(scratch, true);
    }
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
    let read = |path: &PathBuf| std::fs::read(path).ok();
    let family_of = |path: &Path| path.file_stem().and_then(|s| s.to_str()).unwrap_or("").trim_end_matches("-Regular").to_string();
    let found = match (&config.font, &config.font_family) {
        (Some(path), _) => Some((path.clone(), config.bold_font.clone())),
        (None, Some(family)) => {
            let found = render::font::find_font_family(family);
            if found.is_none() {
                eprintln!("quin: font family `{family}` not found, using the built-in Iosevka");
            }
            found
        }
        (None, None) => None,
    };
    let (mut font, family) = match found {
        Some((regular, bold)) => match read(&regular).map(|bytes| FontSet::load(bytes, bold.as_ref().and_then(read), size)) {
            Some(Ok(font)) => (font, family_of(&regular)),
            Some(Err(err)) => {
                eprintln!("quin: cannot load font {}: {err}", regular.display());
                (FontSet::builtin(size), "Iosevka".to_string())
            }
            None => (FontSet::builtin(size), "Iosevka".to_string()),
        },
        None => (FontSet::builtin(size), "Iosevka".to_string()),
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
    wait_for_background(editor);
    editor.views.values_mut().for_each(|v| {
        v.anim = None;
        v.cursor_anim = None;
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
        if editor.live_search.as_ref().is_none_or(|s| s.done) && !editor.git.in_flight() && editor.git.remote.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

fn run_window(editor: Editor) {
    let event_loop = EventLoop::<()>::with_user_event().build().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = App::new(editor, proxy);
    if let Err(err) = event_loop.run_app(&mut app) {
        eprintln!("quin: {err}");
    }
}

struct App {
    editor: Editor,
    font: FontSet,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
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
}

impl App {
    fn new(mut editor: Editor, proxy: EventLoopProxy<()>) -> App {
        let proxy = Mutex::new(proxy);
        editor.wake = Some(Arc::new(move || {
            if let Ok(proxy) = proxy.lock() {
                let _ = proxy.send_event(());
            }
        }));
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
        }
    }

    fn redraw(&mut self) {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else { return };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        if surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else { return };
        {
            let painter = Painter::new(&mut buffer, size.width as usize, size.height as usize);
            let mut ui = Ui { painter, font: &mut self.font };
            render::ui::draw(&mut self.editor, &mut ui);
        }
        let _ = buffer.present();
        let title = format!("{} - quin", self.editor.buffer().display_name());
        window.set_title(&title);
        if self.editor.is_animating() {
            window.request_redraw();
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Things to check after any input: quitting and redrawing.
    fn after_input(&mut self, event_loop: &ActiveEventLoop) {
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
    }
}

impl ApplicationHandler<()> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes().with_title("quin").with_inner_size(LogicalSize::new(1100.0, 750.0));
        let window = Rc::new(event_loop.create_window(attributes).expect("create window"));
        // Input methods and compositor-side dead keys deliver text through IME commits.
        window.set_ime_allowed(true);
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.scale = window.scale_factor();
        self.font = load_font(&mut self.editor, self.scale);
        self.window = Some(window);
        self.surface = Some(surface);
    }

    /// Background work woke us up: terminal output or search results.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        let terminals = self.editor.process_terminals();
        let git = self.editor.poll_git(false);
        // A running pull or push ticks the window so its spinner keeps turning.
        let spinning = self.editor.git.remote.is_some();
        if self.editor.poll_search() || terminals || git || spinning {
            self.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
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
            WindowEvent::Resized(_) => self.request_redraw(),
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
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = (position.x as i32, position.y as i32);
                if self.mouse_moved() {
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.mouse_button(button, state == ElementState::Pressed);
                self.after_input(event_loop);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 3.0).round() as i32,
                    MouseScrollDelta::PixelDelta(p) => (p.y as f32 / self.font.cell_h).round() as i32,
                };
                if lines != 0 {
                    self.scroll(lines);
                    self.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }
}

/// Convert a winit key event to the editor's `Key`.
fn translate_key(event: &KeyEvent, modifiers: ModifiersState) -> Option<Key> {
    let ctrl = modifiers.control_key();
    let alt = modifiers.alt_key();
    let meta = modifiers.super_key();
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
            NamedKey::Space => KeyCode::Char(' '),
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
