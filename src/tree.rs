//! The file tree: a sidebar buffer listing the working directory, in the
//! spirit of oil.nvim. Directories end with `/` and expand on `ret`; files
//! open on `ret`. The text is editable: rename, create, move or delete
//! entries by editing lines, then `:write` applies the changes to disk.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::buffer::BufferId;
use crate::editor::Editor;
use crate::frame::{Dir, ViewId};

/// Width of the sidebar as a fraction of the window.
const SIDEBAR_FRACTION: f32 = 0.25;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub is_dir: bool,
    pub depth: usize,
}

pub struct Tree {
    pub buffer: BufferId,
    pub root: PathBuf,
    expanded: HashSet<PathBuf>,
    /// What the buffer showed when it was last rendered: the baseline that
    /// edits are compared against.
    pub entries: Vec<Entry>,
    /// The frame files open in (the one focused before the tree).
    pub return_view: Option<ViewId>,
    /// Deletions shown on the last `:write`, applied by the next one.
    pending_deletes: Vec<PathBuf>,
    /// `-` moved the root above the working directory: keep it there until
    /// the directory changes or the tree is reopened.
    browsing: bool,
    /// `.` toggles dot-files and dot-directories; they start hidden.
    show_hidden: bool,
}

// ---------------------------------------------------------------------------
// Showing the tree
// ---------------------------------------------------------------------------

/// `A-e` / `:tree`: show the sidebar, or hide it when it is visible.
pub fn toggle(ed: &mut Editor) {
    if let Some(view) = tree_view(ed) {
        ed.close_view(view);
        return;
    }
    let previous = ed.focused_view();
    let buffer = match &ed.tree {
        Some(tree) if ed.buffers.contains_key(&tree.buffer) => tree.buffer,
        _ => {
            let id = ed.new_scratch_buffer();
            let buffer = ed.buffers.get_mut(&id).expect("new buffer");
            buffer.tree = true;
            buffer.set_language(ed.registry.find("tree"));
            ed.tree = Some(Tree {
                buffer: id,
                root: ed.cwd.clone(),
                expanded: HashSet::new(),
                entries: Vec::new(),
                return_view: None,
                pending_deletes: Vec::new(),
                browsing: false,
                show_hidden: false,
            });
            render(ed);
            id
        }
    };
    ed.split_root(Dir::Horizontal, buffer, SIDEBAR_FRACTION);
    if let Some(tree) = &mut ed.tree {
        tree.return_view = Some(previous);
        // Reopening always starts from the working directory.
        tree.browsing = false;
    }
    render(ed);
}

/// `-`: show the parent of the current root (the working directory stays),
/// with the directory we came from unfolded.
pub fn go_to_parent(ed: &mut Editor) {
    let Some(tree) = &mut ed.tree else { return };
    let Some(parent) = tree.root.parent().map(Path::to_path_buf) else { return };
    let from = std::mem::replace(&mut tree.root, parent);
    tree.expanded.insert(from);
    tree.browsing = true;
    render(ed);
}

/// `.`: show or hide entries whose name starts with a dot.
pub fn toggle_hidden(ed: &mut Editor) {
    let Some(tree) = &mut ed.tree else { return };
    tree.show_hidden = !tree.show_hidden;
    let shown = tree.show_hidden;
    render(ed);
    ed.set_message(if shown { "hidden files shown" } else { "hidden files hidden" });
}

/// Spaces of indentation per directory level.
const INDENT: usize = 4;

/// The frame of the current tab showing the tree, if any.
fn tree_view(ed: &Editor) -> Option<ViewId> {
    let tree = ed.tree.as_ref()?;
    ed.root().leaves().into_iter().find(|v| ed.views[v].buffer == tree.buffer)
}

/// The working directory changed: the tree shows it again.
pub fn follow_cwd(ed: &mut Editor) {
    if let Some(tree) = &mut ed.tree {
        tree.browsing = false;
    }
    render(ed);
}

