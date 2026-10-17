//! Images and files coming from outside the editor: dragged onto the window
//! or pasted from the clipboard.
//!
//! - On a buffer whose language has a `media` template (Markdown, HTML), a
//!   PNG or JPEG image is saved next to the buffer's file and the template
//!   (`<img src="{path}" />`) goes into the text: below the line it was
//!   dropped on, or where it is pasted.
//! - On the file tree, anything dropped is copied into the directory under
//!   the pointer. The tree takes nothing from the clipboard but text.
//! - Anywhere else, dropped files are opened.

use std::path::{Path, PathBuf};

use crate::clipboard::Media;
use crate::editor::{Editor, Mode};
use crate::frame::ViewId;
use crate::history::Transaction;
use crate::selection::{Range, Selection};

/// What a drag over the window would do at the pointer, to show it.
#[derive(Debug, Clone, PartialEq)]
pub struct DropHover {
    pub view: ViewId,
    /// The line to highlight: the line an image goes below, or the tree's
    /// target directory (none when it is the tree's root).
    pub line: Option<usize>,
    pub message: String,
}

/// Images are kept when their data starts like one of these formats.
pub fn image_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else {
        None
    }
}

/// Is this file a PNG or JPEG image (by its name and its first bytes)?
fn is_image_file(path: &Path) -> bool {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg") {
        return false;
    }
    let mut head = [0u8; 8];
    let read = std::fs::File::open(path).and_then(|mut file| std::io::Read::read(&mut file, &mut head));
    read.is_ok_and(|n| image_extension(&head[..n]).is_some())
}

/// The `media` template of the buffer shown in `view`, when it can take
/// images: a language with a template, a file on disk, not read-only.
fn media_template(ed: &Editor, view: ViewId) -> Option<(String, PathBuf)> {
    let buffer = &ed.buffers[&ed.views.get(&view)?.buffer];
    if buffer.tree || buffer.readonly || buffer.is_terminal() {
        return None;
    }
    let template = ed.registry.get(buffer.language?).media.clone()?;
    let dir = buffer.path.as_ref()?.parent()?.to_path_buf();
    Some((template, dir))
}

/// What dropping on `line` of `view` would do, for the highlight while dragging.
pub fn hover(ed: &Editor, view: ViewId, line: usize) -> Option<DropHover> {
    let buffer = &ed.buffers[&ed.views.get(&view)?.buffer];
    if buffer.tree {
        let (dir, line) = crate::tree::drop_dir(ed, line)?;
        let message = format!("drop to copy into {}", display_dir(ed, &dir));
        return Some(DropHover { view, line, message });
    }
    if media_template(ed, view).is_some() {
        return Some(DropHover { view, line: Some(line), message: "drop to add images below this line".to_string() });
    }
    Some(DropHover { view, line: None, message: "drop to open".to_string() })
}

/// Files dropped on `line` of `view`.
pub fn drop_files(ed: &mut Editor, view: ViewId, line: usize, files: Vec<PathBuf>) {
    if files.is_empty() || !focus(ed, view) {
        return;
    }
    if ed.buffer().tree {
        let Some((dir, _)) = crate::tree::drop_dir(ed, line) else {
            ed.set_error("write the tree first: it has unsaved edits");
            return;
        };
        let mut copied = 0;
        for file in &files {
            match copy_into(file, &dir) {
                Ok(_) => copied += 1,
                Err(err) => ed.set_error(format!("{}: {err}", file.display())),
            }
        }
        crate::tree::reveal(ed, &dir);
        if copied == files.len() {
            ed.set_message(format!("copied {copied} into {}", display_dir(ed, &dir)));
        }
        return;
    }
    let (images, others): (Vec<PathBuf>, Vec<PathBuf>) = match media_template(ed, view) {
        Some(_) => files.into_iter().partition(|f| is_image_file(f)),
        None => (Vec::new(), files),
    };
    if !images.is_empty() {
        let Some((template, dir)) = media_template(ed, view) else { return };
        let mut tags = Vec::new();
        for image in &images {
            match place_file(image, &dir) {
                Ok(relative) => tags.push(template.replace("{path}", &relative)),
                Err(err) => ed.set_error(format!("{}: {err}", image.display())),
            }
        }
        insert_lines_below(ed, line, &tags);
    }
    for file in others.iter().filter(|f| f.is_file()) {
        crate::overlays::open_path(ed, file, None, crate::picker::OpenMode::Here);
    }
}

/// Image data dropped on `line` of `view` (dragged out of a browser).
pub fn drop_image(ed: &mut Editor, view: ViewId, line: usize, bytes: &[u8]) {
    if !focus(ed, view) {
        return;
    }
    let Some(extension) = image_extension(bytes) else {
        ed.set_error("only PNG and JPEG images can be dropped");
        return;
    };
    let dir = if ed.buffer().tree {
        match crate::tree::drop_dir(ed, line) {
            Some((dir, _)) => dir,
            None => return ed.set_error("write the tree first: it has unsaved edits"),
        }
    } else {
        match media_template(ed, view) {
            Some((_, dir)) => dir,
            None => return ed.set_error("this buffer does not take images"),
        }
    };
    let path = unique_path(&dir, &new_image_name(extension));
    if let Err(err) = std::fs::write(&path, bytes) {
        return ed.set_error(format!("{}: {err}", path.display()));
    }
    if ed.buffer().tree {
        crate::tree::reveal(ed, &dir);
        ed.set_message(format!("saved {}", path.display()));
    } else if let Some((template, _)) = media_template(ed, view) {
        let tag = template.replace("{path}", &file_name(&path));
        insert_lines_below(ed, line, &[tag]);
    }
}

