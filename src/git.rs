//! Light git awareness: the branch and what changed, read by running the
//! `git` binary on a background thread (no git library). The status line
//! shows the branch, the file tree marks changed files and directories, and
//! `space l` opens a status buffer listing the changes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use crate::buffer::BufferId;
use crate::editor::Editor;
use crate::frame::ViewId;
use crate::terminal::WakeFn;

/// How often the status is re-read while the editor is in use.
const REFRESH_EVERY: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// Changed, deleted or renamed compared to HEAD.
    Modified,
    /// Untracked or newly added.
    New,
}

/// One line of `git status`: the two-letter code and the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: PathBuf,
    /// State in the index (staged) and in the working tree, as git prints them.
    pub index: u8,
    pub worktree: u8,
}

impl StatusEntry {
    fn untracked(&self) -> bool {
        self.index == b'?'
    }

    fn staged(&self) -> bool {
        !self.untracked() && self.index != b' '
    }

    fn unstaged(&self) -> bool {
        !self.untracked() && self.worktree != b' '
    }
}

/// The repository containing the working directory, as of the last query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitState {
    pub root: PathBuf,
    pub branch: String,
    /// Every changed file, in git's order.
    pub entries: Vec<StatusEntry>,
    /// Absolute paths of changed files.
    pub files: HashMap<PathBuf, FileStatus>,
    /// Absolute paths of directories with a change somewhere inside.
    pub dirs: HashSet<PathBuf>,
}

impl GitState {
    pub fn dirty(&self) -> bool {
        !self.files.is_empty()
    }

    pub fn status(&self, path: &Path) -> Option<FileStatus> {
        self.files.get(path).copied()
    }

    pub fn dir_changed(&self, dir: &Path) -> bool {
        self.dirs.contains(dir)
    }

    /// Branch name plus `*` when the tree has changes, for the status line.
    pub fn label(&self) -> String {
        if self.dirty() { format!("{}*", self.branch) } else { self.branch.clone() }
    }
}

/// Runs `git` in the background every few seconds and keeps the last answer.
pub struct GitWatcher {
    pub state: Option<GitState>,
    pending: Option<Receiver<Option<GitState>>>,
    last_started: Option<Instant>,
    queried_cwd: PathBuf,
    /// A refresh was asked for while one was running: run again right after.
    rerun: bool,
}

impl GitWatcher {
    pub fn new() -> GitWatcher {
        GitWatcher { state: None, pending: None, last_started: None, queried_cwd: PathBuf::new(), rerun: false }
    }

    /// A query is running.
    pub fn in_flight(&self) -> bool {
        self.pending.is_some()
    }

    /// Pick up a finished query and start a new one when due (or `now`).
    /// Returns true when the known state changed.
    pub fn poll(&mut self, cwd: &Path, wake: WakeFn, now: bool) -> bool {
        let mut changed = false;
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok(state) => {
                    changed = state != self.state;
                    self.state = state;
                    self.pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.rerun |= now;
                    return false;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pending = None,
            }
        }
        let due = now || self.rerun || cwd != self.queried_cwd || self.last_started.is_none_or(|t| t.elapsed() >= REFRESH_EVERY);
        if due && self.pending.is_none() {
            self.rerun = false;
            self.queried_cwd = cwd.to_path_buf();
            self.last_started = Some(Instant::now());
            let (tx, rx) = channel();
            let cwd = cwd.to_path_buf();
            std::thread::spawn(move || {
                let _ = tx.send(query(&cwd));
                wake();
            });
            self.pending = Some(rx);
        }
        changed
    }
}

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Ask git about the repository holding `cwd`. `None` outside a repository
/// or without git installed.
pub fn query(cwd: &Path) -> Option<GitState> {
    let top = git(cwd, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(top.trim());
    let mut branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).map(|b| b.trim().to_string()).unwrap_or_default();
    if branch.is_empty() || branch == "HEAD" {
        // Detached head (or no commit yet): show the short hash, or nothing.
        branch = git(cwd, &["rev-parse", "--short", "HEAD"]).map(|h| h.trim().to_string()).unwrap_or_else(|| "no commits".to_string());
    }
    let status = git(cwd, &["status", "--porcelain", "-z", "--untracked-files=all"]).unwrap_or_default();
    let (entries, files, dirs) = parse_status(&root, &status);
    Some(GitState { root, branch, entries, files, dirs })
}