/// Rebuild the text from the directory tree and the set of expanded directories.
pub fn render(ed: &mut Editor) {
    let Some(tree) = &mut ed.tree else { return };
    if tree.root != ed.cwd && !tree.browsing {
        tree.root = ed.cwd.clone();
        tree.expanded.clear();
    }
    let mut entries = Vec::new();
    collect(&tree.root, 0, &tree.expanded, tree.show_hidden, &mut entries);
    let git = ed.git.state.as_ref();
    let mut text = String::new();
    for entry in &entries {
        text.push_str(&" ".repeat(entry.depth * INDENT));
        // Directories with a change somewhere inside carry a `* ` marker.
        if entry.is_dir && git.is_some_and(|g| g.dir_changed(&entry.path)) {
            text.push_str("* ");
        }
        text.push_str(&file_name(&entry.path));
        if entry.is_dir {
            text.push('/');
        }
        text.push('\n');
    }
    tree.entries = entries;
    let buffer_id = tree.buffer;
    if let Some(buffer) = ed.buffers.get_mut(&buffer_id) {
        buffer.set_text(&text);
        buffer.history = crate::history::History::new();
        let len = buffer.len_chars();
        for view in ed.views.values_mut() {
            if view.buffer == buffer_id {
                view.selection = view.selection.clamp(len);
            }
        }
    }
}

