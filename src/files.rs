//! Walking directories for the file picker and global search.
//! Hidden entries, a few well-known build directories and `.gitignore` patterns
//! (the common simple forms) are skipped, unless everything is asked for.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};

use regex::Regex;

use crate::terminal::WakeFn;

const ALWAYS_SKIP: &[&str] = &[".git", "target", "node_modules", "__pycache__", ".cache", "dist", "build", ".venv"];

pub const FILE_LIMIT: usize = 100_000;

/// Files below `root`, as paths relative to it, in walk order. With `all`,
/// only `.git` is skipped.
pub fn list_files(root: &Path, limit: usize, all: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut ignores: Vec<Ignore> = Vec::new();
    walk(root, root, &mut ignores, &mut out, limit, all);
    out
}

fn walk(root: &Path, dir: &Path, ignores: &mut Vec<Ignore>, out: &mut Vec<PathBuf>, limit: usize, all: bool) {
    let pushed = match if all { None } else { Ignore::load(dir) } {
        Some(ignore) => {
            ignores.push(ignore);
            true
        }
        None => false,
    };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if out.len() >= limit {
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(file_type) = entry.file_type() else { continue };
        let is_dir = file_type.is_dir();
        let hidden = name.starts_with('.');
        if (is_dir && name == ".git") || (!all && (hidden || (is_dir && ALWAYS_SKIP.contains(&name)))) {
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        if ignores.iter().any(|i| i.matches(&relative, name, is_dir)) {
            continue;
        }
        if is_dir {
            walk(root, &path, ignores, out, limit, all);
        } else if file_type.is_file() {
            out.push(relative);
        }
    }
    if pushed {
        ignores.pop();
    }
}

/// The patterns of one `.gitignore` file, compiled to regexes on the relative path.
struct Ignore {
    patterns: Vec<(Regex, bool)>, // (regex, directories only)
}

impl Ignore {
    fn load(dir: &Path) -> Option<Ignore> {
        let source = std::fs::read_to_string(dir.join(".gitignore")).ok()?;
        let mut patterns = Vec::new();
        for raw in source.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            let dir_only = line.ends_with('/');
            if let Some(regex) = glob_to_regex(line) {
                patterns.push((regex, dir_only));
            }
        }
        Some(Ignore { patterns })
    }

    fn matches(&self, relative: &Path, name: &str, is_dir: bool) -> bool {
        let rel = relative.to_string_lossy().replace('\\', "/");
        self.patterns.iter().any(|(regex, dir_only)| (!dir_only || is_dir) && (regex.is_match(name) || regex.is_match(&rel)))
    }
}

/// Convert a gitignore glob to a regex matching the whole name (or path when
/// the pattern contains a slash).
fn glob_to_regex(pattern: &str) -> Option<Regex> {
    let pattern = pattern.trim_matches('/');
    let anchored = pattern.contains('/');
    let mut out = String::from("^");
    if !anchored {
        out.push_str("(?:.*/)?");
    }
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    out.push_str(".*");
                } else {
                    out.push_str("[^/]*");
                }
            }
            '?' => out.push_str("[^/]"),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push_str("(?:/.*)?$");
    Regex::new(&out).ok()
}

pub struct GrepHit {
    /// Relative to the search root.
    pub path: PathBuf,
    pub line: usize,
    /// The matching line, trimmed and cut at 200 characters.
    pub text: String,
    /// Character range of the match inside `text`.
    pub matched: (usize, usize),
}

/// Entries of one directory for the browsing file picker: directories first
/// (with a trailing `/`), everything visible, sorted by name.
pub fn list_dir(dir: &Path) -> Vec<(String, bool)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(String, bool)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            let is_dir = entry.file_type().ok()?.is_dir();
            Some((if is_dir { format!("{name}/") } else { name }, is_dir))
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    out
}

/// Search `files` (relative to `root`) for a regex on a background thread.
/// Hits arrive on the returned channel, which closes when the search is over;
/// `wake` is called when new hits are waiting. Setting `cancel` stops the
/// search at the next file.
pub fn grep_in_background(
    root: PathBuf,
    files: Vec<PathBuf>,
    regex: Regex,
    limit: usize,
    cancel: Arc<AtomicBool>,
    wake: WakeFn,
) -> Receiver<GrepHit> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut sent = 0;
        for relative in files {
            if cancel.load(Ordering::Relaxed) || sent >= limit {
                break;
            }
            let Ok(bytes) = std::fs::read(root.join(&relative)) else { continue };
            if bytes.len() > 4 * 1024 * 1024 || bytes.iter().take(1024).any(|&b| b == 0) {
                continue;
            }
            let content = String::from_utf8_lossy(&bytes);
            let mut found = false;
            for (i, line) in content.lines().enumerate() {
                let Some(m) = regex.find(line) else { continue };
                let leading = line.len() - line.trim_start().len();
                let start = line[leading..m.start().max(leading)].chars().count();
                let end = start + line[m.start().max(leading)..m.end()].chars().count();
                let text: String = line.trim().chars().take(200).collect();
                let hit = GrepHit { path: relative.clone(), line: i, text, matched: (start, end) };
                if tx.send(hit).is_err() {
                    return;
                }
                found = true;
                sent += 1;
                if sent >= limit {
                    break;
                }
            }
            if found {
                wake();
            }
        }
        drop(tx);
        wake();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let re = glob_to_regex("*.log").unwrap();
        assert!(re.is_match("a.log"));
        assert!(re.is_match("dir/a.log"));
        assert!(!re.is_match("a.logs"));
        let re = glob_to_regex("build/").unwrap();
        assert!(re.is_match("build"));
        assert!(re.is_match("build/x"));
        let re = glob_to_regex("/docs/*.md").unwrap();
        assert!(re.is_match("docs/a.md"));
        assert!(!re.is_match("x/docs/a.md"));
    }
}