/// Parse `git status --porcelain -z`: `XY path\0`, with a second
/// `original\0` field after renames and copies.
#[allow(clippy::type_complexity)]
fn parse_status(root: &Path, status: &str) -> (Vec<StatusEntry>, HashMap<PathBuf, FileStatus>, HashSet<PathBuf>) {
    let mut entries = Vec::new();
    let mut files = HashMap::new();
    let mut dirs = HashSet::new();
    let mut fields = status.split('\0').filter(|f| !f.is_empty());
    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue;
        }
        let (code, path) = entry.split_at(3);
        let code = &code[..2];
        let (index, worktree) = (code.as_bytes()[0], code.as_bytes()[1]);
        if matches!(index, b'R' | b'C') {
            fields.next(); // the path it was renamed or copied from
        }
        let status = if code == "??" || (index == b'A' && worktree != b'D') { FileStatus::New } else { FileStatus::Modified };
        let path = root.join(path.trim_end_matches('/'));
        for ancestor in path.ancestors().skip(1) {
            if !ancestor.starts_with(root) {
                break;
            }
            dirs.insert(ancestor.to_path_buf());
        }
        entries.push(StatusEntry { path: path.clone(), index, worktree });
        files.insert(path, status);
    }
    (entries, files, dirs)
}

// ---------------------------------------------------------------------------
// The status buffer (`space l`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Untracked,
    Unstaged,
    Staged,
}

/// What a line of the status buffer stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Plain,
    /// A section heading: `s` stages or unstages everything under it.
    Section(Section),
    File {
        section: Section,
        path: PathBuf,
    },
    /// A hunk header or one of its lines; `line` is the matching line in the
    /// working file (0-based), for `ret`.
    Hunk {
        section: Section,
        path: PathBuf,
        hunk: usize,
        line: usize,
    },
}

/// One hunk of a diff, header included, exactly as git printed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub lines: Vec<String>,
    /// First line of the hunk in the new file (1-based, from `@@ -a,b +c,d @@`).
    pub new_start: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// The `diff --git` ... `+++` lines, needed in front of a hunk to apply it.
    pub header: Vec<String>,
    pub hunks: Vec<Hunk>,
}

/// The buffer showing the status, what its lines mean, which files are
/// unfolded to their diff, and the frame files open in.
pub struct StatusBuffer {
    pub buffer: BufferId,
    pub lines: Vec<Line>,
    pub expanded: HashSet<(Section, PathBuf)>,
    /// The read-only split on the right of the status buffer that `ret` reuses.
    pub file_view: Option<ViewId>,
    /// How the file shown in that split differs from HEAD, for colouring it.
    pub file_marks: Option<FileMarks>,
}

/// A changed line of a previewed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineChange {
    /// New line (green); otherwise a modified one (yellow).
    pub added: bool,
    /// Character ranges that differ from the old line (modified lines only).
    pub spans: Vec<(usize, usize)>,
}

/// Where a previewed file differs from HEAD, by line of the working file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileMarks {
    pub path: PathBuf,
    pub lines: HashMap<usize, LineChange>,
    /// Lines in front of which something was deleted (a red marker is drawn).
    pub deleted_before: HashSet<usize>,
}