fn collect(dir: &Path, depth: usize, expanded: &HashSet<PathBuf>, show_hidden: bool, out: &mut Vec<Entry>) {
    for (name, is_dir) in crate::files::list_dir(dir) {
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let path = dir.join(name.trim_end_matches('/'));
        out.push(Entry { path: path.clone(), is_dir, depth });
        if is_dir && expanded.contains(&path) {
            collect(&path, depth + 1, expanded, show_hidden, out);
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Drag and drop
// ---------------------------------------------------------------------------

/// Where files dropped on `line` of the tree go: the directory on that line,
/// the directory holding the file on that line, or the root below the last
/// line. Also gives the line showing that directory (none for the root), to
/// highlight it while dragging. Unsaved edits make the lines unreliable:
/// then nothing can be dropped.
pub fn drop_dir(ed: &Editor, line: usize) -> Option<(PathBuf, Option<usize>)> {
    let tree = ed.tree.as_ref()?;
    if ed.buffers.get(&tree.buffer)?.is_modified() {
        return None;
    }
    let Some(entry) = tree.entries.get(line) else { return Some((tree.root.clone(), None)) };
    if entry.is_dir {
        return Some((entry.path.clone(), Some(line)));
    }
    // A file: its directory is the closest line above that is one level up.
    let parent = (0..line).rev().find(|&i| tree.entries[i].is_dir && tree.entries[i].depth + 1 == entry.depth);
    match parent {
        Some(i) => Some((tree.entries[i].path.clone(), Some(i))),
        None => Some((tree.root.clone(), None)),
    }
}

/// Unfold `dir` (and the directories above it) and show the tree again, so
/// that files just put there are visible.
pub fn reveal(ed: &mut Editor, dir: &Path) {
    let Some(tree) = &mut ed.tree else { return };
    let mut current = Some(dir);
    while let Some(path) = current {
        if !path.starts_with(&tree.root) || path == tree.root {
            break;
        }
        tree.expanded.insert(path.to_path_buf());
        current = path.parent();
    }
    render(ed);
}

// ---------------------------------------------------------------------------
// ret: open a file or fold a directory
// ---------------------------------------------------------------------------

pub fn activate(ed: &mut Editor) {
    let Some(entry) = entry_under_cursor(ed) else { return };
    if entry.is_dir {
        toggle_directory(ed);
        return;
    }
    open_in_return_view(ed, &entry.path);
}

/// `S-ret`: make the directory under the cursor (or the directory of the
/// file under it) the working directory; the tree then shows it.
pub fn cd_under_cursor(ed: &mut Editor) {
    let Some(entry) = entry_under_cursor(ed) else { return };
    let target = if entry.is_dir { entry.path.clone() } else { entry.path.parent().map(Path::to_path_buf).unwrap_or(entry.path.clone()) };
    if let Err(err) = crate::typed::cd(ed, &target) {
        ed.set_error(err);
    }
}

/// `tab`: fold or unfold the directory under the cursor (nothing on a file).
pub fn toggle_directory(ed: &mut Editor) {
    let Some(entry) = entry_under_cursor(ed) else { return };
    if !entry.is_dir {
        return;
    }
    let tree = ed.tree.as_mut().expect("tree");
    if !tree.expanded.remove(&entry.path) {
        tree.expanded.insert(entry.path);
    }
    render(ed);
}

/// The entry on the cursor line, unless the tree has unsaved edits (then the
/// lines no longer match the entries).
fn entry_under_cursor(ed: &mut Editor) -> Option<Entry> {
    let tree = ed.tree.as_ref()?;
    if ed.buffer().is_modified() {
        ed.set_error("the tree has unsaved edits: :write them or undo first");
        return None;
    }
    let line = ed.buffer().line_of(ed.view().cursor());
    tree.entries.get(line).cloned()
}

/// Open a file in the frame the tree was opened from (or the first frame that
/// is not the tree), keeping the sidebar.
fn open_in_return_view(ed: &mut Editor, path: &Path) {
    let tree_buffer = ed.tree.as_ref().map(|t| t.buffer);
    let wanted = ed.tree.as_ref().and_then(|t| t.return_view);
    let leaves = ed.root().leaves();
    let usable = |v: &ViewId| ed.views.get(v).is_some_and(|view| Some(view.buffer) != tree_buffer) && leaves.contains(v);
    let target = match wanted.filter(usable).or_else(|| leaves.iter().copied().find(usable)) {
        Some(view) => view,
        None => {
            // Only the tree is open: make room on the left.
            let tree_view = ed.focused_view();
            let view = ed.split(Dir::Horizontal, None);
            ed.tab_mut().root.replace(view, tree_view);
            ed.tab_mut().root.replace(tree_view, view);
            view
        }
    };
    ed.focus_view(target);
    crate::overlays::open_path(ed, path, None, crate::picker::OpenMode::Here);
}

// ---------------------------------------------------------------------------
// :write — apply the edited text to the file system
// ---------------------------------------------------------------------------

/// What one `:write` of the tree does.
#[derive(Debug, Default, PartialEq, Eq)]
struct Changes {
    creates: Vec<Entry>,
    /// (from, to): renames and moves.
    moves: Vec<(PathBuf, PathBuf)>,
    deletes: Vec<PathBuf>,
}

/// Parse the buffer text back into entries. Errors describe the offending line.
fn parse(root: &Path, text: &str) -> Result<Vec<Entry>, String> {
    let mut entries: Vec<Entry> = Vec::new();
    // Directory of each depth, as a stack.
    let mut parents: Vec<PathBuf> = vec![root.to_path_buf()];
    for (i, raw) in text.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        // A tab counts as one level, like the four spaces `tab` inserts.
        let indent: usize = raw.chars().take_while(|c| c.is_whitespace()).map(|c| if c == '\t' { INDENT } else { 1 }).sum();
        let depth = indent / INDENT;
        let name = raw.trim();
        let name = name.strip_prefix("* ").unwrap_or(name); // the git marker is not part of the name
        let is_dir = name.ends_with('/');
        let name = name.trim_end_matches('/');
        if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
            return Err(format!("line {}: `{}` is not a valid name", i + 1, raw.trim()));
        }
        if depth + 1 > parents.len() {
            return Err(format!("line {}: indented more than one level below its parent", i + 1));
        }
        parents.truncate(depth + 1);
        if depth > 0 && entries.last().is_none_or(|e| e.depth < depth && !e.is_dir) {
            return Err(format!("line {}: a file cannot contain entries", i + 1));
        }
        let path = parents[depth].join(name);
        entries.push(Entry { path: path.clone(), is_dir, depth });
        if is_dir {
            parents.push(path);
        }
    }
    Ok(entries)
}

/// Work out creates, moves/renames and deletes between what was shown and
/// what the text says now. Returns an error for an ambiguous edit.
fn diff(old: &[Entry], new: &[Entry]) -> Result<Changes, String> {
    let old_paths: HashSet<&PathBuf> = old.iter().map(|e| &e.path).collect();
    let new_paths: HashSet<&PathBuf> = new.iter().map(|e| &e.path).collect();
    let mut missing: Vec<&Entry> = old.iter().filter(|e| !new_paths.contains(&e.path)).collect();
    let mut added: Vec<&Entry> = new.iter().filter(|e| !old_paths.contains(&e.path)).collect();
    // Entries whose parent is itself missing go away with it.
    let missing_dirs: Vec<PathBuf> = missing.iter().filter(|e| e.is_dir).map(|e| e.path.clone()).collect();
    missing.retain(|e| !missing_dirs.iter().any(|d| e.path.starts_with(d) && e.path != *d));
    added.retain(|e| !added_under_new_dir(e, new, &old_paths));
    let mut changes = Changes::default();

    // A name that disappeared from one directory and appeared in another moved.
    let mut i = 0;
    while i < missing.len() {
        let gone = missing[i];
        let same_name = added.iter().position(|a| a.is_dir == gone.is_dir && a.path.file_name() == gone.path.file_name());
        if let Some(j) = same_name {
            changes.moves.push((gone.path.clone(), added[j].path.clone()));
            added.remove(j);
            missing.remove(i);
        } else {
            i += 1;
        }
    }
    // Within one directory, a missing and an added name make a rename: the
    // only pair there is, or pairs whose names clearly resemble each other.
    let mut dirs: Vec<PathBuf> = missing.iter().chain(added.iter()).filter_map(|e| e.path.parent().map(Path::to_path_buf)).collect();
    dirs.dedup();
    for dir in dirs {
        loop {
            let gone: Vec<usize> = missing.iter().enumerate().filter(|(_, e)| e.path.parent() == Some(&dir)).map(|(i, _)| i).collect();
            let new: Vec<usize> = added.iter().enumerate().filter(|(_, e)| e.path.parent() == Some(&dir)).map(|(i, _)| i).collect();
            if gone.is_empty() || new.is_empty() {
                break;
            }
            let pair = if gone.len() == 1 && new.len() == 1 {
                Some((gone[0], new[0]))
            } else {
                let mut best: Option<(f32, usize, usize)> = None;
                for &g in &gone {
                    for &n in &new {
                        let score = similarity(&file_name(&missing[g].path), &file_name(&added[n].path));
                        if score >= 0.5 && best.is_none_or(|(b, _, _)| score > b) {
                            best = Some((score, g, n));
                        }
                    }
                }
                best.map(|(_, g, n)| (g, n))
            };
            let Some((g, n)) = pair else {
                return Err(format!("ambiguous edit in {}: rename one entry per directory per write", dir.display()));
            };
            changes.moves.push((missing[g].path.clone(), added[n].path.clone()));
            missing.remove(g);
            added.remove(n);
        }
    }
    changes.deletes = missing.iter().map(|e| e.path.clone()).collect();
    changes.creates = added.iter().map(|e| (*e).clone()).collect();
    if let Some((taker, victim)) = takes_over_children(old, new, &changes.creates) {
        return Err(format!(
            "`{}/` would take the entries of `{}/`: indent it to put it inside, or fold `{}/` first",
            file_name(&taker),
            file_name(&victim),
            file_name(&victim)
        ));
    }
    Ok(changes)
}

/// The classic slip: a new directory line typed right below an unfolded
/// directory, at the same depth, so the children listed under it now read as
/// the new directory's. Returns (new directory, directory it would empty).
fn takes_over_children(old: &[Entry], new: &[Entry], creates: &[Entry]) -> Option<(PathBuf, PathBuf)> {
    let children = |entries: &[Entry], dir: &Path| entries.iter().any(|e| e.path.parent() == Some(dir));
    for created in creates.iter().filter(|e| e.is_dir) {
        let index = new.iter().position(|e| e.path == created.path)?;
        let Some(previous) = index.checked_sub(1).map(|i| &new[i]) else { continue };
        if previous.is_dir
            && previous.depth == created.depth
            && children(old, &previous.path)
            && !children(new, &previous.path)
            && children(new, &created.path)
        {
            return Some((created.path.clone(), previous.path.clone()));
        }
    }
    None
}

/// How alike two names are, ignoring the extension: shared prefix and suffix
/// over the longer stem (`draw.rs` vs `draw2.rs` = 0.8, `a.rs` vs `b.rs` = 0).
fn similarity(a: &str, b: &str) -> f32 {
    let stem = |s: &str| s.rsplit_once('.').map(|(stem, _)| stem.to_string()).filter(|s| !s.is_empty()).unwrap_or(s.to_string());
    let (a, b): (Vec<char>, Vec<char>) = (stem(a).chars().collect(), stem(b).chars().collect());
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 0.0;
    }
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a.iter().rev().zip(b.iter().rev()).take_while(|(x, y)| x == y).count().min(a.len().min(b.len()) - prefix);
    (prefix + suffix) as f32 / longest as f32
}