/// `p` and friends in a buffer that takes images: an image (or an image
/// file) on the clipboard is saved next to the buffer's file and the paste
/// inserts its tag instead. `None` when there is nothing of the kind: the
/// paste goes on with the text.
pub fn paste_text(ed: &mut Editor) -> Option<String> {
    let (template, dir) = media_template(ed, ed.focused_view())?;
    let relative = match ed.clipboard.media()? {
        Media::Bytes(bytes) => {
            let extension = image_extension(&bytes)?;
            let path = unique_path(&dir, &new_image_name(extension));
            if let Err(err) = std::fs::write(&path, &bytes) {
                ed.set_error(format!("{}: {err}", path.display()));
                return None;
            }
            file_name(&path)
        }
        Media::File(file) if is_image_file(&file) => match place_file(&file, &dir) {
            Ok(relative) => relative,
            Err(err) => {
                ed.set_error(format!("{}: {err}", file.display()));
                return None;
            }
        },
        Media::File(_) => return None,
    };
    Some(template.replace("{path}", &relative))
}

/// Drops act on the frame under the pointer, like a click. False when it
/// cannot take the focus (a floating frame is open).
fn focus(ed: &mut Editor, view: ViewId) -> bool {
    if ed.views.contains_key(&view) {
        ed.focus_view(view);
    }
    ed.focused_view() == view
}

/// Put each text on its own line below `line`, and select the last one.
fn insert_lines_below(ed: &mut Editor, line: usize, texts: &[String]) {
    if texts.is_empty() {
        return;
    }
    if ed.mode == Mode::Select {
        ed.set_mode(Mode::Normal);
    }
    let buffer = ed.buffer();
    let line = line.min(buffer.line_count().saturating_sub(1));
    let at = buffer.line_end(line);
    let text: String = texts.iter().map(|t| format!("\n{t}")).collect();
    let end = at + text.chars().count();
    let last_start = end - texts.last().map(|t| t.chars().count()).unwrap_or(0);
    let mut tx = Transaction::new();
    tx.insert(at, text);
    ed.view_mut().save_jump();
    ed.apply(tx, Selection::single(Range::new(last_start, end.saturating_sub(1))));
}

/// Make `file` available in `dir` and give its path relative to `dir`: a
/// file already inside `dir` stays where it is, others are copied in.
fn place_file(file: &Path, dir: &Path) -> std::io::Result<String> {
    let file = crate::buffer::absolute(file);
    if let Ok(inside) = file.strip_prefix(dir) {
        return Ok(inside.to_string_lossy().replace('\\', "/"));
    }
    let copy = copy_into(&file, dir)?;
    Ok(file_name(&copy))
}

/// Copy a file or a whole directory into `dir`, under a free name. Returns
/// the new path.
fn copy_into(source: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    let name = source.file_name().ok_or_else(|| std::io::Error::other("no file name"))?;
    let target = unique_path(dir, &name.to_string_lossy());
    if source.is_dir() {
        if dir.starts_with(source) {
            return Err(std::io::Error::other("cannot copy a directory into itself"));
        }
        crate::files::copy_dir(source, &target)?;
    } else {
        std::fs::copy(source, &target)?;
    }
    Ok(target)
}

/// `dir/name`, or `dir/name-2.ext`, `dir/name-3.ext`... when it is taken.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    (2..).map(|n| dir.join(format!("{stem}-{n}{extension}"))).find(|p| !p.exists()).expect("a free name")
}

/// `image-20261016-213012.png`: the time (UTC) the image came in.
fn new_image_name(extension: &str) -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("image-{year:04}{month:02}{day:02}-{:02}{:02}{:02}.{extension}", rest / 3600, rest / 60 % 60, rest % 60)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A directory as the tree shows it: relative to the working directory.
fn display_dir(ed: &Editor, dir: &Path) -> String {
    match dir.strip_prefix(&ed.cwd) {
        Ok(inside) if inside.as_os_str().is_empty() => "./".to_string(),
        Ok(inside) => format!("{}/", inside.to_string_lossy()),
        Err(_) => dir.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_formats() {
        assert_eq!(image_extension(b"\x89PNG\r\n\x1a\nrest"), Some("png"));
        assert_eq!(image_extension(&[0xff, 0xd8, 0xff, 0xe0]), Some("jpg"));
        assert_eq!(image_extension(b"GIF89a"), None);
        let name = new_image_name("png");
        assert!(name.starts_with("image-20") && name.ends_with(".png") && name.len() == "image-20261016-213012.png".len(), "{name}");
        let dir = std::env::temp_dir().join(format!("quin-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), "x").unwrap();
        std::fs::write(dir.join("a-2.png"), "x").unwrap();
        assert_eq!(unique_path(&dir, "a.png"), dir.join("a-3.png"));
        assert_eq!(unique_path(&dir, "b.png"), dir.join("b.png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