/// Work out line marks from a diff: runs of `-` then `+` lines pair up as
/// modifications (with the differing span), leftovers are additions or
/// deletions.
pub fn marks_from_diff(path: &Path, diff: &Diff) -> FileMarks {
    let mut marks = FileMarks { path: path.to_path_buf(), ..FileMarks::default() };
    for hunk in &diff.hunks {
        let mut line = hunk.new_start.saturating_sub(1);
        let mut removed: Vec<String> = Vec::new();
        let mut added: Vec<(usize, String)> = Vec::new();
        let flush = |removed: &mut Vec<String>, added: &mut Vec<(usize, String)>, line: usize, marks: &mut FileMarks| {
            let pairs = removed.len().min(added.len());
            for i in 0..pairs {
                let (new_line, new_text) = &added[i];
                marks.lines.insert(*new_line, LineChange { added: false, spans: changed_spans(&removed[i], new_text) });
            }
            for (new_line, _) in added.iter().skip(pairs) {
                marks.lines.insert(*new_line, LineChange { added: true, spans: Vec::new() });
            }
            if removed.len() > pairs {
                marks.deleted_before.insert(line);
            }
            removed.clear();
            added.clear();
        };
        for raw in hunk.lines.iter().skip(1) {
            match raw.chars().next() {
                Some('-') => {
                    // git prints a block's `-` lines before its `+` lines: a `-`
                    // after `+` lines belongs to the next block.
                    if !added.is_empty() {
                        flush(&mut removed, &mut added, line, &mut marks);
                    }
                    removed.push(raw[1..].to_string());
                }
                Some('+') => {
                    added.push((line, raw[1..].to_string()));
                    line += 1;
                }
                Some('\\') => {}
                _ => {
                    flush(&mut removed, &mut added, line, &mut marks);
                    line += 1;
                }
            }
        }
        flush(&mut removed, &mut added, line, &mut marks);
    }
    marks
}

/// The part of `new` that differs from `old`: everything between the common
/// prefix and the common suffix, widened to whole words so that a changed
/// identifier lights up entirely (the whole line when nothing is shared).
fn changed_spans(old: &str, new: &str) -> Vec<(usize, usize)> {
    let (old, new): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old.iter().rev().zip(new.iter().rev()).take_while(|(a, b)| a == b).count().min(new.len() - prefix).min(old.len() - prefix);
    let (mut start, mut end) = (prefix, new.len() - suffix);
    if end <= start {
        return Vec::new();
    }
    let word = |c: &char| c.is_alphanumeric() || *c == '_';
    while start > 0 && word(&new[start - 1]) && word(&new[start]) {
        start -= 1;
    }
    while end < new.len() && word(&new[end]) && word(&new[end - 1]) {
        end += 1;
    }
    vec![(start, end)]
}

/// The marks to draw in `view`, when it is the git preview frame showing the
/// previewed file unmodified.
pub fn marks_for(ed: &Editor, view: ViewId) -> Option<FileMarks> {
    let status = ed.git_buffer.as_ref()?;
    if status.file_view != Some(view) {
        return None;
    }
    let marks = status.file_marks.as_ref()?;
    let buffer = &ed.buffers[&ed.views.get(&view)?.buffer];
    (buffer.path.as_deref() == Some(&marks.path) && !buffer.is_modified()).then(|| marks.clone())
}

fn describe(code: u8) -> &'static str {
    match code {
        b'M' => "modified",
        b'A' => "new file",
        b'D' => "deleted",
        b'R' => "renamed",
        b'C' => "copied",
        b'T' => "type changed",
        b'U' => "conflict",
        _ => "changed",
    }
}

/// Run git with stdin; returns (succeeded, stdout).
fn run_git(cwd: &Path, args: &[&str], input: Option<&str>) -> (bool, String) {
    let mut command = Command::new("git");
    command.arg("-C").arg(cwd).args(args).env("GIT_OPTIONAL_LOCKS", "0").stdout(Stdio::piped()).stderr(Stdio::piped());
    command.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let Ok(mut child) = command.spawn() else { return (false, String::new()) };
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        use std::io::Write;
        let _ = stdin.write_all(input.as_bytes());
    }
    match child.wait_with_output() {
        Ok(output) => {
            let text = if output.status.success() || !output.stdout.is_empty() { &output.stdout } else { &output.stderr };
            (output.status.success(), String::from_utf8_lossy(text).into_owned())
        }
        Err(_) => (false, String::new()),
    }
}