/// Children listed under a directory that is itself new are created with it.
fn added_under_new_dir(entry: &Entry, new: &[Entry], old_paths: &HashSet<&PathBuf>) -> bool {
    entry.path.ancestors().skip(1).any(|a| new.iter().any(|e| e.is_dir && e.path == a && !old_paths.contains(&e.path)))
}

/// Apply the buffer's edits to disk. Deletions need a second `:write`.
pub fn write(ed: &mut Editor) -> Result<(), String> {
    let Some(tree) = &ed.tree else { return Err("no tree".to_string()) };
    let text = ed.buffer().text.to_string();
    let new = parse(&tree.root, &text)?;
    let changes = diff(&tree.entries, &new)?;
    if changes == Changes::default() {
        render(ed);
        ed.set_message("tree: nothing to do");
        return Ok(());
    }
    let tree = ed.tree.as_ref().expect("tree");
    let confirmed = !changes.deletes.is_empty() && changes.deletes == tree.pending_deletes;
    if !changes.deletes.is_empty() && !confirmed {
        let names: Vec<String> = changes.deletes.iter().map(|p| file_name(p)).collect();
        ed.tree.as_mut().expect("tree").pending_deletes = changes.deletes.clone();
        return Err(format!("tree: :write again to delete {}", names.join(", ")));
    }

    let mut done = Vec::new();
    for (from, to) in &changes.moves {
        if to.exists() {
            return Err(format!("{} already exists", to.display()));
        }
        std::fs::rename(from, to).map_err(|e| format!("rename {}: {e}", from.display()))?;
        retarget_buffers(ed, from, to);
        done.push(format!("{} → {}", file_name(from), file_name(to)));
    }
    for entry in &changes.creates {
        if entry.is_dir {
            std::fs::create_dir_all(&entry.path).map_err(|e| format!("create {}: {e}", entry.path.display()))?;
        } else {
            if let Some(parent) = entry.path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            if !entry.path.exists() {
                std::fs::write(&entry.path, "").map_err(|e| format!("create {}: {e}", entry.path.display()))?;
            }
        }
        done.push(format!("+{}", file_name(&entry.path)));
    }
    for path in &changes.deletes {
        let result = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        result.map_err(|e| format!("delete {}: {e}", path.display()))?;
        done.push(format!("-{}", file_name(path)));
    }

    let tree = ed.tree.as_mut().expect("tree");
    tree.pending_deletes.clear();
    for entry in &new {
        if entry.is_dir && new.iter().any(|e| e.depth == entry.depth + 1 && e.path.parent() == Some(&entry.path)) {
            tree.expanded.insert(entry.path.clone());
        }
    }
    for (from, to) in &changes.moves {
        if tree.expanded.remove(from) {
            tree.expanded.insert(to.clone());
        }
    }
    render(ed);
    ed.file_list = None;
    ed.set_message(format!("tree: {}", done.join(", ")));
    Ok(())
}

