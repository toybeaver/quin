//! The welcome buffer shown when quin starts without a file: the logo, a
//! short greeting and the most recent files and directories: directories
//! moved into with `:cd` (or given on the command line) and files opened by
//! name with `:open` (or on the command line), not files picked from inside
//! a directory. `ret` opens the entry under the cursor; a directory becomes
//! the working directory and the page gives way to a scratch buffer, as it
//! does when typing starts.

use std::path::{Path, PathBuf};

use crate::buffer::BufferId;
use crate::editor::Editor;

/// The logo as raw RGBA pixels, pre-rendered from `assets/icon/icon.png`.
pub const LOGO_RGBA: &[u8] = include_bytes!("../assets/icon/icon-96.rgba");
pub const LOGO_SIZE: usize = 96;

/// How many recent entries the welcome buffer lists.
const SHOWN: usize = 5;
/// How many are remembered on disk.
const KEPT: usize = 50;

/// The file keeping recent paths, most recent first, one per line. Tests
/// use a scratch file so they never touch the real list.
fn recent_file() -> Option<PathBuf> {
    if cfg!(test) {
        return Some(std::env::temp_dir().join(format!("quin-test-recent-{}", std::process::id())));
    }
    crate::config::config_dir().map(|d| d.join("recent"))
}

pub fn recent() -> Vec<PathBuf> {
    let Some(path) = recent_file() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect()
}

/// Put `path` at the top of the recent list.
pub fn remember(path: &Path) {
    let Some(file) = recent_file() else { return };
    let path = crate::buffer::absolute(path);
    let mut list = recent();
    list.retain(|p| *p != path);
    list.insert(0, path);
    list.truncate(KEPT);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    let _ = std::fs::write(file, text);
}

/// `~/x` for paths under the home directory.
fn pretty(path: &Path) -> String {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    let shown = match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    };
    if path.is_dir() { format!("{}/", shown.trim_end_matches('/')) } else { shown }
}

/// The text of the welcome buffer and the entries behind its lines.
pub fn text() -> (String, Vec<Option<PathBuf>>) {
    let mut text = String::from("quin\n\nA small, fast, keyboard-driven code editor.\n\n");
    let mut lines: Vec<Option<PathBuf>> = vec![None; 4];
    let entries: Vec<PathBuf> = recent().into_iter().filter(|p| p.exists()).take(SHOWN).collect();
    if entries.is_empty() {
        text.push_str("Recent files and directories will show up here.\n");
        lines.push(None);
    } else {
        text.push_str("Recent\n");
        lines.push(None);
        for entry in entries {
            text.push_str(&format!("  {}\n", pretty(&entry)));
            lines.push(Some(entry));
        }
    }
    text.push_str("\n· ret      open the entry under the cursor (a directory becomes the working directory)\n");
    text.push_str("· i / a    start writing in a scratch buffer\n");
    text.push_str("· space e  open a file        space l  git        :help  every key\n");
    lines.extend(std::iter::repeat_n(None, 4));
    (text, lines)
}

/// Show the welcome buffer in the focused frame, cursor on the most recent entry.
pub fn open(ed: &mut Editor) {
    let id = ed.new_scratch_buffer();
    let (text, lines) = text();
    let buffer = ed.buffers.get_mut(&id).expect("new buffer");
    buffer.set_text(&text);
    buffer.welcome = Some(lines.clone());
    buffer.readonly = true;
    buffer.set_language(ed.registry.find("welcome"));
    ed.show_buffer(id);
    if let Some(line) = lines.iter().position(Option::is_some) {
        crate::commands::goto_line_number(ed, line, false);
    }
}

/// `ret`: open the file, or move into the directory, under the cursor.
pub fn activate(ed: &mut Editor) {
    let line = ed.buffer().line_of(ed.view().cursor());
    let Some(path) = ed.buffer().welcome.as_ref().and_then(|lines| lines.get(line).cloned().flatten()) else { return };
    if path.is_dir() {
        if let Err(err) = crate::typed::cd(ed, &path) {
            ed.set_error(err);
        }
        return;
    }
    let welcome = ed.view().buffer;
    crate::overlays::open_path(ed, &path, None, crate::picker::OpenMode::Here);
    if ed.view().buffer != welcome {
        let _ = ed.close_buffer(welcome, true);
    }
}

/// Typing in the welcome buffer: replace it with an empty scratch buffer.
pub fn leave_for_scratch(ed: &mut Editor) -> BufferId {
    let welcome = ed.view().buffer;
    let scratch = ed.new_scratch_buffer();
    ed.show_buffer(scratch);
    let _ = ed.close_buffer(welcome, true);
    scratch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_is_a_square_of_rgba_pixels() {
        assert_eq!(LOGO_RGBA.len(), LOGO_SIZE * LOGO_SIZE * 4);
    }

    #[test]
    fn pretty_paths_use_tilde_and_a_slash_for_directories() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(pretty(Path::new(&format!("{home}/x.rs"))), "~/x.rs");
        assert_eq!(pretty(Path::new("/tmp")), "/tmp/");
    }
}