/// The diff of one file in one section. Untracked files diff against nothing.
pub fn diff_for(root: &Path, section: Section, path: &Path) -> Diff {
    let path = path.to_string_lossy();
    let (_, output) = match section {
        Section::Untracked => run_git(root, &["diff", "--no-color", "--no-index", "--", "/dev/null", &path], None),
        Section::Unstaged => run_git(root, &["diff", "--no-color", "--", &path], None),
        Section::Staged => run_git(root, &["diff", "--no-color", "--cached", "--", &path], None),
    };
    parse_diff(&output)
}

fn parse_diff(output: &str) -> Diff {
    let mut diff = Diff { header: Vec::new(), hunks: Vec::new() };
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("@@ ") {
            let new_start = rest.split_once('+').and_then(|(_, after)| after.split([',', ' ']).next()?.parse().ok()).unwrap_or(1);
            diff.hunks.push(Hunk { lines: vec![line.to_string()], new_start });
        } else if let Some(hunk) = diff.hunks.last_mut() {
            hunk.lines.push(line.to_string());
        } else {
            diff.header.push(line.to_string());
        }
    }
    diff
}

/// The text of the status buffer and what each line means.
pub fn status_text(
    state: Option<&GitState>,
    expanded: &HashSet<(Section, PathBuf)>,
    diffs: &HashMap<(Section, PathBuf), Diff>,
) -> (String, Vec<Line>) {
    let mut text = String::new();
    let mut lines: Vec<Line> = Vec::new();
    let mut push = |line: &str, kind: Line| {
        text.push_str(line);
        text.push('\n');
        lines.push(kind);
    };
    let Some(state) = state else {
        push("Not a git repository.", Line::Plain);
        return (text, lines);
    };
    push(&format!("Head:  {}", state.branch), Line::Plain);
    push(&format!("Root:  {}", state.root.display()), Line::Plain);
    push("", Line::Plain);
    if state.entries.is_empty() {
        push("Nothing changed.", Line::Plain);
        return (text, lines);
    }
    let relative = |path: &Path| path.strip_prefix(&state.root).unwrap_or(path).to_string_lossy().replace('\\', "/");
    type Listed<'a> = Vec<(&'a StatusEntry, &'a str)>;
    let sections: [(Section, &str, Listed); 3] = [
        (Section::Untracked, "Untracked files", state.entries.iter().filter(|e| e.untracked()).map(|e| (e, "untracked")).collect()),
        (Section::Unstaged, "Unstaged changes", state.entries.iter().filter(|e| e.unstaged()).map(|e| (e, describe(e.worktree))).collect()),
        (Section::Staged, "Staged changes", state.entries.iter().filter(|e| e.staged()).map(|e| (e, describe(e.index))).collect()),
    ];
    for (section, title, entries) in sections {
        if entries.is_empty() {
            continue;
        }
        push(&format!("{title} ({})", entries.len()), Line::Section(section));
        for (entry, word) in entries {
            let key = (section, entry.path.clone());
            let marker = if expanded.contains(&key) { "v" } else { ">" };
            push(&format!("{marker} {word:<13}{}", relative(&entry.path)), Line::File { section, path: entry.path.clone() });
            let Some(diff) = diffs.get(&key) else { continue };
            for (hunk_index, hunk) in diff.hunks.iter().enumerate() {
                let mut line = hunk.new_start.saturating_sub(1);
                for (i, raw) in hunk.lines.iter().enumerate() {
                    push(&format!("    {raw}"), Line::Hunk { section, path: entry.path.clone(), hunk: hunk_index, line });
                    // `+` and context lines advance in the new file; `-` lines do not.
                    if i > 0 && !raw.starts_with('-') && !raw.starts_with('\\') {
                        line += 1;
                    }
                }
            }
        }
        push("", Line::Plain);
    }
    (text, lines)
}