/// Buffers that had a renamed file (or a file inside a renamed directory) open
/// follow it to the new path.
fn retarget_buffers(ed: &mut Editor, from: &Path, to: &Path) {
    for buffer in ed.buffers.values_mut() {
        if let Some(path) = &buffer.path
            && let Ok(rest) = path.strip_prefix(from)
        {
            buffer.path = Some(to.join(rest));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, is_dir: bool, depth: usize) -> Entry {
        Entry { path: PathBuf::from(path), is_dir, depth }
    }

    #[test]
    fn parses_indented_names() {
        let entries = parse(Path::new("/r"), "* src/\n    main.rs\n    ui/\n        draw.rs\nREADME.md\n").unwrap();
        assert_eq!(entries[0], entry("/r/src", true, 0), "the git marker is dropped");
        assert_eq!(entries[1], entry("/r/src/main.rs", false, 1));
        assert_eq!(entries[3], entry("/r/src/ui/draw.rs", false, 2));
        assert_eq!(entries[4], entry("/r/README.md", false, 0));
        assert!(parse(Path::new("/r"), "a.rs\n    b.rs\n").is_err(), "a file cannot have children");
        assert!(parse(Path::new("/r"), "        deep\n").is_err());
        assert!(parse(Path::new("/r"), "a/b\n").is_err());
    }

    #[test]
    fn diffs_renames_moves_creates_and_deletes() {
        let old = vec![entry("/r/a.rs", false, 0), entry("/r/b.rs", false, 0), entry("/r/d/", true, 0), entry("/r/d/x.rs", false, 1)];
        // b.rs renamed, x.rs moved to the root, a.rs deleted.
        let new = vec![entry("/r/b2.rs", false, 0), entry("/r/d/", true, 0), entry("/r/x.rs", false, 0)];
        let changes = diff(&old, &new).unwrap();
        assert_eq!(changes.moves, vec![("/r/d/x.rs".into(), "/r/x.rs".into()), ("/r/b.rs".into(), "/r/b2.rs".into())]);
        assert!(changes.creates.is_empty());
        assert_eq!(changes.deletes, vec![PathBuf::from("/r/a.rs")]);
        // A new directory with children creates only the directory entry; its children come with it.
        let mut with_new = old.clone();
        with_new.push(entry("/r/n/", true, 0));
        with_new.push(entry("/r/n/f.rs", false, 1));
        let changes = diff(&old, &with_new).unwrap();
        assert_eq!(changes.creates, vec![entry("/r/n/", true, 0)]);
        // Creating next to a deletion is fine; renaming while creating in one directory is not.
        let new = vec![entry("/r/b.rs", false, 0), entry("/r/c.rs", false, 0), entry("/r/d/", true, 0), entry("/r/d/x.rs", false, 1)];
        assert_eq!(diff(&old, &new).unwrap().moves, vec![("/r/a.rs".into(), "/r/c.rs".into())]);
        // Two renames in one directory are paired by how alike the names are.
        let new = vec![entry("/r/a2.rs", false, 0), entry("/r/b2.rs", false, 0), entry("/r/d/", true, 0), entry("/r/d/x.rs", false, 1)];
        assert_eq!(diff(&old, &new).unwrap().moves, vec![("/r/a.rs".into(), "/r/a2.rs".into()), ("/r/b.rs".into(), "/r/b2.rs".into())]);
        // Unrelated names cannot be paired: refuse rather than guess.
        let new = vec![entry("/r/zz.rs", false, 0), entry("/r/qq.rs", false, 0), entry("/r/d/", true, 0), entry("/r/d/x.rs", false, 1)];
        assert!(diff(&old, &new).is_err());
        assert!(similarity("draw.rs", "draw2.rs") > 0.7 && similarity("a.rs", "b.rs") == 0.0);
        // Deleting a directory line deletes it once, not its children separately.
        let new = vec![entry("/r/a.rs", false, 0), entry("/r/b.rs", false, 0)];
        assert_eq!(diff(&old, &new).unwrap().deletes, vec![PathBuf::from("/r/d/")]);
        // A new directory typed under an unfolded one at the same depth would
        // swallow its children: refuse instead of moving everything.
        let new = vec![
            entry("/r/a.rs", false, 0),
            entry("/r/b.rs", false, 0),
            entry("/r/d/", true, 0),
            entry("/r/t/", true, 0),
            entry("/r/t/x.rs", false, 1),
        ];
        assert!(diff(&old, &new).unwrap_err().contains("`t/` would take the entries of `d/`"));
        // Listing it after the children keeps it an ordinary empty sibling.
        let new = vec![
            entry("/r/a.rs", false, 0),
            entry("/r/b.rs", false, 0),
            entry("/r/d/", true, 0),
            entry("/r/d/x.rs", false, 1),
            entry("/r/t/", true, 0),
        ];
        assert_eq!(diff(&old, &new).unwrap().creates, vec![entry("/r/t/", true, 0)]);
    }
}