/// `space l`: show the status buffer in its own tab (reusing it when open).
pub fn open_status(ed: &mut Editor) {
    ed.poll_git(true);
    let existing = ed.git_buffer.as_ref().map(|s| s.buffer).filter(|id| ed.buffers.contains_key(id));
    let buffer = match existing {
        Some(id) => id,
        None => {
            let id = ed.new_scratch_buffer();
            let buffer = ed.buffers.get_mut(&id).expect("new buffer");
            buffer.git = true;
            buffer.readonly = true;
            buffer.set_language(ed.registry.find("git-status"));
            ed.git_buffer =
                Some(StatusBuffer { buffer: id, lines: Vec::new(), expanded: HashSet::new(), file_view: None, file_marks: None });
            id
        }
    };
    render_status(ed);
    if ed.view().buffer == buffer {
        return;
    }
    let shown_in = ed.tabs.iter().position(|t| t.root.leaves().iter().any(|v| ed.views[v].buffer == buffer));
    match shown_in {
        Some(tab) => {
            ed.tab_goto(tab);
            let view = ed.tab().root.leaves().into_iter().find(|v| ed.views[v].buffer == buffer).expect("shown");
            ed.focus_view(view);
        }
        None => ed.tab_new(Some(buffer)),
    }
}

/// Rewrite the status buffer from the current state, keeping the cursor line.
pub fn render_status(ed: &mut Editor) {
    let Some(status) = &ed.git_buffer else { return };
    let id = status.buffer;
    let mut diffs = HashMap::new();
    if let Some(state) = &ed.git.state {
        for key in &status.expanded {
            diffs.insert(key.clone(), diff_for(&state.root, key.0, &key.1));
        }
    }
    let (text, lines) = status_text(ed.git.state.as_ref(), &status.expanded, &diffs);
    if let Some(buffer) = ed.buffers.get_mut(&id) {
        let line_of = |pos: usize, buffer: &crate::buffer::Buffer| buffer.line_of(pos);
        let cursor_lines: Vec<(ViewId, usize)> =
            ed.views.values().filter(|v| v.buffer == id).map(|v| (v.id, line_of(v.cursor(), buffer))).collect();
        buffer.set_text(&text);
        let last = buffer.line_count().saturating_sub(1);
        for (view_id, line) in cursor_lines {
            let pos = buffer.first_nonblank(line.min(last));
            if let Some(view) = ed.views.get_mut(&view_id) {
                view.selection = crate::selection::Selection::point(pos);
            }
        }
    }
    if let Some(status) = &mut ed.git_buffer {
        status.lines = lines;
    }
    refresh_marks(ed);
}

/// Recompute the colouring of the previewed file against HEAD.
fn refresh_marks(ed: &mut Editor) {
    let Some(status) = &ed.git_buffer else { return };
    let (Some(root), Some(marks)) = (ed.git.state.as_ref().map(|s| s.root.clone()), &status.file_marks) else { return };
    let path = marks.path.clone();
    let untracked = ed.git.state.as_ref().is_some_and(|s| s.entries.iter().any(|e| e.path == path && e.untracked()));
    let fresh = marks_from_diff(&path, &diff_against_head(&root, &path, untracked));
    if let Some(status) = &mut ed.git_buffer {
        status.file_marks = Some(fresh);
    }
}

/// Everything that differs between HEAD and the working file (staged or not).
fn diff_against_head(root: &Path, path: &Path, untracked: bool) -> Diff {
    let path = path.to_string_lossy();
    let (_, output) = if untracked {
        run_git(root, &["diff", "--no-color", "--no-index", "--", "/dev/null", &path], None)
    } else {
        run_git(root, &["diff", "--no-color", "HEAD", "--", &path], None)
    };
    parse_diff(&output)
}

/// The meaning of the line under the cursor in the status buffer.
fn line_under_cursor(ed: &Editor) -> Line {
    let line = ed.buffer().line_of(ed.view().cursor());
    ed.git_buffer.as_ref().and_then(|s| s.lines.get(line).cloned()).unwrap_or(Line::Plain)
}

/// `ret`: open the file (at the hunk's line) in the read-only split to the
/// right of the status buffer, creating it once, and move into it.
pub fn activate(ed: &mut Editor) {
    let (path, line) = match line_under_cursor(ed) {
        Line::File { path, .. } => (path, None),
        Line::Hunk { path, line, .. } => (path, Some(line)),
        Line::Plain | Line::Section(_) => return,
    };
    let leaves = ed.root().leaves();
    let existing = ed.git_buffer.as_ref().and_then(|s| s.file_view).filter(|v| leaves.contains(v));
    let file_view = match existing {
        Some(view) => view,
        None => {
            let view = ed.split(crate::frame::Dir::Horizontal, None);
            ed.views.get_mut(&view).expect("new view").readonly = true;
            if let Some(status) = &mut ed.git_buffer {
                status.file_view = Some(view);
            }
            view
        }
    };
    if let Some(status) = &mut ed.git_buffer {
        status.file_marks = Some(FileMarks { path: path.clone(), ..FileMarks::default() });
    }
    refresh_marks(ed);
    ed.focus_view(file_view);
    crate::overlays::open_path(ed, &path, line, crate::picker::OpenMode::Here);
    if let (Some(line), Some(view)) = (line, ed.views.get_mut(&file_view)) {
        view.scroll_line = line.saturating_sub(view.rows.max(1) / 2);
        view.anim = None;
    }
}

/// `esc` in the status buffer: close the preview split. Returns false when
/// there is none to close.
pub fn close_file_view(ed: &mut Editor) -> bool {
    let Some(view) = ed.git_buffer.as_ref().and_then(|s| s.file_view) else { return false };
    if !ed.root().leaves().contains(&view) {
        return false;
    }
    ed.close_view(view);
    if let Some(status) = &mut ed.git_buffer {
        status.file_view = None;
        status.file_marks = None;
    }
    true
}

/// `tab`: fold or unfold the diff of the file under the cursor.
pub fn toggle_diff(ed: &mut Editor) {
    let key = match line_under_cursor(ed) {
        Line::File { section, path } | Line::Hunk { section, path, .. } => (section, path),
        Line::Plain | Line::Section(_) => return,
    };
    if let Some(status) = &mut ed.git_buffer
        && !status.expanded.remove(&key)
    {
        status.expanded.insert(key);
    }
    render_status(ed);
}

/// `s` on a file or hunk: stage it (unstage it in the staged section). On a
/// section heading, every file of the section.
pub fn stage(ed: &mut Editor) {
    let Some(state) = ed.git.state.clone() else { return };
    let root = state.root.clone();
    let (ok, output) = match line_under_cursor(ed) {
        Line::Section(section) => {
            let paths: Vec<String> = state
                .entries
                .iter()
                .filter(|e| match section {
                    Section::Untracked => e.untracked(),
                    Section::Unstaged => e.unstaged(),
                    Section::Staged => e.staged(),
                })
                .map(|e| e.path.to_string_lossy().into_owned())
                .collect();
            let mut args: Vec<&str> = if section == Section::Staged { vec!["restore", "--staged", "--"] } else { vec!["add", "--"] };
            args.extend(paths.iter().map(String::as_str));
            run_git(&root, &args, None)
        }
        Line::File { section: Section::Staged, path } => run_git(&root, &["restore", "--staged", "--", &path.to_string_lossy()], None),
        Line::File { path, .. } | Line::Hunk { section: Section::Untracked, path, .. } => {
            run_git(&root, &["add", "--", &path.to_string_lossy()], None)
        }
        Line::Hunk { section, path, hunk, .. } => {
            let diff = diff_for(&root, section, &path);
            let Some(hunk) = diff.hunks.get(hunk) else { return };
            let mut patch = diff.header.join("\n");
            patch.push('\n');
            patch.push_str(&hunk.lines.join("\n"));
            patch.push('\n');
            let args: &[&str] =
                if section == Section::Staged { &["apply", "--cached", "--reverse", "-"] } else { &["apply", "--cached", "-"] };
            run_git(&root, args, Some(&patch))
        }
        Line::Plain => return,
    };
    if ok {
        ed.set_message("staged");
    } else {
        ed.set_error(format!("git: {}", output.trim()));
    }
    ed.poll_git(true);
}

// ---------------------------------------------------------------------------
// Committing (`c c`, `c a`)
// ---------------------------------------------------------------------------

/// Open a floating commit-message buffer, prefilled like `git commit` does.
/// `:write` commits, `:quit` cancels.
pub fn open_commit(ed: &mut Editor, amend: bool) {
    let Some(state) = ed.git.state.clone() else {
        ed.set_error("not a git repository");
        return;
    };
    if !amend && !state.entries.iter().any(|e| e.staged()) {
        ed.set_error("nothing staged to commit (stage with `s`)");
        return;
    }
    let mut text = String::new();
    if amend {
        let (_, previous) = run_git(&state.root, &["log", "-1", "--pretty=%B"], None);
        text.push_str(previous.trim_end());
    }
    text.push('\n');
    text.push_str("\n# Please enter the commit message for your changes. Lines starting\n# with '#' will be ignored, and an empty message aborts the commit.\n#\n");
    text.push_str(&format!("# On branch {}\n", state.branch));
    let relative = |path: &Path| path.strip_prefix(&state.root).unwrap_or(path).to_string_lossy().replace('\\', "/");
    let groups: [(&str, Vec<String>); 3] = [
        (
            "Changes to be committed:",
            state.entries.iter().filter(|e| e.staged()).map(|e| format!("#\t{}:   {}", describe(e.index), relative(&e.path))).collect(),
        ),
        (
            "Changes not staged for commit:",
            state
                .entries
                .iter()
                .filter(|e| e.unstaged())
                .map(|e| format!("#\t{}:   {}", describe(e.worktree), relative(&e.path)))
                .collect(),
        ),
        ("Untracked files:", state.entries.iter().filter(|e| e.untracked()).map(|e| format!("#\t{}", relative(&e.path))).collect()),
    ];
    for (title, lines) in groups {
        if !lines.is_empty() {
            text.push_str(&format!("#\n# {title}\n{}\n", lines.join("\n")));
        }
    }
    let id = ed.new_scratch_buffer();
    let buffer = ed.buffers.get_mut(&id).expect("new buffer");
    buffer.set_text(&text);
    buffer.history = crate::history::History::new();
    buffer.commit = Some(amend);
    buffer.set_language(ed.registry.find("git-commit"));
    ed.open_float(id);
    ed.set_mode(crate::editor::Mode::Insert);
    ed.set_message(if amend { ":write amends the commit, :quit cancels" } else { ":write commits, :quit cancels" });
}

/// `:write` in the commit buffer: commit with its text (comments stripped),
/// then close it. An empty message cancels instead.
pub fn commit_from_buffer(ed: &mut Editor, amend: bool) -> Result<(), String> {
    let message: String = ed.buffer().text.to_string().lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>().join("\n");
    if message.trim().is_empty() {
        ed.close_float();
        ed.set_message("commit cancelled: empty message");
        return Ok(());
    }
    let Some(root) = ed.git.state.as_ref().map(|s| s.root.clone()) else { return Err("not a git repository".to_string()) };
    let mut args = vec!["commit", "--cleanup=strip", "-F", "-"];
    if amend {
        args.push("--amend");
    }
    let (ok, output) = run_git(&root, &args, Some(&message));
    if !ok {
        return Err(format!("git: {}", output.trim()));
    }
    ed.close_float();
    ed.set_message(output.lines().next().unwrap_or("committed").to_string());
    ed.poll_git(true);
    Ok(())
}

/// `:quit` in the commit buffer: drop the message.
pub fn cancel_commit(ed: &mut Editor) {
    ed.close_float();
    ed.set_message("commit cancelled");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_status() {
        let root = Path::new("/r");
        let status = " M src/a.rs\0?? new.txt\0A  added.rs\0R  renamed.rs\0old.rs\0 D gone/deep/x.rs\0";
        let (entries, files, dirs) = parse_status(root, status);
        assert_eq!(entries.len(), 5);
        assert!(entries[0].unstaged() && !entries[0].staged());
        assert!(entries[2].staged() && !entries[2].unstaged());
        assert!(entries[1].untracked());
        assert_eq!(files[&PathBuf::from("/r/src/a.rs")], FileStatus::Modified);
        assert_eq!(files[&PathBuf::from("/r/new.txt")], FileStatus::New);
        assert_eq!(files[&PathBuf::from("/r/added.rs")], FileStatus::New);
        assert_eq!(files[&PathBuf::from("/r/renamed.rs")], FileStatus::Modified);
        assert!(!files.contains_key(&PathBuf::from("/r/old.rs")), "the rename source is not an entry");
        assert_eq!(files[&PathBuf::from("/r/gone/deep/x.rs")], FileStatus::Modified);
        assert!(dirs.contains(&PathBuf::from("/r/gone")) && dirs.contains(&PathBuf::from("/r/gone/deep")) && dirs.contains(root));
        assert!(!dirs.contains(&PathBuf::from("/")));
    }

    #[test]
    fn marks_pair_removed_and_added_lines() {
        let diff =
            parse_diff("--- a/f\n+++ b/f\n@@ -1,4 +1,5 @@\n keep\n-let x = old_name(1);\n+let x = new_name(1);\n+added\n-gone\n keep2\n");
        let marks = marks_from_diff(Path::new("/r/f"), &diff);
        assert_eq!(marks.lines[&1], LineChange { added: false, spans: vec![(8, 16)] }, "only `new_name` differs");
        assert_eq!(marks.lines[&2], LineChange { added: true, spans: Vec::new() });
        assert!(marks.deleted_before.contains(&3), "`gone` was removed in front of keep2");
        assert_eq!(changed_spans("same", "same"), Vec::<(usize, usize)>::new());
        assert_eq!(changed_spans("abc", "xyz"), vec![(0, 3)]);
    }

    #[test]
    fn status_text_lists_sections_or_says_nothing() {
        let root = PathBuf::from("/r");
        let (entries, files, dirs) = parse_status(&root, " M src/a.rs\0?? new.txt\0A  added.rs\0");
        let state = GitState { root: root.clone(), branch: "main".into(), entries, files, dirs };
        let none = (HashSet::new(), HashMap::new());
        let (text, lines) = status_text(Some(&state), &none.0, &none.1);
        assert!(text.starts_with("Head:  main\n"));
        assert!(text.contains("Untracked files (1)\n> untracked    new.txt\n"));
        assert!(text.contains("Unstaged changes (1)\n> modified     src/a.rs\n"));
        assert!(text.contains("Staged changes (1)\n> new file     added.rs\n"));
        assert_eq!(lines.iter().filter(|l| matches!(l, Line::File { .. })).count(), 3);
        // Unfolded: the hunk lines follow the file, each knowing its line in the file.
        let key = (Section::Unstaged, root.join("src/a.rs"));
        let diff =
            parse_diff("diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -3,2 +3,3 @@ fn x\n ctx\n-old\n+new\n+more\n");
        assert_eq!(diff.header.len(), 3);
        assert_eq!(diff.hunks[0].new_start, 3);
        let expanded = HashSet::from([key.clone()]);
        let diffs = HashMap::from([(key, diff)]);
        let (text, lines) = status_text(Some(&state), &expanded, &diffs);
        assert!(text.contains("v modified     src/a.rs\n    @@ -3,2 +3,3 @@ fn x\n     ctx\n    -old\n    +new\n    +more\n"), "{text}");
        let hunk_lines: Vec<usize> =
            lines.iter().filter_map(|l| if let Line::Hunk { line, .. } = l { Some(*line) } else { None }).collect();
        assert_eq!(hunk_lines, vec![2, 2, 3, 3, 4], "header, ctx (line 3), -old (still 3), +new (4), +more (5), 0-based");
        let clean = GitState { entries: Vec::new(), files: HashMap::new(), dirs: HashSet::new(), ..state };
        assert!(status_text(Some(&clean), &none.0, &none.1).0.ends_with("Nothing changed.\n"));
        assert!(status_text(None, &none.0, &none.1).0.contains("Not a git repository"));
    }
}
